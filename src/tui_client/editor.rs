use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

use crate::app::model::SourceRow;
use crate::config::profile::Config;
use crate::paths::profiles_path;

/// Config object that should be selected when the editor opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EditorTarget {
    Profile(usize),
    Subscription(usize),
}

impl From<SourceRow> for EditorTarget {
    fn from(row: SourceRow) -> Self {
        match row {
            SourceRow::StandaloneProfile(idx)
            | SourceRow::SubscriptionProfile {
                profile_idx: idx, ..
            } => Self::Profile(idx),
            SourceRow::SubscriptionHeader(idx) => Self::Subscription(idx),
        }
    }
}

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

/// Determine the 1-based line number of an object in a top-level JSON array.
fn find_array_item_line(path: &Path, array_name: &str, item_index: usize) -> Option<usize> {
    let content = fs::read_to_string(path).ok()?;

    enum State {
        Normal,
        InString,
        InStringEscape,
    }

    let array_prefix = format!("\"{array_name}\"");
    let mut in_array = false;
    let mut depth = 0;
    let mut item_count = 0;
    let mut state = State::Normal;

    for (line_num, line) in content.lines().enumerate() {
        let trimmed = line.trim();

        if !in_array {
            if trimmed.starts_with(&array_prefix) {
                in_array = true;
            } else {
                continue;
            }
        }

        for c in line.chars() {
            match state {
                State::Normal => match c {
                    '"' => state = State::InString,
                    '[' if in_array => {
                        depth += 1;
                    }
                    ']' if in_array && depth > 0 => {
                        depth -= 1;
                        if depth == 0 {
                            return None;
                        }
                    }
                    '{' if in_array && depth == 1 => {
                        if item_count == item_index {
                            return Some(line_num + 1);
                        }
                        item_count += 1;
                        depth += 1;
                    }
                    '{' if in_array => {
                        depth += 1;
                    }
                    '}' if in_array && depth > 0 => {
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

fn find_target_line(path: &Path, target: EditorTarget) -> Option<usize> {
    match target {
        EditorTarget::Profile(idx) => find_array_item_line(path, "profiles", idx),
        EditorTarget::Subscription(idx) => find_array_item_line(path, "subscriptions", idx),
    }
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

pub(super) struct EditedConfig {
    pub base: Config,
    pub edited: Config,
}

/// Edit an isolated snapshot. The live `profiles.json` remains daemon-owned.
pub fn open_profiles_editor(target: Option<EditorTarget>, base: Config) -> Result<EditedConfig> {
    let editor = detect_editor();
    let (program, base_args) = split_editor(&editor);
    let live_path = profiles_path().context("Failed to determine profiles path")?;
    let path =
        live_path.with_file_name(format!(".profiles.json.edit-{}.json", uuid::Uuid::new_v4()));
    crate::config::save_config_at(&path, &base).context("Failed to create editor snapshot")?;

    let args = if let Some(line) = target.and_then(|target| find_target_line(&path, target)) {
        editor_args(&program, &path, line)
    } else {
        vec![path.display().to_string()]
    };

    let status = Command::new(&program)
        .args(&base_args)
        .args(&args)
        .status()
        .with_context(|| format!("Failed to launch editor: {}", editor));

    let status = match status {
        Ok(status) => status,
        Err(error) => {
            let _ = fs::remove_file(&path);
            return Err(error);
        }
    };

    if !status.success() {
        let _ = fs::remove_file(&path);
        anyhow::bail!("Editor exited with non-zero status");
    }

    let edited = match crate::config::load_config_at(&path) {
        Ok(cfg) => cfg,
        Err(e) => {
            let conflict = live_path.with_file_name(format!(
                "profiles.json.conflict-invalid-{}-{}",
                chrono::Local::now().format("%Y%m%dT%H%M%S"),
                uuid::Uuid::new_v4()
            ));
            fs::rename(&path, &conflict).with_context(|| {
                format!("Failed to preserve invalid edit at {}", conflict.display())
            })?;
            return Err(e).with_context(|| {
                format!(
                    "Invalid JSON; live config was not changed. Edited version saved to {}.",
                    conflict.display()
                )
            });
        }
    };

    if let Err(e) = edited.validate() {
        let conflict = live_path.with_file_name(format!(
            "profiles.json.conflict-invalid-{}-{}",
            chrono::Local::now().format("%Y%m%dT%H%M%S"),
            uuid::Uuid::new_v4()
        ));
        fs::rename(&path, &conflict).with_context(|| {
            format!("Failed to preserve invalid edit at {}", conflict.display())
        })?;
        return Err(e).with_context(|| {
            format!(
                "Validation failed; live config was not changed. Edited version saved to {}.",
                conflict.display()
            )
        });
    }

    let _ = fs::remove_file(&path);
    Ok(EditedConfig { base, edited })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

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

        let line = find_target_line(&path, EditorTarget::Profile(0));
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

        let line0 = find_target_line(&path, EditorTarget::Profile(0)).unwrap();
        let line1 = find_target_line(&path, EditorTarget::Profile(1)).unwrap();
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

        assert_eq!(find_target_line(&path, EditorTarget::Profile(5)), None);
    }

    #[test]
    fn find_profile_line_empty_profiles() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("profiles.json");

        let config = Config::default();
        let json = serde_json::to_string_pretty(&config).unwrap();
        fs::write(&path, json).unwrap();

        assert_eq!(find_target_line(&path, EditorTarget::Profile(0)), None);
    }

    #[test]
    fn find_subscription_line_first_and_second_subscription() {
        use crate::config::profile::Subscription;
        use uuid::Uuid;

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("profiles.json");
        let config = Config {
            subscriptions: vec![
                Subscription {
                    id: Uuid::new_v4(),
                    name: "First subscription".to_string(),
                    url: "https://example.com/first".to_string(),
                    auto_update: Default::default(),
                    last_updated: None,
                    next_auto_update: None,
                    retry_state: None,
                    send_hwid: false,
                    hwid: None,
                },
                Subscription {
                    id: Uuid::new_v4(),
                    name: "Second subscription".to_string(),
                    url: "https://example.com/second".to_string(),
                    auto_update: Default::default(),
                    last_updated: None,
                    next_auto_update: None,
                    retry_state: None,
                    send_hwid: false,
                    hwid: None,
                },
            ],
            ..Default::default()
        };
        fs::write(&path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

        let line0 = find_target_line(&path, EditorTarget::Subscription(0)).unwrap();
        let line1 = find_target_line(&path, EditorTarget::Subscription(1)).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = content.lines().collect();

        assert!(line1 > line0);
        assert_eq!(lines[line0 - 1].trim(), "{");
        assert_eq!(lines[line1 - 1].trim(), "{");
        assert!(lines[line0 + 1].contains("First subscription"));
        assert!(lines[line1 + 1].contains("Second subscription"));
    }

    #[test]
    fn editor_target_maps_source_rows() {
        assert_eq!(
            EditorTarget::from(SourceRow::StandaloneProfile(2)),
            EditorTarget::Profile(2)
        );
        assert_eq!(
            EditorTarget::from(SourceRow::SubscriptionProfile {
                sub_idx: 1,
                profile_idx: 4,
            }),
            EditorTarget::Profile(4)
        );
        assert_eq!(
            EditorTarget::from(SourceRow::SubscriptionHeader(3)),
            EditorTarget::Subscription(3)
        );
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
}
