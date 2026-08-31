use std::{collections::HashMap, env, process::Command};

use anyhow::Result;
use base64::Engine;
use uuid::Uuid;

use crate::config::profile::{
    Profile, SUPPORTED_SHARE_SCHEMES, Settings, Subscription, parse_share_link,
};

const KVN_TUI_USER_AGENT: &str = concat!("kvn-tui/", env!("CARGO_PKG_VERSION"));
const MAX_SUBSCRIPTION_BYTES: usize = 2 * 1024 * 1024;

fn ensure_subscription_size(bytes: usize, kind: &str) -> Result<()> {
    if bytes > MAX_SUBSCRIPTION_BYTES {
        anyhow::bail!(
            "Subscription {kind} exceeds the {} MiB limit",
            MAX_SUBSCRIPTION_BYTES / (1024 * 1024)
        );
    }
    Ok(())
}

/// Validate that a subscription URL uses encrypted transport.
pub fn validate_subscription_url(input: &str) -> Result<()> {
    let url = url::Url::parse(input).map_err(|_| anyhow::anyhow!("Invalid subscription URL"))?;
    match url.scheme() {
        "https" if url.host_str().is_some() => Ok(()),
        "https" => anyhow::bail!("Invalid subscription URL: missing host"),
        "http" => anyhow::bail!("Insecure HTTP subscriptions are blocked; use HTTPS"),
        scheme => anyhow::bail!("Unsupported subscription URL scheme: {scheme}"),
    }
}

/// Generate a stable installation identifier once: `lnx-` + UUID v4.
/// Never derived from the subscription URL — rotating a token or changing a
/// domain must not make the provider treat the same installation as a new
/// device.
pub(crate) fn generate_hwid() -> String {
    format!("lnx-{}", Uuid::new_v4().simple())
}

/// Platform facts read at request time (never persisted) so device headers
/// cannot go stale in `profiles.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceEnv {
    pub kernel_version: String,
    pub locale: String,
}

impl DeviceEnv {
    /// Read platform facts from the running system. Must never be called from
    /// `app::update` — platform detection stays outside the pure update
    /// boundary; this module is only invoked by the daemon runtime.
    pub fn detect() -> Self {
        Self {
            kernel_version: kernel_version(),
            locale: normalize_locale(&system_locale()),
        }
    }
}

/// Build the request headers for a subscription fetch. Every request carries
/// `User-Agent: kvn-tui/<version>`; when `sub.effective_hwid(settings)` yields
/// an HWID, the Linux device headers are added as well.
fn build_request_headers(
    sub: &Subscription,
    settings: &Settings,
    env: &DeviceEnv,
) -> HashMap<String, String> {
    let mut headers = HashMap::new();
    headers.insert("User-Agent".to_string(), KVN_TUI_USER_AGENT.to_string());
    if let Some(hwid) = sub.effective_hwid(settings) {
        headers.insert("X-Hwid".to_string(), hwid.to_string());
        headers.insert("X-Device-Os".to_string(), "Linux".to_string());
        headers.insert("X-Ver-Os".to_string(), env.kernel_version.clone());
        headers.insert("X-Device-Model".to_string(), "Desktop".to_string());
        headers.insert("X-Device-Locale".to_string(), env.locale.clone());
    }
    headers
}

/// Normalize a Linux locale identifier to a compact `language[-REGION]` form
/// for the `X-Device-Locale` header: `ru_RU.UTF-8` → `ru-RU`,
/// `de_DE.utf8` → `de-DE`, `sr_RS@latin` → `sr-RS`, `en` → `en`,
/// `C`/`POSIX`/empty → `en`.
pub fn normalize_locale(raw: &str) -> String {
    let base = raw.split('@').next().unwrap_or(raw);
    let base = base.split('.').next().unwrap_or(base).trim();
    if base.is_empty() || base.eq_ignore_ascii_case("c") || base.eq_ignore_ascii_case("posix") {
        return "en".to_string();
    }
    let mut parts = base.splitn(2, '_');
    let lang = parts.next().unwrap_or_default().trim().to_ascii_lowercase();
    if lang.is_empty() {
        return "en".to_string();
    }
    match parts.next() {
        Some(region) if !region.trim().is_empty() => {
            format!("{lang}-{}", region.trim().to_ascii_uppercase())
        }
        _ => lang,
    }
}

fn system_locale() -> String {
    env::var("LC_ALL")
        .or_else(|_| env::var("LC_MESSAGES"))
        .or_else(|_| env::var("LANG"))
        .unwrap_or_else(|_| "en_US.UTF-8".to_string())
}

fn kernel_version() -> String {
    Command::new("uname")
        .arg("-r")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Decode the Remnawave `Announce` response header value. The documented form
/// is `base64:<payload>`; anything undecodable is returned as-is so the
/// provider's text is never silently dropped.
fn decode_announce(raw: &str) -> String {
    let payload = raw.strip_prefix("base64:").unwrap_or(raw);
    base64::engine::general_purpose::STANDARD
        .decode(payload.trim())
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| raw.to_string())
}

