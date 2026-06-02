use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::config::profile::{Profile, Settings};
use crate::config::singbox::generate_config;
use crate::process_handle::ProcessHandle;

fn resolve_singbox_binary() -> String {
    std::env::var("SING_BOX_PATH").unwrap_or_else(|_| "sing-box".to_string())
}

static SINGBOX_BINARY: OnceLock<String> = OnceLock::new();

/// Path to the sing-box binary. Can be overridden by SING_BOX_PATH env variable.
fn singbox_binary() -> &'static str {
    SINGBOX_BINARY.get_or_init(resolve_singbox_binary)
}

/// Write the generated sing-box configuration to a temporary file.
fn write_config(profile: &Profile, settings: &Settings) -> Result<PathBuf> {
    let config =
        generate_config(profile, settings).context("Failed to generate sing-box config")?;
    let path = crate::paths::temp_singbox_config_path();

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(&path, serde_json::to_string_pretty(&config)?)
        .with_context(|| format!("Failed to write config to {:?}", path))?;

    Ok(path)
}

/// Validate the sing-box configuration by running `sing-box check`.
fn check_config(path: &PathBuf) -> Result<()> {
    let output = Command::new(singbox_binary())
        .arg("check")
        .arg("-c")
        .arg(path)
        .output()
        .with_context(|| format!("Failed to run {} check", singbox_binary()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("sing-box config validation failed: {}", stderr);
    }

    Ok(())
}

/// Start the sing-box process with the given profile.
/// Validates config first, then spawns the process and verifies it stays alive.
pub fn start(profile: &Profile, settings: &Settings) -> Result<ProcessHandle> {
    let config_path = write_config(profile, settings)?;

    // Validate configuration before starting.
    check_config(&config_path)?;

    let mut child = Command::new(singbox_binary())
        .arg("run")
        .arg("-c")
        .arg(&config_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to start sing-box (binary: {})", singbox_binary()))?;

    // Give sing-box a moment to either start or fail immediately.
    thread::sleep(Duration::from_millis(300));

    // Check if process exited immediately with an error.
    match child.try_wait() {
        Ok(Some(status)) => {
            let mut stderr = String::new();
            if let Some(ref mut err) = child.stderr {
                if let Err(e) = err.read_to_string(&mut stderr) {
                    tracing::warn!("Failed to read sing-box stderr: {}", e);
                }
            }
            anyhow::bail!(
                "sing-box exited immediately (code: {:?}). stderr: {}",
                status.code(),
                stderr.trim()
            );
        }
        Ok(None) => {
            // Process is still running — good.
            Ok(ProcessHandle::new(child))
        }
        Err(e) => {
            anyhow::bail!("Failed to check sing-box status: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::profile::{Profile, Protocol};

    #[test]
    fn singbox_binary_resolution() {
        // Default (no env override)
        unsafe { std::env::remove_var("SING_BOX_PATH") };
        assert_eq!(resolve_singbox_binary(), "sing-box");

        // With env override
        unsafe { std::env::set_var("SING_BOX_PATH", "/usr/local/bin/sing-box") };
        assert_eq!(resolve_singbox_binary(), "/usr/local/bin/sing-box");
        unsafe { std::env::remove_var("SING_BOX_PATH") };
    }

    #[test]
    fn write_config_creates_valid_json() {
        let profile = Profile::new(
            "Test".to_string(),
            Protocol::Vless,
            "1.2.3.4".to_string(),
            443,
            "uuid".to_string(),
        );
        let settings = Settings::default();
        let path = write_config(&profile, &settings).unwrap();
        assert!(path.exists());

        let contents = fs::read_to_string(&path).unwrap();
        let json: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert!(json.get("log").is_some());
        assert!(json.get("outbounds").is_some());

        // Clean up
        let _ = fs::remove_file(&path);
    }
}
