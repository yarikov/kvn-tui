use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

pub mod profile;
pub mod subscription;

use profile::Config;

/// Load configuration from a specific path.
pub fn load_config_at(path: &Path) -> Result<Config> {
    if !path.exists() {
        return Ok(Config::default());
    }

    let contents =
        fs::read_to_string(path).with_context(|| format!("Failed to read {:?}", path))?;

    let mut config: Config =
        serde_json::from_str(&contents).with_context(|| format!("Failed to parse {:?}", path))?;
    let loaded_schema_version = config.schema_version;
    config
        .migrate()
        .with_context(|| format!("Failed to migrate {:?}", path))?;
    if config.schema_version != loaded_schema_version && config.validate().is_ok() {
        save_config_at(path, &config)
            .with_context(|| format!("Failed to persist migration for {:?}", path))?;
    }

    Ok(config)
}

/// Save configuration to a specific path atomically.
///
/// Fail-close on invalid input: [`Config::validate`] runs before the file is
/// touched, so a broken in-memory state cannot overwrite a good `profiles.json`.
pub fn save_config_at(path: &Path, config: &Config) -> Result<()> {
    config
        .validate()
        .context("Refusing to save invalid config")?;

    let dir = path.parent().context("Invalid config path")?;
    fs::create_dir_all(dir)?;

    // Mirror `dns.strategy` into the legacy `dns_strategy` field so configs
    // remain readable by older kvn-tui builds during the deprecation window.
    let mut serializable = config.clone();
    serializable.settings.dns_strategy = serializable.settings.dns.strategy.clone();

    let json = serde_json::to_string_pretty(&serializable)?;
    crate::atomic_write::write(path, json.as_bytes())?;

    Ok(())
}

/// Load configuration from disk, or return default if not present.
///
/// On first launch (no `profiles.json` yet) under Omarchy, override the
/// default theme with the [`profile::OMARCHY_THEME_SENTINEL`] so the TUI
/// automatically follows the system theme instead of always starting on
/// `tokyo-night`. Existing configs are left untouched — the user's stored
/// theme choice always wins.
///
/// Also generates the installation HWID once (UUID v4) and persists it
/// immediately through the atomic config write path, so every later start —
/// and every subscription server — sees the same identifier.
pub fn load_config() -> Result<Config> {
    let path = crate::paths::profiles_path().context("Failed to determine profiles path")?;
    let is_first_launch = !path.exists();
    let mut config = load_config_at(&path)?;
    if is_first_launch && crate::omarchy::detect_omarchy_theme().is_some() {
        config.settings.theme = profile::OMARCHY_THEME_SENTINEL.to_string();
    }
    ensure_hwid(&mut config, &path)?;
    Ok(config)
}

/// Generate the installation HWID once and persist it immediately through the
/// existing atomic config write path. Idempotent: an existing non-empty
/// `settings.hwid` is never regenerated.
fn ensure_hwid(config: &mut Config, path: &Path) -> Result<()> {
    if !config.settings.hwid.is_empty() {
        return Ok(());
    }
    config.settings.hwid = subscription::generate_hwid();

    // Validation errors belong to the caller's existing config-validation
    // path. Do not turn an unrelated invalid profile into an HWID persistence
    // error merely because this load also generated the missing identifier.
    if config.validate().is_err() {
        return Ok(());
    }

    tracing::info!("Generated installation HWID");
    save_config_at(path, config).context("Failed to persist generated HWID")
}