/// Inspect the Remnawave HWID response headers *before* parsing the body and
/// produce the user-facing message when the server rejected the request.
///
/// Header semantics:
/// - `X-Hwid-Not-Supported: true` — HWID identification is required but
///   missing. When the client did not send HWID headers the message asks to
///   enable `send_hwid`; when it did, the server simply does not support HWID.
/// - `X-Hwid-Max-Devices-Reached: true` — the device limit for this HWID is
///   exhausted.
/// - `X-Hwid-Active: true` / `X-Hwid-Limit: true` — success/compatibility
///   markers, nothing to report.
/// - `Announce: base64:...` — decoded and appended to an HWID rejection, but
///   never turns an otherwise successful response into an error on its own.
pub fn hwid_response_error(headers: &HashMap<String, String>, hwid_sent: bool) -> Option<String> {
    let header_value = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    };
    let is_true = |name: &str| header_value(name).is_some_and(|v| v.eq_ignore_ascii_case("true"));
    let announce = header_value("Announce").map(decode_announce);
    let with_announce = |msg: String| match &announce {
        Some(text) => format!("{msg}\nProvider announcement: {text}"),
        None => msg,
    };

    if is_true("X-Hwid-Not-Supported") {
        let msg = if hwid_sent {
            "Subscription server does not support HWID identification; disable 'send HWID' for this subscription."
        } else {
            "Subscription requires HWID identification; enable 'send HWID' for this subscription and retry."
        };
        return Some(with_announce(msg.to_string()));
    }
    if is_true("X-Hwid-Max-Devices-Reached") {
        return Some(with_announce(
            "HWID device limit reached for this subscription; deactivate another device or contact the provider."
                .to_string(),
        ));
    }
    // `X-Hwid-Active` and the legacy `X-Hwid-Limit` compatibility flag mean
    // the request was accepted. An announcement alone is advisory.
    None
}

fn fetch_response(
    url: &str,
    headers: &HashMap<String, String>,
) -> Result<(String, HashMap<String, String>)> {
    let redacted_url = crate::redaction::redact_url_for_log(url);
    let agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .new_agent();
    let mut req = agent.get(url);

    if let Some(headers_map) = req.headers_mut() {
        for (name, value) in headers {
            if let (Ok(n), Ok(v)) = (
                ureq::http::HeaderName::from_bytes(name.as_bytes()),
                ureq::http::HeaderValue::from_str(value),
            ) {
                headers_map.insert(n, v);
            }
        }
    }

    // Do not retain ureq's error as a source: its Display output may contain
    // the complete request URL, including subscription credentials.
    let resp = req
        .call()
        .map_err(|_| anyhow::anyhow!("GET {redacted_url} failed"))?;

    let mut resp_headers = HashMap::new();
    for (name, value) in resp.headers() {
        let value = value.to_str().unwrap_or("");
        match resp_headers.entry(name.as_str().to_string()) {
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(value.to_string());
            }
            std::collections::hash_map::Entry::Occupied(mut e) => {
                e.insert(format!("{}, {}", e.get(), value));
            }
        }
    }

    if resp.status() != 200 {
        // Remnawave reports HWID problems on non-200 responses too.
        let hwid_sent = headers.contains_key("X-Hwid");
        if let Some(message) = hwid_response_error(&resp_headers, hwid_sent) {
            anyhow::bail!("{} (HTTP {} for {})", message, resp.status(), redacted_url);
        }
        anyhow::bail!("HTTP {} for {}", resp.status(), redacted_url);
    }

    // Read one byte beyond the application limit so an exact-limit body is
    // accepted while any larger response is rejected deterministically.
    let body = resp
        .into_body()
        .into_with_config()
        .limit((MAX_SUBSCRIPTION_BYTES + 1) as u64)
        .read_to_string()
        .map_err(|error| match error {
            ureq::Error::BodyExceedsLimit(_) => anyhow::anyhow!(
                "Subscription response exceeds the {} MiB limit",
                MAX_SUBSCRIPTION_BYTES / (1024 * 1024)
            ),
            error => anyhow::Error::new(error).context("Failed to read subscription body"),
        })?;
    ensure_subscription_size(body.len(), "response")?;
    Ok((body, resp_headers))
}

/// Fetch a subscription URL, decode its body (Base64 or plain text), and parse
/// every share link it contains. Mixed-protocol subscriptions are supported;
/// individual malformed lines are logged and skipped.
///
/// Every request identifies the client with `User-Agent: kvn-tui/<version>`;
/// when `send_hwid` is enabled, the HWID + Linux device headers are added.
/// Remnawave HWID response headers are inspected before the body is parsed so
/// rejection reasons surface directly instead of failing deep inside body
/// parsing.
pub fn fetch_subscription(sub: &Subscription, settings: &Settings) -> Result<Vec<Profile>> {
    validate_subscription_url(&sub.url)?;
    fetch_subscription_after_validation(sub, settings)
}

