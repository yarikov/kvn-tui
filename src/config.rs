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
pub fn load_config() -> Result<Config> {
    let path = crate::paths::profiles_path().context("Failed to determine profiles path")?;
    let is_first_launch = !path.exists();
    let mut config = load_config_at(&path)?;
    if is_first_launch && crate::omarchy::detect_omarchy_theme().is_some() {
        config.settings.theme = profile::OMARCHY_THEME_SENTINEL.to_string();
    }
    Ok(config)
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
        assert!(persisted.contains("\"schema_version\": 4"));
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
