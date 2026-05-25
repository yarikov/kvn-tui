use std::process::Command;

use anyhow::{Context, Result};
use url::Url;

use crate::app::App;
use crate::config::profile::{Profile, Protocol, RealitySettings};

/// Read clipboard contents and attempt to parse a VPN share link.
/// Falls back to manual URI input if the clipboard is unavailable.
pub fn paste_profile(app: &mut App) -> Result<()> {
    let text = read_clipboard_text()?;
    add_profile_from_text(app, &text)
}

/// Read text from the Wayland clipboard via `wl-paste`.
fn read_clipboard_text() -> Result<String> {
    let text = read_clipboard_command("wl-paste", &[])?;
    if !text.is_empty() {
        Ok(text)
    } else {
        anyhow::bail!("Clipboard is empty or unavailable")
    }
}

/// Read clipboard via an external command.
fn read_clipboard_command(cmd: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(cmd)
        .args(args)
        .output()
        .with_context(|| format!("Failed to execute {}", cmd))?;

    if !output.status.success() {
        anyhow::bail!("{} exited with non-zero status", cmd);
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Parse a share link text and add the resulting profile to the app.
pub fn add_profile_from_text(app: &mut App, text: &str) -> Result<()> {
    let profile = parse_share_link(text)?;
    app.add_profile(profile.clone());
    app.set_status(format!("Pasted profile: {}", profile.name));
    Ok(())
}

/// Parse a VLESS share link into a Profile.
fn parse_share_link(text: &str) -> Result<Profile> {
    let trimmed = text.trim();

    if let Some(rest) = trimmed.strip_prefix("vless://") {
        parse_vless(rest)
    } else {
        anyhow::bail!("Unsupported share link format: only vless:// is supported")
    }
}

/// Parse a VLESS URI fragment.
fn parse_vless(rest: &str) -> Result<Profile> {
    let url = Url::parse(&format!("vless://{}", rest)).context("Invalid VLESS URL")?;

    let uuid = url.username().to_string();
    let host = url
        .host_str()
        .context("Missing host in VLESS URL")?
        .to_string();
    let port = url.port().unwrap_or(443);

    let mut profile = Profile::new(host.clone(), Protocol::Vless, host, port, uuid);

    // Extract fragment as profile name
    if let Some(fragment) = url.fragment() {
        profile.name = urlencoding::decode(fragment)?.to_string();
    }

    let query: std::collections::HashMap<String, String> = url
        .query_pairs()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

    if let Some(flow) = query.get("flow") {
        profile.flow = Some(flow.clone());
    }
    if let Some(security) = query.get("security") {
        profile.security = Some(security.clone());
    }
    if let Some(fp) = query.get("fp") {
        profile.fingerprint = Some(fp.clone());
    }
    if let Some(transport) = query.get("type") {
        profile.transport_type = Some(transport.clone());
    }
    if let Some(service_name) = query.get("serviceName") {
        profile.transport_service_name = Some(service_name.clone());
    }
    if let Some(pbk) = query.get("pbk") {
        let reality = RealitySettings {
            public_key: pbk.clone(),
            short_id: query.get("sid").cloned().unwrap_or_default(),
            server_name: query.get("sni").cloned().unwrap_or_default(),
            spider_x: query.get("spx").cloned().unwrap_or_default(),
        };
        profile.reality = Some(reality);
    }

    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::profile::Config;

    #[test]
    fn parse_long_vless_uri() {
        let uri = r#"vless://671c62c7-6768-4b98-ac6b-572c9c707be0@203.0.113.42:59431?type=grpc&encryption=none&serviceName=&authority=&security=reality&pbk=0IO3LodsrMnhOWh4ogwgdVqYg30CS5-snhFMwldOuAQ&fp=chrome&sni=google.com&sid=f04debc34cbc48a4&spx=%2F#Example-2873vb06"#;
        let profile = parse_share_link(uri).unwrap();
        assert_eq!(profile.protocol, Protocol::Vless);
        assert_eq!(profile.address, "203.0.113.42");
        assert_eq!(profile.port, 59431);
        assert_eq!(profile.uuid, "671c62c7-6768-4b98-ac6b-572c9c707be0");
        assert_eq!(profile.name, "Example-2873vb06");
        assert!(profile.security.is_some());
        let reality = profile.reality.unwrap();
        assert_eq!(
            reality.public_key,
            "0IO3LodsrMnhOWh4ogwgdVqYg30CS5-snhFMwldOuAQ"
        );
        assert_eq!(reality.server_name, "google.com");
        assert_eq!(reality.short_id, "f04debc34cbc48a4");
        assert_eq!(reality.spider_x, "/");
    }

    #[test]
    fn parse_vless_minimal() {
        let uri = "vless://uuid@1.2.3.4:443#Name";
        let profile = parse_share_link(uri).unwrap();
        assert_eq!(profile.uuid, "uuid");
        assert_eq!(profile.address, "1.2.3.4");
        assert_eq!(profile.port, 443);
        assert_eq!(profile.name, "Name");
        assert!(profile.reality.is_none());
        assert!(profile.flow.is_none());
        assert!(profile.fingerprint.is_none());
        assert!(profile.transport_type.is_none());
    }

    #[test]
    fn parse_vless_default_port() {
        let uri = "vless://uuid@example.com#Test";
        let profile = parse_share_link(uri).unwrap();
        assert_eq!(profile.port, 443);
        assert_eq!(profile.address, "example.com");
    }

    #[test]
    fn parse_vless_partial_reality() {
        let uri = "vless://uuid@1.2.3.4:8443?security=reality&pbk=pk123&sni=sni.test#Partial";
        let profile = parse_share_link(uri).unwrap();
        assert_eq!(profile.security, Some("reality".to_string()));
        let reality = profile.reality.unwrap();
        assert_eq!(reality.public_key, "pk123");
        assert_eq!(reality.server_name, "sni.test");
        assert!(reality.short_id.is_empty());
        assert!(reality.spider_x.is_empty());
    }

    #[test]
    fn parse_vless_url_encoded_spx() {
        let uri = "vless://uuid@1.2.3.4?pbk=k&spx=%2Fpath%2Fhere#N";
        let profile = parse_share_link(uri).unwrap();
        assert_eq!(profile.reality.as_ref().unwrap().spider_x, "/path/here");
    }

    #[test]
    fn parse_unsupported_format_fails() {
        let result = parse_share_link("ss://encrypted");
        assert!(result.is_err());
    }

    #[test]
    fn parse_vless_missing_host_fails() {
        // vless:// without any host fails
        let result = parse_share_link("vless://");
        assert!(result.is_err());
    }

    #[test]
    fn add_profile_from_text_adds_to_app() {
        let mut app = App::test_new(Config::default());
        let uri = "vless://uuid@1.2.3.4:443#TestProfile";
        add_profile_from_text(&mut app, uri).unwrap();
        assert_eq!(app.config.profiles.len(), 1);
        assert_eq!(app.config.profiles[0].name, "TestProfile");
        assert!(app.status.text().contains("Pasted profile"));
    }
}
