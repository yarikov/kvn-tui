use std::env;
use std::fs;
use std::io::{self, stdout};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;

use crate::config::profile::Config;
use crate::paths::profiles_path;

/// Detect the user's preferred editor using $VISUAL, $EDITOR, or a fallback chain.
fn detect_editor() -> String {
    env::var("VISUAL")
        .or_else(|_| env::var("EDITOR"))
        .unwrap_or_else(|_| {
            for candidate in &["nvim", "vim", "vi", "nano"] {
                if Command::new("sh")
                    .args(["-c", &format!("command -v {candidate}")])
                    .status()
                    .is_ok_and(|s| s.success())
                {
                    return candidate.to_string();
                }
            }
            "vi".to_string()
        })
}

/// RAII guard that restores terminal state when dropped.
///
/// On creation: leaves the alternate screen and disables raw mode.
/// On drop: re-enters the alternate screen and re-enables raw mode.
struct TerminalGuard;

impl TerminalGuard {
    fn new() -> io::Result<Self> {
        disable_raw_mode().inspect_err(|e| {
            tracing::warn!("Failed to disable raw mode: {}", e);
        })?;
        stdout()
            .execute(LeaveAlternateScreen)
            .inspect_err(|e| {
                tracing::warn!("Failed to leave alternate screen: {}", e);
            })?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = stdout()
            .execute(EnterAlternateScreen)
            .inspect_err(|e| tracing::warn!("Failed to enter alternate screen: {}", e));
        let _ = enable_raw_mode()
            .inspect_err(|e| tracing::warn!("Failed to enable raw mode: {}", e));
    }
}

/// RAII guard for a config file backup.
///
/// On creation: copies `original` to a `.bak` sibling.
/// On drop: restores `original` from the backup unless [`ConfigBackup::commit`] was called.
struct ConfigBackup {
    original: PathBuf,
    backup: PathBuf,
    committed: bool,
}

impl ConfigBackup {
    fn create(original: &Path) -> Result<Self> {
        let backup = original.with_extension("json.bak");
        fs::copy(original, &backup)
            .with_context(|| format!("Failed to create backup at {:?}", backup))?;
        Ok(Self {
            original: original.to_path_buf(),
            backup,
            committed: false,
        })
    }

    /// Mark the backup as committed — the original file is valid and the
    /// backup can be safely removed on drop.
    fn commit(&mut self) {
        self.committed = true;
        let _ = fs::remove_file(&self.backup)
            .inspect_err(|e| tracing::warn!("Failed to remove backup file: {}", e));
    }
}

impl Drop for ConfigBackup {
    fn drop(&mut self) {
        if self.committed || !self.backup.exists() {
            return;
        }
        let _ = fs::rename(&self.backup, &self.original)
            .inspect_err(|e| tracing::warn!("Failed to restore config backup: {}", e));
    }
}

/// Ensure that `profiles.json` exists on disk, creating a default one if necessary.
fn ensure_profiles_file(path: &Path) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    let default_config = Config::default();
    crate::config::save_config_at(path, &default_config)
        .context("Failed to create default profiles.json")
}

/// Open `profiles.json` in the user's preferred external editor.
///
/// Terminal state is temporarily restored so the editor can take full control.
/// A backup is created before editing; if the edited file contains invalid JSON,
/// the backup is restored automatically and an error is returned.
/// On success the parsed [`Config`] is returned so the application can reload.
pub fn open_profiles_editor() -> Result<Config> {
    let editor = detect_editor();
    let path = profiles_path().context("Failed to determine profiles path")?;

    ensure_profiles_file(&path)?;

    let mut backup = ConfigBackup::create(&path)?;

    let _guard = TerminalGuard::new()
        .context("Failed to restore terminal for external editor")?;

    let status = Command::new(&editor)
        .arg(&path)
        .status()
        .with_context(|| format!("Failed to launch editor: {}", editor))?;

    if !status.success() {
        anyhow::bail!("Editor exited with non-zero status");
    }

    let config = match crate::config::load_config_at(&path) {
        Ok(cfg) => cfg,
        Err(e) => {
            return Err(e).with_context(|| {
                format!(
                    "Invalid JSON in {:?}. Original config restored from backup.",
                    path
                )
            });
            // ConfigBackup::drop restores the original automatically.
        }
    };

    if let Err(e) = config.validate() {
        return Err(e).with_context(|| {
            format!(
                "Validation failed for {:?}. Original config restored from backup.",
                path
            )
        });
        // ConfigBackup::drop restores the original automatically.
    }

    backup.commit();
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn config_backup_restores_on_drop() {
        let dir = TempDir::new().unwrap();
        let original = dir.path().join("profiles.json");

        let mut file = std::fs::File::create(&original).unwrap();
        file.write_all(b"valid content").unwrap();
        drop(file);

        {
            let _backup = ConfigBackup::create(&original).unwrap();
            std::fs::write(&original, "modified content").unwrap();
            // _backup drops here, should restore original
        }

        let content = std::fs::read_to_string(&original).unwrap();
        assert_eq!(content, "valid content");
    }

    #[test]
    fn config_backup_commits_successfully() {
        let dir = TempDir::new().unwrap();
        let original = dir.path().join("profiles.json");

        std::fs::write(&original, "old content").unwrap();

        {
            let mut backup = ConfigBackup::create(&original).unwrap();
            std::fs::write(&original, "new content").unwrap();
            backup.commit();
            // backup drops here but should NOT restore
        }

        let content = std::fs::read_to_string(&original).unwrap();
        assert_eq!(content, "new content");
        assert!(!original.with_extension("json.bak").exists());
    }
}
