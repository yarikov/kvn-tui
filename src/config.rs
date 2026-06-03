use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

pub mod profile;

use profile::Config;

/// Load configuration from a specific path.
pub fn load_config_at(path: &Path) -> Result<Config> {
    if !path.exists() {
        return Ok(Config::default());
    }

    let contents =
        fs::read_to_string(path).with_context(|| format!("Failed to read {:?}", path))?;

    let config: Config =
        serde_json::from_str(&contents).with_context(|| format!("Failed to parse {:?}", path))?;

    Ok(config)
}

/// Save configuration to a specific path atomically.
pub fn save_config_at(path: &Path, config: &Config) -> Result<()> {
    let dir = path.parent().context("Invalid config path")?;
    fs::create_dir_all(dir)?;

    let temp = dir.join("profiles.json.tmp");
    let json = serde_json::to_string_pretty(config)?;
    fs::write(&temp, json)?;
    fs::rename(&temp, path)?;

    Ok(())
}

/// Load configuration from disk, or return default if not present.
pub fn load_config() -> Result<Config> {
    let path = crate::infra::paths::profiles_path().context("Failed to determine profiles path")?;
    load_config_at(&path)
}

/// Save configuration to disk atomically.
pub fn save_config(config: &Config) -> Result<()> {
    let path = crate::infra::paths::profiles_path().context("Failed to determine profiles path")?;
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
        let from_paths = crate::infra::paths::config_dir();
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
        config.settings.routing_mode = profile::RoutingMode::OnlyRu;
        config.profiles.push(profile::Profile::new(
            "Test".to_string(),
            profile::Protocol::Vless,
            "1.2.3.4".to_string(),
            443,
            "uuid".to_string(),
        ));

        save_config_at(&path, &config).unwrap();
        let loaded = load_config_at(&path).unwrap();
        assert_eq!(loaded, config);
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
    fn load_and_save_config_use_default_path() {
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        // Remove any existing file
        let _ = std::fs::remove_file(crate::infra::paths::profiles_path().unwrap());

        let config = load_config().unwrap();
        assert!(config.profiles.is_empty());

        let mut config = Config::default();
        config.profiles.push(profile::Profile::new(
            "PathTest".to_string(),
            profile::Protocol::Vless,
            "9.9.9.9".to_string(),
            443,
            "uuid".to_string(),
        ));
        save_config(&config).unwrap();

        let loaded = load_config().unwrap();
        assert_eq!(loaded.profiles.len(), 1);
        assert_eq!(loaded.profiles[0].name, "PathTest");
    }
}
