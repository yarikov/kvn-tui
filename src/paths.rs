use std::path::PathBuf;

/// Return the application configuration directory (`~/.config/kvn-tui`).
pub fn config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("kvn-tui"))
}

/// Return the path to `profiles.json`.
pub fn profiles_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("profiles.json"))
}

/// Return the path to the sing-box log file.
pub fn singbox_log_path() -> PathBuf {
    config_dir()
        .map(|d| d.join("logs/sing-box.log"))
        .unwrap_or_else(|| PathBuf::from("sing-box.log"))
}

/// Return the path to the temporary sing-box JSON configuration.
pub fn temp_singbox_config_path() -> PathBuf {
    if let Some(dir) = dirs::runtime_dir() {
        dir.join("kvn-tui-singbox.json")
    } else if let Some(dir) = dirs::cache_dir() {
        dir.join("kvn-tui/singbox.json")
    } else {
        PathBuf::from("/tmp/kvn-tui-singbox.json")
    }
}

/// Return the directory for geo rule-set databases.
pub fn geo_dir() -> PathBuf {
    config_dir()
        .map(|d| d.join("geo"))
        .unwrap_or_else(|| PathBuf::from("./geo"))
}

/// Ensure the configuration directory and its sub-directories exist.
/// Returns the config directory on success.
pub fn ensure_config_dirs() -> anyhow::Result<PathBuf> {
    use anyhow::Context;
    use std::fs;

    let dir = config_dir().context("Failed to determine config directory")?;
    fs::create_dir_all(&dir)?;
    fs::create_dir_all(dir.join("logs"))?;
    fs::create_dir_all(dir.join("geo"))?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn singbox_log_path_is_not_empty() {
        let path = singbox_log_path();
        assert!(!path.as_os_str().is_empty());
    }

    #[test]
    fn temp_singbox_config_path_is_not_empty() {
        let path = temp_singbox_config_path();
        assert!(!path.as_os_str().is_empty());
    }

    #[test]
    fn geo_dir_is_not_empty() {
        let path = geo_dir();
        assert!(!path.as_os_str().is_empty());
    }

    #[test]
    fn profiles_path_inside_config_dir() {
        let config = config_dir().unwrap();
        let profiles = profiles_path().unwrap();
        assert!(profiles.starts_with(&config));
    }
}