/// Perform the request after the caller has enforced the subscription URL
/// policy. Request-level tests call this directly to use a local HTTP fixture.
fn fetch_subscription_after_validation(
    sub: &Subscription,
    settings: &Settings,
) -> Result<Vec<Profile>> {
    let hwid_sent = sub.effective_hwid(settings).is_some();
    let env = DeviceEnv::detect();
    let headers = build_request_headers(sub, settings, &env);
    let (body, resp_headers) = fetch_response(&sub.url, &headers)?;

    if let Some(message) = hwid_response_error(&resp_headers, hwid_sent) {
        anyhow::bail!("{message}");
    }

    parse_subscription_body(&body)
}

/// True when `line` (already trimmed) starts with any supported share-link scheme.
fn line_has_supported_scheme(line: &str) -> bool {
    SUPPORTED_SHARE_SCHEMES
        .iter()
        .any(|prefix| line.starts_with(prefix))
}

fn try_parse_singbox_json(body: &str) -> Option<Vec<Profile>> {
    use serde_json::Value;
    let v: Value = serde_json::from_str(body).ok()?;
    let items: Vec<serde_json::Value> = match v {
        serde_json::Value::Array(arr) => arr,
        serde_json::Value::Object(map) => vec![serde_json::Value::Object(map)],
        _ => return None,
    };

    let mut profiles = Vec::new();
    for item in items {
        let outbounds = item.get("outbounds").and_then(|x| x.as_array());
        let objs = match outbounds {
            Some(a) => a.iter().collect::<Vec<_>>(),
            None => continue,
        };

        for ob in objs {
            let proto = ob.get("protocol").and_then(|x| x.as_str()).unwrap_or("");
            if proto != "vless" {
                continue;
            }

            // settings.vnext[0]: address, port
            let vnext = ob
                .get("settings")
                .and_then(|s| s.get("vnext"))
                .and_then(|v| v.as_array())
                .and_then(|a| a.first());
            let vnext = match vnext {
                Some(x) => x,
                None => continue,
            };

            let addr = vnext.get("address").and_then(|x| x.as_str())?.to_string();
            let port = vnext.get("port").and_then(|x| x.as_u64())? as u16;

            // settings.vnext[0].users[0]: id, flow, encryption
            let user = vnext
                .get("users")
                .and_then(|u| u.as_array())
                .and_then(|u| u.first());
            let user = match user {
                Some(x) => x,
                None => continue,
            };
            let uuid = user.get("id").and_then(|x| x.as_str())?.to_string();
            let flow = user
                .get("flow")
                .and_then(|x| x.as_str())
                .map(str::to_string);
            let encryption = user
                .get("encryption")
                .and_then(|x| x.as_str())
                .unwrap_or("none")
                .to_string();

            // streamSettings: network, security, + transport / tls / reality
            let ss = ob.get("streamSettings").cloned().unwrap_or(Value::Null);
            let network = ss
                .get("network")
                .and_then(|x| x.as_str())
                .unwrap_or("tcp")
                .to_string();
            let security = ss
                .get("security")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();

            // собираем query-параметры в порядке, который ждёт парсер share-link
            let mut q = vec![
                ("type", network.as_str()),
                ("encryption", encryption.as_str()),
            ];
            let mut fp = None;
            let mut pbk = None;
            let mut sid = None;
            let mut sni = None;
            if security == "reality" {
                if let Some(rs) = ss.get("realitySettings") {
                    sni = rs
                        .get("serverName")
                        .and_then(|x| x.as_str())
                        .map(str::to_string);
                    pbk = rs
                        .get("publicKey")
                        .and_then(|x| x.as_str())
                        .map(str::to_string);
                    sid = rs
                        .get("shortId")
                        .and_then(|x| x.as_str())
                        .map(str::to_string);
                    fp = rs
                        .get("fingerprint")
                        .and_then(|x| x.as_str())
                        .map(str::to_string);
                }
                q.push(("security", "reality"));
            } else if security == "tls" {
                q.push(("security", "tls"));
                if let Some(ts) = ss.get("tlsSettings") {
                    sni = ts
                        .get("serverName")
                        .and_then(|x| x.as_str())
                        .map(str::to_string);
                    fp = ts
                        .get("fingerprint")
                        .and_then(|x| x.as_str())
                        .map(str::to_string);
                }
            }
            if let Some(ref s) = sni {
                q.push(("sni", s.as_str()));
            }
            if let Some(ref p) = pbk {
                q.push(("pbk", p.as_str()));
            }
            if let Some(ref s) = sid {
                q.push(("sid", s.as_str()));
            }
            if let Some(ref f) = fp {
                q.push(("fp", f.as_str()));
            }
            if let Some(ref f) = flow {
                q.push(("flow", f.as_str()));
            }

            let transport = ss.get("wsSettings");

            // транспорт (для ws / grpc / http)
            match network.as_str() {
                "ws" => {
                    if let Some(ws) = transport {
                        if let Some(path) = ws.get("path").and_then(|x| x.as_str()) {
                            q.push(("path", path));
                        }
                        if let Some(host) = ws
                            .get("headers")
                            .and_then(|h| h.get("Host"))
                            .and_then(|x| x.as_str())
                        {
                            q.push(("host", host));
                        }
                    }
                }
                "grpc" => {
                    if let Some(g) = ss.get("grpcSettings")
                        && let Some(sn) = g.get("serviceName")
                        && let Some(sn) = sn.as_str()
                    {
                        q.push(("serviceName", sn));
                    }
                }
                _ => {}
            }

            let query = q
                .iter()
                .map(|(k, v)| format!("{}={}", urlencode(k), urlencode(v)))
                .collect::<Vec<_>>()
                .join("&");

            // имя — поле `remarks` если есть, иначе адрес
            let name = item
                .get("remarks")
                .and_then(|x| x.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| addr.clone());

            let uri = format!(
                "vless://{uuid}@{addr}:{port}?{query}#{name}",
                uuid = uuid,
                addr = addr,
                port = port,
                query = query,
                name = urlencode(&name),
            );

            match parse_share_link(&uri) {
                Ok(p) => profiles.push(p),
                Err(e) => tracing::warn!("subscription: singbox-json: bad vless URI: {e}"),
            }
        }
    }
    if profiles.is_empty() {
        None
    } else {
        Some(profiles)
    }
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            _ => out.push_str(&format!("%{:02X}", byte)),
        }
    }
    out
}

