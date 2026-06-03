use std::env;
use std::fs;
use std::io::{self, stdout};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use crossterm::ExecutableCommand;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

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
        stdout().execute(LeaveAlternateScreen).inspect_err(|e| {
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
        let _ =
            enable_raw_mode().inspect_err(|e| tracing::warn!("Failed to enable raw mode: {}", e));
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

/// Determine the 1-based line number of the start of `profile_index`-th profile
/// in a pretty-printed JSON file.
fn find_profile_line(path: &Path, profile_index: usize) -> Option<usize> {
    let content = fs::read_to_string(path).ok()?;

    enum State {
        Normal,
        InString,
        InStringEscape,
    }

    let mut in_profiles = false;
    let mut depth = 0;
    let mut profile_count = 0;
    let mut state = State::Normal;

    for (line_num, line) in content.lines().enumerate() {
        let trimmed = line.trim();

        if !in_profiles {
            if trimmed.starts_with("\"profiles\"") {
                in_profiles = true;
            } else {
                continue;
            }
        }

        for c in line.chars() {
            match state {
                State::Normal => match c {
                    '"' => state = State::InString,
                    '[' if in_profiles => {
                        depth += 1;
                    }
                    ']' if in_profiles && depth > 0 => {
                        depth -= 1;
                        if depth == 0 {
                            return None;
                        }
                    }
                    '{' if in_profiles && depth == 1 => {
                        if profile_count == profile_index {
                            return Some(line_num + 1);
                        }
                        profile_count += 1;
                        depth += 1;
                    }
                    '{' if in_profiles => {
                        depth += 1;
                    }
                    '}' if in_profiles && depth > 0 => {
                        depth -= 1;
                    }
                    _ => {}
                },
                State::InString => {
                    if c == '\\' {
                        state = State::InStringEscape;
                    } else if c == '"' {
                        state = State::Normal;
                    }
                }
                State::InStringEscape => state = State::InString,
            }
        }
    }

    None
}

/// Build editor command-line arguments that jump to `line` in `path`.
fn editor_args(editor: &str, path: &Path, line: usize) -> Vec<String> {
    let name = Path::new(editor)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(editor);

    match name {
        "code" | "code-oss" | "codium" => {
            vec!["--goto".to_string(), format!("{}:{}", path.display(), line)]
        }
        _ => vec![format!("+{}", line), path.display().to_string()],
    }
}

/// Open `profiles.json` in the user's preferred external editor.
///
/// If `profile_index` is within bounds, the editor will be asked to jump to the
/// line where that profile object starts. Terminal state is temporarily restored
/// so the editor can take full control. A backup is created before editing; if
/// the edited file contains invalid JSON, the backup is restored automatically
/// and an error is returned. On success the parsed [`Config`] is returned so
/// the application can reload.
pub fn open_profiles_editor(profile_index: usize) -> Result<Config> {
    let editor = detect_editor();
    let path = profiles_path().context("Failed to determine profiles path")?;

    ensure_profiles_file(&path)?;

    let mut backup = ConfigBackup::create(&path)?;

    let _guard = TerminalGuard::new().context("Failed to restore terminal for external editor")?;

    let args = if let Some(line) = find_profile_line(&path, profile_index) {
        editor_args(&editor, &path, line)
    } else {
        vec![path.display().to_string()]
    };

    let status = crate::user_env::command_as_user(&editor)
        .args(&args)
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

    #[test]
    fn find_profile_line_first_profile() {
        use crate::config::profile::{Profile, Protocol};
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("profiles.json");

        let mut config = Config::default();
        config.profiles.push(Profile::new(
            "First".to_string(),
            Protocol::Vless,
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        ));

        let json = serde_json::to_string_pretty(&config).unwrap();
        fs::write(&path, json).unwrap();

        let line = find_profile_line(&path, 0);
        assert!(line.is_some());

        let content = fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = content.lines().collect();
        assert_eq!(lines[line.unwrap() - 1].trim(), "{");
        // The line after the opening brace should contain the first profile's id
        assert!(lines[line.unwrap()].contains("id"));
    }

    #[test]
    fn find_profile_line_second_profile() {
        use crate::config::profile::{Profile, Protocol};
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("profiles.json");

        let mut config = Config::default();
        config.profiles.push(Profile::new(
            "First".to_string(),
            Protocol::Vless,
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        ));
        config.profiles.push(Profile::new(
            "Second".to_string(),
            Protocol::Vless,
            "2.2.2.2".to_string(),
            443,
            "u2".to_string(),
        ));

        let json = serde_json::to_string_pretty(&config).unwrap();
        fs::write(&path, json).unwrap();

        let line0 = find_profile_line(&path, 0).unwrap();
        let line1 = find_profile_line(&path, 1).unwrap();
        assert!(line1 > line0);

        let content = fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = content.lines().collect();
        assert_eq!(lines[line1 - 1].trim(), "{");
        assert!(lines[line1].contains("Second") || lines[line1 + 1].contains("Second"));
    }

    #[test]
    fn find_profile_line_out_of_bounds() {
        use crate::config::profile::{Profile, Protocol};
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("profiles.json");

        let mut config = Config::default();
        config.profiles.push(Profile::new(
            "Only".to_string(),
            Protocol::Vless,
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        ));

        let json = serde_json::to_string_pretty(&config).unwrap();
        fs::write(&path, json).unwrap();

        assert_eq!(find_profile_line(&path, 5), None);
    }

    #[test]
    fn find_profile_line_empty_profiles() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("profiles.json");

        let config = Config::default();
        let json = serde_json::to_string_pretty(&config).unwrap();
        fs::write(&path, json).unwrap();

        assert_eq!(find_profile_line(&path, 0), None);
    }
}