/// Save configuration to disk atomically.
pub fn save_config(config: &Config) -> Result<()> {
    let path = crate::paths::profiles_path().context("Failed to determine profiles path")?;
    save_config_at(&path, config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    #[test]
    fn config_dir_matches_paths_module() {
        let from_paths = crate::paths::config_dir();
        assert!(from_paths.is_some());
    }

    #[test]
    fn load_config_missing_file_returns_default() {
        let path = PathBuf::from("/nonexistent/path/profiles.json");
        let config = load_config_at(&path).unwrap();
        assert!(config.profiles.is_empty());
        assert_eq!(config.settings.tun_interface, "tun0");
        assert_eq!(
            config.settings.dns_strategy,
            profile::DnsStrategy::PreferIpv4
        );
    }

    #[test]
    fn save_and_load_roundtrip() {
        let file = NamedTempFile::new().unwrap();
        let path = file.path().to_path_buf();

        let mut config = Config::default();
        config
            .settings
            .geo_routing
            .set_region(profile::GeoRegion::Ru);
        config
            .settings
            .geo_routing
            .set_mode(profile::RoutingMode::Only(profile::GeoRegion::Ru));
        config.profiles.push(profile::Profile::new_vless(
            "Test".to_string(),
            "1.2.3.4".to_string(),
            443,
            crate::test_helpers::TEST_UUID.to_string(),
        ));

        save_config_at(&path, &config).unwrap();
        let loaded = load_config_at(&path).unwrap();
        assert_eq!(loaded, config);
    }

    #[test]
    fn load_persists_v3_update_interval_migration() {
        let file = NamedTempFile::new().unwrap();
        let mut config = Config {
            schema_version: 2,
            ..Config::default()
        };
        config.subscriptions.push(profile::Subscription {
            id: uuid::Uuid::new_v4(),
            name: "legacy".into(),
            url: "https://example.com/sub".into(),
            auto_update: profile::SubscriptionAutoUpdate::Every1h,
            last_updated: None,
            next_auto_update: None,
            retry_state: None,
            send_hwid: false,
            hwid: None,
        });
        config.settings.geo_routing.auto_update = profile::GeoAutoUpdate::Every12h;
        save_config_at(file.path(), &config).unwrap();

        let loaded = load_config_at(file.path()).unwrap();
        let persisted = fs::read_to_string(file.path()).unwrap();

        assert_eq!(loaded.schema_version, profile::CURRENT_SCHEMA_VERSION);
        assert_eq!(
            loaded.subscriptions[0].auto_update,
            profile::SubscriptionAutoUpdate::Every1d
        );
        assert_eq!(
            loaded.settings.geo_routing.auto_update,
            profile::GeoAutoUpdate::Every1d
        );
        assert!(persisted.contains("\"schema_version\": 5"));
        assert!(persisted.contains("\"auto_update\": \"every1d\""));
        assert!(!persisted.contains("every1h"));
        assert!(!persisted.contains("every_12h"));
    }

    #[test]
    fn save_config_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a/b/c/profiles.json");
        let config = Config::default();
        save_config_at(&nested, &config).unwrap();
        assert!(nested.exists());
    }

    #[test]
    fn load_config_invalid_json_fails() {
        let mut file = NamedTempFile::new().unwrap();
        write!(file, "not json at all").unwrap();
        let result = load_config_at(file.path());
        assert!(result.is_err());
    }

    #[test]
    fn load_config_first_launch_on_omarchy_sets_sentinel() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        unsafe { std::env::set_var("XDG_STATE_HOME", state.path()) };
        let _ = std::fs::remove_file(crate::paths::profiles_path().unwrap());
        let current = dir.path().join("omarchy").join("current");
        std::fs::create_dir_all(&current).unwrap();
        std::fs::write(current.join("theme.name"), "catppuccin-mocha\n").unwrap();

        let config = load_config().unwrap();
        assert_eq!(config.settings.theme, profile::OMARCHY_THEME_SENTINEL);
    }

    #[test]
    fn load_config_first_launch_without_omarchy_keeps_default() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        unsafe { std::env::set_var("XDG_STATE_HOME", state.path()) };
        let _ = std::fs::remove_file(crate::paths::profiles_path().unwrap());

        let config = load_config().unwrap();
        assert_eq!(config.settings.theme, "tokyo-night");
    }

    #[test]
    fn load_config_existing_file_ignores_omarchy() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        unsafe { std::env::set_var("XDG_STATE_HOME", state.path()) };
        let current = dir.path().join("omarchy").join("current");
        std::fs::create_dir_all(&current).unwrap();
        std::fs::write(current.join("theme.name"), "gruvbox\n").unwrap();

        let mut stored = Config::default();
        stored.settings.theme = "tokyo-night".to_string();
        save_config(&stored).unwrap();

        let loaded = load_config().unwrap();
        assert_eq!(loaded.settings.theme, "tokyo-night");
    }

    #[test]
    fn load_config_generates_hwid_once_and_persists_it() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let _ = std::fs::remove_file(crate::paths::profiles_path().unwrap());

        let config = load_config().unwrap();
        let hwid = config.settings.hwid.clone();
        assert!(hwid.starts_with("lnx-"));
        assert_eq!(hwid.len(), "lnx-".len() + 32);

        // The generated HWID is immediately written through the atomic path.
        let persisted = std::fs::read_to_string(crate::paths::profiles_path().unwrap()).unwrap();
        assert!(persisted.contains(&hwid));

        // A second load retains the same installation HWID.
        let reloaded = load_config().unwrap();
        assert_eq!(reloaded.settings.hwid, hwid);
    }

    #[test]
    fn ensure_hwid_propagates_persistence_error() {
        let dir = tempfile::tempdir().unwrap();
        let parent_file = dir.path().join("not-a-directory");
        std::fs::write(&parent_file, "occupied").unwrap();
        let path = parent_file.join("profiles.json");
        let mut config = Config::default();

        let error = ensure_hwid(&mut config, &path).unwrap_err();

        assert!(
            format!("{error:#}").contains("Failed to persist generated HWID"),
            "got: {error:#}"
        );
    }

    #[test]
    fn hwid_survives_save_and_load_roundtrip() {
        let file = NamedTempFile::new().unwrap();
        let path = file.path().to_path_buf();

        let mut config = Config::default();
        config.settings.hwid = "lnx-d415f1264a264924aa76e7f55380b0c7".to_string();
        save_config_at(&path, &config).unwrap();

        let loaded = load_config_at(&path).unwrap();
        assert_eq!(loaded.settings.hwid, "lnx-d415f1264a264924aa76e7f55380b0c7");
    }

    #[test]
    fn settings_and_subscriptions_without_hwid_fields_are_backward_compatible() {
        // Pre-HWID config shape: no `settings.hwid`, no `send_hwid`/`hwid` on
        // subscriptions — and no legacy `subscription_headers` either.
        let legacy = r#"{
            "schema_version": 3,
            "profiles": [],
            "subscriptions": [
                {
                    "id": "3f2504e0-4f89-11d3-9a0c-0305e82c3301",
                    "name": "Regular subscription",
                    "url": "https://example.com/regular"
                }
            ],
            "settings": {
                "tun_interface": "tun0",
                "dns_strategy": "prefer_ipv4",
                "theme": "tokyo-night",
                "log_level": "info"
            }
        }"#;
        let mut file = NamedTempFile::new().unwrap();
        write!(file, "{legacy}").unwrap();

        let config = load_config_at(file.path()).unwrap();
        assert_eq!(config.settings.hwid, "");
        assert!(!config.subscriptions[0].send_hwid);
        assert_eq!(config.subscriptions[0].hwid, None);
    }

    #[test]
    fn subscription_send_hwid_fields_roundtrip_through_json() {
        let json = r#"{
            "id": "3f2504e0-4f89-11d3-9a0c-0305e82c3301",
            "name": "HWID subscription",
            "url": "https://example.com/hwid",
            "send_hwid": true,
            "hwid": "provider-registered-device-id"
        }"#;
        let sub: profile::Subscription = serde_json::from_str(json).unwrap();
        assert!(sub.send_hwid);
        assert_eq!(sub.hwid.as_deref(), Some("provider-registered-device-id"));

        let serialized = serde_json::to_string(&sub).unwrap();
        let back: profile::Subscription = serde_json::from_str(&serialized).unwrap();
        assert_eq!(back, sub);
    }

    #[test]
    fn load_and_save_config_use_default_path() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        // Remove any existing file
        let _ = std::fs::remove_file(crate::paths::profiles_path().unwrap());

        let config = load_config().unwrap();
        assert!(config.profiles.is_empty());

        let mut config = Config::default();
        config.profiles.push(profile::Profile::new_vless(
            "PathTest".to_string(),
            "9.9.9.9".to_string(),
            443,
            crate::test_helpers::TEST_UUID.to_string(),
        ));
        save_config(&config).unwrap();

        let loaded = load_config().unwrap();
        assert_eq!(loaded.profiles.len(), 1);
        assert_eq!(loaded.profiles[0].name, "PathTest");
    }
}