/// Parse a subscription body that is either Base64-encoded or plain text.
/// Each non-empty line is interpreted as a share link in any supported scheme.
pub fn parse_subscription_body(body: &str) -> Result<Vec<Profile>> {
    ensure_subscription_size(body.len(), "body")?;
    let trimmed = body.trim();
    if trimmed.is_empty() {
        anyhow::bail!("Subscription body is empty");
    }

    if let Some(profiles) = try_parse_singbox_json(trimmed) {
        return Ok(profiles);
    }

    let decoded = try_decode_base64(trimmed)?;
    let text = decoded.as_deref().unwrap_or(trimmed);

    let mut profiles = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match parse_share_link(line) {
            Ok(profile) => profiles.push(profile),
            Err(e) => tracing::warn!("subscription: skipped malformed line: {e}"),
        }
    }

    if profiles.is_empty() {
        anyhow::bail!("No supported share links found in subscription");
    }

    Ok(profiles)
}

/// Attempt to Base64-decode `text`. Returns `Some(decoded)` only when decoding
/// succeeds and the result looks like a subscription (contains at least one
/// supported scheme prefix). This prevents treating plain text as binary garbage.
fn try_decode_base64(text: &str) -> Result<Option<String>> {
    let Some(decoded_bytes) = base64::engine::general_purpose::STANDARD.decode(text).ok() else {
        return Ok(None);
    };
    ensure_subscription_size(decoded_bytes.len(), "decoded body")?;
    let Some(decoded) = String::from_utf8(decoded_bytes).ok() else {
        return Ok(None);
    };
    if decoded.lines().any(|line| {
        let l = line.trim();
        line_has_supported_scheme(l)
    }) {
        Ok(Some(decoded))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_vless() -> &'static str {
        "vless://671c62c7-6768-4b98-ac6b-572c9c707be0@203.0.113.42:443?security=tls#Sub-1"
    }

    #[test]
    fn parse_plain_body_with_two_links() {
        let body = format!("{}\n{}\n", sample_vless(), sample_vless());
        let profiles = parse_subscription_body(&body).unwrap();
        assert_eq!(profiles.len(), 2);
        assert_eq!(profiles[0].address, "203.0.113.42");
        assert_eq!(profiles[0].name, "Sub-1");
    }

    #[test]
    fn parse_base64_body() {
        let plain = format!("{}\n{}\n", sample_vless(), sample_vless());
        let encoded = base64::engine::general_purpose::STANDARD.encode(&plain);
        let profiles = parse_subscription_body(&encoded).unwrap();
        assert_eq!(profiles.len(), 2);
    }

    #[test]
    fn parse_body_with_invalid_lines_is_tolerant() {
        let body = format!("not-a-link\n{}\n\nalso-not-a-link\n", sample_vless());
        let profiles = parse_subscription_body(&body).unwrap();
        assert_eq!(profiles.len(), 1);
    }

    #[test]
    fn parse_empty_body_fails() {
        assert!(parse_subscription_body("   ").is_err());
    }

    #[test]
    fn subscription_size_limit_accepts_boundary_and_rejects_larger_values() {
        assert!(ensure_subscription_size(MAX_SUBSCRIPTION_BYTES - 1, "body").is_ok());
        assert!(ensure_subscription_size(MAX_SUBSCRIPTION_BYTES, "body").is_ok());
        let error = ensure_subscription_size(MAX_SUBSCRIPTION_BYTES + 1, "body").unwrap_err();
        assert!(error.to_string().contains("2 MiB limit"));
    }

    #[test]
    fn oversized_subscription_body_fails_before_parsing() {
        let body = "x".repeat(MAX_SUBSCRIPTION_BYTES + 1);
        let error = parse_subscription_body(&body).unwrap_err();
        assert!(error.to_string().contains("Subscription body exceeds"));
    }

    #[test]
    fn parse_body_without_vless_links_fails() {
        assert!(parse_subscription_body("just some text").is_err());
    }

    #[test]
    fn parse_mixed_protocol_subscription() {
        let body = "vless://uuid@1.1.1.1:443#V\n\
                    trojan://pw@2.2.2.2:443#T\n\
                    ss://YWVzLTI1Ni1nY206cHc@3.3.3.3:8388#S\n\
                    hysteria2://hp@4.4.4.4:443#H2\n";
        let profiles = parse_subscription_body(body).unwrap();
        assert_eq!(profiles.len(), 4);
        let names: Vec<_> = profiles.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["V", "T", "S", "H2"]);
    }

    #[test]
    fn parse_base64_body_with_multiple_schemes() {
        let body = "vless://uuid@1.1.1.1:443#V\ntrojan://pw@2.2.2.2:443#T\n";
        let encoded = base64::engine::general_purpose::STANDARD.encode(body);
        let profiles = parse_subscription_body(&encoded).unwrap();
        assert_eq!(profiles.len(), 2);
    }

    #[test]
    fn generate_hwid_format() {
        let hwid = generate_hwid();
        assert!(hwid.starts_with("lnx-"));
        let uuid_part = &hwid["lnx-".len()..];
        assert_eq!(uuid_part.len(), 32);
        assert!(uuid_part.chars().all(|c| c.is_ascii_hexdigit()));
        // UUID v4: two independent generations must never collide.
        assert_ne!(generate_hwid(), generate_hwid());
    }

    #[test]
    fn normalize_locale_covers_common_linux_formats() {
        assert_eq!(normalize_locale("ru_RU.UTF-8"), "ru-RU");
        assert_eq!(normalize_locale("de_DE.utf8"), "de-DE");
        assert_eq!(normalize_locale("zh_CN.UTF-8"), "zh-CN");
        assert_eq!(normalize_locale("sr_RS@latin"), "sr-RS");
        assert_eq!(normalize_locale("ru_RU"), "ru-RU");
        assert_eq!(normalize_locale("en"), "en");
        assert_eq!(normalize_locale("C"), "en");
        assert_eq!(normalize_locale("POSIX"), "en");
        assert_eq!(normalize_locale(""), "en");
        assert_eq!(normalize_locale("C.UTF-8"), "en");
    }

    fn settings_with_hwid() -> Settings {
        Settings {
            hwid: "lnx-installation-hwid".to_string(),
            ..Default::default()
        }
    }

    fn sub_with(url: String, send_hwid: bool, hwid: Option<&str>) -> Subscription {
        Subscription {
            id: Uuid::new_v4(),
            name: "Sub".into(),
            url,
            auto_update: crate::config::profile::SubscriptionAutoUpdate::Off,
            last_updated: None,
            next_auto_update: None,
            retry_state: None,
            send_hwid,
            hwid: hwid.map(str::to_string),
        }
    }

    #[test]
    fn build_request_headers_always_set_user_agent() {
        let env = DeviceEnv {
            kernel_version: "6.12.0-arch1-1".into(),
            locale: "ru-RU".into(),
        };
        for send_hwid in [false, true] {
            let sub = sub_with("https://example.com/sub".into(), send_hwid, None);
            let headers = build_request_headers(&sub, &settings_with_hwid(), &env);
            assert_eq!(
                headers.get("User-Agent").map(String::as_str),
                Some(KVN_TUI_USER_AGENT),
            );
            assert!(KVN_TUI_USER_AGENT.starts_with("kvn-tui/"));
        }
    }

    #[test]
    fn subscription_url_policy_accepts_https() {
        assert!(validate_subscription_url("https://example.com/sub").is_ok());
    }

    #[test]
    fn subscription_url_policy_rejects_http() {
        let error = validate_subscription_url("http://example.com/sub").unwrap_err();
        assert!(error.to_string().contains("HTTP subscriptions are blocked"));
    }

    #[test]
    fn fetch_rejects_http_before_request() {
        let sub = sub_with("http://example.invalid/secret-token".into(), false, None);
        let error = fetch_subscription(&sub, &settings_with_hwid()).unwrap_err();
        assert!(error.to_string().contains("HTTP subscriptions are blocked"));
    }

    #[test]
    fn subscription_url_policy_rejects_unsupported_and_invalid_urls() {
        assert!(validate_subscription_url("ftp://example.com/sub").is_err());
        assert!(validate_subscription_url("not a URL").is_err());
    }

    #[test]
    fn build_request_headers_send_hwid_false_sends_no_device_headers() {
        let env = DeviceEnv {
            kernel_version: "6.12.0-arch1-1".into(),
            locale: "ru-RU".into(),
        };
        // Even a per-subscription override must not be sent when send_hwid is false.
        let sub = sub_with(
            "https://example.com/sub".into(),
            false,
            Some("provider-registered-device-id"),
        );
        let headers = build_request_headers(&sub, &settings_with_hwid(), &env);
        assert!(!headers.contains_key("X-Hwid"));
        assert!(!headers.contains_key("X-Device-Os"));
        assert!(!headers.contains_key("X-Ver-Os"));
        assert!(!headers.contains_key("X-Device-Model"));
        assert!(!headers.contains_key("X-Device-Locale"));
    }

    #[test]
    fn build_request_headers_send_hwid_true_sends_all_linux_device_headers() {
        let env = DeviceEnv {
            kernel_version: "6.12.0-arch1-1".into(),
            locale: "ru-RU".into(),
        };
        let sub = sub_with("https://example.com/sub".into(), true, None);
        let headers = build_request_headers(&sub, &settings_with_hwid(), &env);
        assert_eq!(
            headers.get("X-Hwid").map(String::as_str),
            Some("lnx-installation-hwid")
        );
        assert_eq!(
            headers.get("X-Device-Os").map(String::as_str),
            Some("Linux")
        );
        assert_eq!(
            headers.get("X-Ver-Os").map(String::as_str),
            Some("6.12.0-arch1-1")
        );
        assert_eq!(
            headers.get("X-Device-Model").map(String::as_str),
            Some("Desktop")
        );
        assert_eq!(
            headers.get("X-Device-Locale").map(String::as_str),
            Some("ru-RU")
        );
    }

    #[test]
    fn build_request_headers_subscription_hwid_overrides_settings() {
        let env = DeviceEnv {
            kernel_version: "6.12.0-arch1-1".into(),
            locale: "en".into(),
        };
        let sub = sub_with(
            "https://example.com/sub".into(),
            true,
            Some("provider-registered-device-id"),
        );
        let headers = build_request_headers(&sub, &settings_with_hwid(), &env);
        assert_eq!(
            headers.get("X-Hwid").map(String::as_str),
            Some("provider-registered-device-id"),
        );
    }

    #[test]
    fn hwid_response_error_not_supported_without_hwid_sent_asks_to_enable() {
        let headers = HashMap::from([("x-hwid-not-supported".to_string(), "true".to_string())]);
        let msg = hwid_response_error(&headers, false).unwrap();
        assert!(msg.contains("enable 'send HWID'"), "got: {msg}");
    }

    #[test]
    fn hwid_response_error_not_supported_with_hwid_sent_reports_unsupported() {
        let headers = HashMap::from([
            ("X-Hwid-Not-Supported".to_string(), "true".to_string()),
            ("X-Hwid-Active".to_string(), "false".to_string()),
        ]);
        let msg = hwid_response_error(&headers, true).unwrap();
        assert!(msg.contains("does not support HWID"), "got: {msg}");
    }

    #[test]
    fn hwid_response_error_max_devices_reached_reports_limit() {
        let headers =
            HashMap::from([("X-Hwid-Max-Devices-Reached".to_string(), "true".to_string())]);
        let msg = hwid_response_error(&headers, true).unwrap();
        assert!(msg.contains("device limit reached"), "got: {msg}");
    }

    #[test]
    fn hwid_response_error_limit_header_is_compatibility_success() {
        let headers = HashMap::from([("X-Hwid-Limit".to_string(), "true".to_string())]);
        assert!(hwid_response_error(&headers, true).is_none());
    }

    #[test]
    fn hwid_response_error_active_header_alone_is_success() {
        let headers = HashMap::from([
            ("X-Hwid-Active".to_string(), "true".to_string()),
            ("X-Hwid-Limit".to_string(), "3".to_string()),
        ]);
        assert!(hwid_response_error(&headers, true).is_none());
    }

    #[test]
    fn hwid_response_error_announce_is_decoded_and_appended() {
        let announcement = base64::engine::general_purpose::STANDARD
            .encode("Server maintenance tonight")
            .to_string();
        let headers = HashMap::from([
            ("X-Hwid-Max-Devices-Reached".to_string(), "true".to_string()),
            ("Announce".to_string(), format!("base64:{announcement}")),
        ]);
        let msg = hwid_response_error(&headers, true).unwrap();
        assert!(msg.contains("device limit reached"), "got: {msg}");
        assert!(
            msg.contains("Provider announcement: Server maintenance tonight"),
            "got: {msg}"
        );
    }

    #[test]
    fn hwid_response_error_announce_only_is_not_an_error() {
        let announcement = base64::engine::general_purpose::STANDARD
            .encode("Welcome!")
            .to_string();
        let headers = HashMap::from([("Announce".to_string(), format!("base64:{announcement}"))]);
        assert!(hwid_response_error(&headers, false).is_none());
    }

    #[test]
    fn hwid_response_error_announce_undecodable_falls_back_to_raw() {
        let headers = HashMap::from([
            ("X-Hwid-Not-Supported".to_string(), "true".to_string()),
            ("Announce".to_string(), "not base64!!!".to_string()),
        ]);
        let msg = hwid_response_error(&headers, false).unwrap();
        assert!(msg.contains("not base64!!!"), "got: {msg}");
    }

    fn singbox_json_body() -> String {
        r#"{
            "remarks": "JSON node",
            "outbounds": [{
                "protocol": "vless",
                "settings": {
                    "vnext": [{
                        "address": "198.51.100.7",
                        "port": 8443,
                        "users": [{
                            "id": "671c62c7-6768-4b98-ac6b-572c9c707be0",
                            "flow": "xtls-rprx-vision",
                            "encryption": "none"
                        }]
                    }]
                },
                "streamSettings": {
                    "network": "ws",
                    "security": "tls",
                    "tlsSettings": {"serverName": "example.com", "fingerprint": "chrome"},
                    "wsSettings": {"path": "/wspath", "headers": {"Host": "ws.example.com"}}
                }
            }]
        }"#
        .to_string()
    }

    #[test]
    fn parse_singbox_json_object() {
        let profiles = parse_subscription_body(&singbox_json_body()).unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, "JSON node");
        assert_eq!(profiles[0].address, "198.51.100.7");
        assert_eq!(profiles[0].port, 8443);
    }

    #[test]
    fn parse_singbox_json_array() {
        let single = singbox_json_body();
        let body = format!("[{}, {}]", single, single);
        let profiles = parse_subscription_body(&body).unwrap();
        assert_eq!(profiles.len(), 2);
    }

    #[test]
    fn parse_singbox_json_malformed_falls_back_to_text_and_fails() {
        let body = "{not valid json, and not share links either}";
        assert!(parse_subscription_body(body).is_err());
    }

    #[test]
    fn parse_singbox_json_without_vless_outbounds_fails() {
        let body = r#"{"outbounds": [{"protocol": "shadowsocks"}]}"#;
        assert!(parse_subscription_body(body).is_err());
    }

    #[test]
    fn parse_singbox_json_malformed_outbound_fields_are_skipped() {
        // vless outbound missing `settings.vnext` — must not panic; the body
        // falls through to text parsing and fails cleanly.
        let body = r#"{"outbounds": [{"protocol": "vless"}]}"#;
        assert!(parse_subscription_body(body).is_err());
    }

    /// Minimal one-shot HTTP server for request tests: captures the request
    /// headers and replies with a fixed status line, extra headers and body.
    /// Keeps request tests independent of any external subscription provider.
    fn spawn_http_server(
        status_line: &str,
        extra_headers: &[(&str, String)],
        body: &str,
    ) -> (String, std::sync::mpsc::Receiver<HashMap<String, String>>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let status_line = status_line.to_string();
        let extra_headers: Vec<(String, String)> = extra_headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        let body = body.to_string();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                }
            }
            let text = String::from_utf8_lossy(&buf);
            let mut req_headers = HashMap::new();
            for line in text.lines().skip(1) {
                if line.is_empty() {
                    break;
                }
                if let Some((k, v)) = line.split_once(':') {
                    req_headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
                }
            }
            let _ = tx.send(req_headers);

            let mut resp = format!(
                "{status_line}\r\nContent-Length: {}\r\nConnection: close\r\n",
                body.len()
            );
            for (k, v) in &extra_headers {
                resp.push_str(&format!("{k}: {v}\r\n"));
            }
            resp.push_str("\r\n");
            resp.push_str(&body);
            let _ = stream.write_all(resp.as_bytes());
        });
        (format!("http://{addr}/sub"), rx)
    }

    fn captured_request(
        rx: std::sync::mpsc::Receiver<HashMap<String, String>>,
    ) -> HashMap<String, String> {
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap()
    }

    #[test]
    fn fetch_always_sends_kvntui_user_agent() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let (url, rx) = spawn_http_server("HTTP/1.1 200 OK", &[], sample_vless());
        let sub = sub_with(url, false, None);
        fetch_subscription_after_validation(&sub, &settings_with_hwid()).unwrap();
        let req = captured_request(rx);
        assert_eq!(
            req.get("user-agent").map(String::as_str),
            Some(KVN_TUI_USER_AGENT),
        );
    }

    #[test]
    fn fetch_send_hwid_false_does_not_send_device_headers() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let (url, rx) = spawn_http_server("HTTP/1.1 200 OK", &[], sample_vless());
        let sub = sub_with(url, false, Some("provider-registered-device-id"));
        fetch_subscription_after_validation(&sub, &settings_with_hwid()).unwrap();
        let req = captured_request(rx);
        assert!(!req.contains_key("x-hwid"));
        assert!(!req.contains_key("x-device-os"));
        assert!(!req.contains_key("x-device-locale"));
    }

    #[test]
    fn fetch_send_hwid_true_uses_settings_hwid_and_device_headers() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let (url, rx) = spawn_http_server("HTTP/1.1 200 OK", &[], sample_vless());
        let sub = sub_with(url, true, None);
        fetch_subscription_after_validation(&sub, &settings_with_hwid()).unwrap();
        let req = captured_request(rx);
        assert_eq!(
            req.get("x-hwid").map(String::as_str),
            Some("lnx-installation-hwid")
        );
        assert_eq!(req.get("x-device-os").map(String::as_str), Some("Linux"));
        assert_eq!(
            req.get("x-device-model").map(String::as_str),
            Some("Desktop")
        );
        assert!(req.get("x-ver-os").is_some_and(|v| !v.is_empty()));
        assert!(req.get("x-device-locale").is_some_and(|v| !v.is_empty()));
    }

    #[test]
    fn fetch_subscription_hwid_overrides_settings_hwid() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let (url, rx) = spawn_http_server("HTTP/1.1 200 OK", &[], sample_vless());
        let sub = sub_with(url, true, Some("provider-registered-device-id"));
        fetch_subscription_after_validation(&sub, &settings_with_hwid()).unwrap();
        let req = captured_request(rx);
        assert_eq!(
            req.get("x-hwid").map(String::as_str),
            Some("provider-registered-device-id"),
        );
    }

    #[test]
    fn fetch_one_subscriptions_override_is_never_sent_to_another() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let settings = settings_with_hwid();

        // Subscription A carries a provider-registered HWID override...
        let (url_a, rx_a) = spawn_http_server("HTTP/1.1 200 OK", &[], sample_vless());
        let sub_a = sub_with(url_a, true, Some("provider-registered-device-id"));
        fetch_subscription_after_validation(&sub_a, &settings).unwrap();
        assert_eq!(
            captured_request(rx_a).get("x-hwid").map(String::as_str),
            Some("provider-registered-device-id"),
        );

        // ...and subscription B (no override) falls back to settings.hwid,
        // never to A's override.
        let (url_b, rx_b) = spawn_http_server("HTTP/1.1 200 OK", &[], sample_vless());
        let sub_b = sub_with(url_b, true, None);
        fetch_subscription_after_validation(&sub_b, &settings).unwrap();
        assert_eq!(
            captured_request(rx_b).get("x-hwid").map(String::as_str),
            Some("lnx-installation-hwid"),
        );

        // ...and subscription C (send_hwid: false) sends no HWID at all.
        let (url_c, rx_c) = spawn_http_server("HTTP/1.1 200 OK", &[], sample_vless());
        let sub_c = sub_with(url_c, false, None);
        fetch_subscription_after_validation(&sub_c, &settings).unwrap();
        assert!(!captured_request(rx_c).contains_key("x-hwid"));
    }

    #[test]
    fn fetch_hwid_not_supported_response_reports_enable_send_hwid() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let (url, _rx) = spawn_http_server(
            "HTTP/1.1 200 OK",
            &[("X-Hwid-Not-Supported", "true".to_string())],
            sample_vless(),
        );
        let sub = sub_with(url, false, None);
        let err = fetch_subscription_after_validation(&sub, &settings_with_hwid()).unwrap_err();
        assert!(err.to_string().contains("enable 'send HWID'"), "got: {err}");
    }

    #[test]
    fn fetch_hwid_max_devices_reached_response_reports_limit() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let (url, _rx) = spawn_http_server(
            "HTTP/1.1 200 OK",
            &[("X-Hwid-Max-Devices-Reached", "true".to_string())],
            sample_vless(),
        );
        let sub = sub_with(url, true, None);
        let err = fetch_subscription_after_validation(&sub, &settings_with_hwid()).unwrap_err();
        assert!(
            err.to_string().contains("device limit reached"),
            "got: {err}"
        );
    }

    #[test]
    fn fetch_hwid_rejection_on_http_403_is_reported() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let (url, _rx) = spawn_http_server(
            "HTTP/1.1 403 Forbidden",
            &[("X-Hwid-Not-Supported", "true".to_string())],
            "forbidden",
        );
        let sub = sub_with(url, false, None);
        let err = fetch_subscription_after_validation(&sub, &settings_with_hwid()).unwrap_err();
        assert!(err.to_string().contains("enable 'send HWID'"), "got: {err}");
        assert!(err.to_string().contains("HTTP 403"), "got: {err}");
    }
}
