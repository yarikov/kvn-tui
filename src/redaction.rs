/// Return a diagnostic representation of a URL or share link without
/// credentials, path tokens, query parameters, or fragments.
pub fn redact_url_for_log(input: &str) -> String {
    let Ok(url) = url::Url::parse(input) else {
        return "<redacted URL>".to_string();
    };

    let scheme = url.scheme();
    if !matches!(scheme, "http" | "https") && url.username().is_empty() {
        return format!("{scheme}://<redacted>");
    }
    let Some(host) = url.host() else {
        return format!("{scheme}://<redacted>");
    };
    let host = match host {
        url::Host::Ipv6(address) => format!("[{address}]"),
        _ => host.to_string(),
    };
    let port = url
        .port()
        .map_or_else(String::new, |port| format!(":{port}"));

    match scheme {
        "http" | "https" => format!("{scheme}://{host}{port}/<redacted>"),
        _ => format!("{scheme}://<redacted>@{host}{port}"),
    }
}

#[cfg(test)]
mod tests {
    use super::redact_url_for_log;

    #[test]
    fn redacts_subscription_credentials_and_tokens() {
        let inputs = [
            "https://user:password@example.com/sub/secret-token?auth=query-secret#fragment",
            "http://example.com/another-secret",
        ];

        for input in inputs {
            let redacted = redact_url_for_log(input);
            for secret in [
                "user",
                "password",
                "secret-token",
                "query-secret",
                "fragment",
                "another-secret",
            ] {
                assert!(!redacted.contains(secret), "{secret} leaked in {redacted}");
            }
        }
    }

    #[test]
    fn redacts_supported_share_link_credentials() {
        let inputs = [
            "vless://secret-uuid@vpn.example:443?pbk=secret-key#name",
            "trojan://secret-password@vpn.example:443#name",
            "socks://user:secret-password@vpn.example:1080",
            "ssh://user:secret-password@vpn.example:22",
            "vmess://opaque-base64-secret",
        ];

        for input in inputs {
            let redacted = redact_url_for_log(input);
            for secret in [
                "secret-uuid",
                "secret-key",
                "secret-password",
                "opaque-base64-secret",
            ] {
                assert!(!redacted.contains(secret), "{secret} leaked in {redacted}");
            }
        }
    }

    #[test]
    fn preserves_only_safe_endpoint_context() {
        assert_eq!(
            redact_url_for_log("https://example.com:8443/sub/token"),
            "https://example.com:8443/<redacted>"
        );
        assert_eq!(
            redact_url_for_log("vless://uuid@[2001:db8::1]:443?security=reality"),
            "vless://<redacted>@[2001:db8::1]:443"
        );
        assert_eq!(redact_url_for_log("not a URL"), "<redacted URL>");
    }
}
