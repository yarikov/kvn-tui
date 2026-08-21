use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

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

/// Split an editor command into program and leading arguments.
///
/// $VISUAL/$EDITOR may contain flags, not just a binary — Omarchy 4 sets
/// `EDITOR="omarchy-launch-editor --inline"` — so the value cannot be passed
/// to `Command::new` whole.
fn split_editor(editor: &str) -> (String, Vec<String>) {
    let mut parts = editor.split_whitespace().map(String::from);
    let program = parts.next().unwrap_or_else(|| "vi".to_string());
    (program, parts.collect())
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
/// line where that profile object starts. The caller must restore the terminal
/// before invoking this function. A backup is created before editing; if
/// the edited file contains invalid JSON, the backup is restored automatically
/// and an error is returned. On success the parsed [`Config`] is returned so
/// the application can reload.
pub fn open_profiles_editor(profile_index: usize) -> Result<Config> {
    let editor = detect_editor();
    let (program, base_args) = split_editor(&editor);
    let path = profiles_path().context("Failed to determine profiles path")?;

    ensure_profiles_file(&path)?;

    let mut backup = ConfigBackup::create(&path)?;

    let args = if let Some(line) = find_profile_line(&path, profile_index) {
        editor_args(&program, &path, line)
    } else {
        vec![path.display().to_string()]
    };

    let status = Command::new(&program)
        .args(&base_args)
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
        use crate::config::profile::Profile;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("profiles.json");

        let mut config = Config::default();
        config.profiles.push(Profile::new_vless(
            "First".to_string(),
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
        use crate::config::profile::Profile;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("profiles.json");

        let mut config = Config::default();
        config.profiles.push(Profile::new_vless(
            "First".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        ));
        config.profiles.push(Profile::new_vless(
            "Second".to_string(),
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
        use crate::config::profile::Profile;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("profiles.json");

        let mut config = Config::default();
        config.profiles.push(Profile::new_vless(
            "Only".to_string(),
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

    // ---- editor_args ----

    #[test]
    fn split_editor_bare_program() {
        assert_eq!(split_editor("nvim"), ("nvim".to_string(), vec![]));
    }

    #[test]
    fn split_editor_program_with_flags() {
        assert_eq!(
            split_editor("omarchy-launch-editor --inline"),
            (
                "omarchy-launch-editor".to_string(),
                vec!["--inline".to_string()]
            )
        );
    }

    #[test]
    fn split_editor_empty_falls_back_to_vi() {
        assert_eq!(split_editor(""), ("vi".to_string(), vec![]));
    }

    #[test]
    fn editor_args_vim_uses_plus_line() {
        let path = Path::new("/tmp/profiles.json");
        let args = editor_args("vim", path, 42);
        assert_eq!(args, vec!["+42".to_string(), "/tmp/profiles.json".into()]);
    }

    #[test]
    fn editor_args_nvim_uses_plus_line() {
        let args = editor_args("nvim", Path::new("/tmp/p.json"), 7);
        assert_eq!(args[0], "+7");
        assert!(args[1].ends_with("p.json"));
    }

    #[test]
    fn editor_args_nano_uses_plus_line() {
        let args = editor_args("nano", Path::new("/x.json"), 3);
        assert_eq!(args[0], "+3");
    }

    #[test]
    fn editor_args_code_uses_goto_flag() {
        let args = editor_args("code", Path::new("/tmp/profiles.json"), 12);
        assert_eq!(
            args,
            vec!["--goto".to_string(), "/tmp/profiles.json:12".into()]
        );
    }

    #[test]
    fn editor_args_code_oss_and_codium_use_goto() {
        let a = editor_args("code-oss", Path::new("/p.json"), 1);
        let b = editor_args("codium", Path::new("/p.json"), 1);
        assert_eq!(a[0], "--goto");
        assert_eq!(b[0], "--goto");
    }

    #[test]
    fn editor_args_unknown_editor_uses_plus_line() {
        let args = editor_args("helix", Path::new("/x.json"), 9);
        assert_eq!(args[0], "+9");
        assert_eq!(args[1], "/x.json");
    }

    #[test]
    fn editor_args_handles_full_path_to_editor() {
        // Full path: only the file_name is matched against the switch table.
        let args = editor_args("/usr/bin/code", Path::new("/p.json"), 2);
        assert_eq!(args[0], "--goto");
    }

    // ---- detect_editor ----

    #[test]
    fn detect_editor_prefers_visual_over_editor() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let prev_visual = env::var("VISUAL").ok();
        let prev_editor = env::var("EDITOR").ok();
        // SAFETY: tests using ENV_LOCK serialize env mutation.
        unsafe {
            env::set_var("VISUAL", "my-visual-editor");
            env::set_var("EDITOR", "my-other-editor");
        }
        assert_eq!(detect_editor(), "my-visual-editor");
        unsafe {
            match prev_visual {
                Some(v) => env::set_var("VISUAL", v),
                None => env::remove_var("VISUAL"),
            }
            match prev_editor {
                Some(v) => env::set_var("EDITOR", v),
                None => env::remove_var("EDITOR"),
            }
        }
    }

    #[test]
    fn detect_editor_falls_back_to_command_v_chain_when_no_env_set() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let prev_visual = env::var("VISUAL").ok();
        let prev_editor = env::var("EDITOR").ok();
        unsafe {
            env::remove_var("VISUAL");
            env::remove_var("EDITOR");
        }
        // On any reasonable dev/CI box at least one of nvim/vim/vi/nano is
        // present; the function must return one of those (or "vi" as the
        // hard-coded last resort).
        let editor = detect_editor();
        assert!(
            ["nvim", "vim", "vi", "nano"].contains(&editor.as_str()),
            "unexpected fallback editor: {editor}"
        );
        unsafe {
            match prev_visual {
                Some(v) => env::set_var("VISUAL", v),
                None => env::remove_var("VISUAL"),
            }
            match prev_editor {
                Some(v) => env::set_var("EDITOR", v),
                None => env::remove_var("EDITOR"),
            }
        }
    }

    #[test]
    fn detect_editor_falls_back_to_editor_var() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let prev_visual = env::var("VISUAL").ok();
        let prev_editor = env::var("EDITOR").ok();
        unsafe {
            env::remove_var("VISUAL");
            env::set_var("EDITOR", "fallback-editor");
        }
        assert_eq!(detect_editor(), "fallback-editor");
        unsafe {
            match prev_visual {
                Some(v) => env::set_var("VISUAL", v),
                None => env::remove_var("VISUAL"),
            }
            match prev_editor {
                Some(v) => env::set_var("EDITOR", v),
                None => env::remove_var("EDITOR"),
            }
        }
    }

    // ---- ensure_profiles_file ----

    #[test]
    fn ensure_profiles_file_is_noop_when_present() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("profiles.json");
        fs::write(&path, "{\"profiles\":[]}").unwrap();
        let before = fs::read_to_string(&path).unwrap();
        ensure_profiles_file(&path).unwrap();
        let after = fs::read_to_string(&path).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn ensure_profiles_file_creates_default_when_missing() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("profiles.json");
        assert!(!path.exists());
        ensure_profiles_file(&path).unwrap();
        assert!(path.exists());
        // The created file must round-trip into a Config.
        let cfg = crate::config::load_config_at(&path).unwrap();
        assert!(cfg.profiles.is_empty());
    }
}
