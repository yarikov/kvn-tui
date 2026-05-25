use std::env;
use std::io::stdout;
use std::process::Command;

use anyhow::{Context, Result};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;

use crate::paths::profiles_path;

/// Detect the user's preferred editor using $VISUAL, $EDITOR, or a fallback chain.
fn detect_editor() -> String {
    env::var("VISUAL")
        .or_else(|_| env::var("EDITOR"))
        .unwrap_or_else(|_| {
            for candidate in &["nvim", "vim", "vi", "nano"] {
                if Command::new("sh")
                    .arg("-c")
                    .arg(format!("command -v {}", candidate))
                    .status()
                    .is_ok_and(|s| s.success())
                {
                    return candidate.to_string();
                }
            }
            "vi".to_string()
        })
}

/// Open profiles.json in the user's preferred editor.
/// Temporarily restores the terminal (leaves alternate screen and disables raw mode)
/// so the editor can take full control.
pub fn open_profiles_editor() -> Result<()> {
    let editor = detect_editor();
    let path = profiles_path().context("Failed to determine profiles path")?;

    // Ensure the file exists before opening.
    if !path.exists() {
        let default_config = crate::config::profile::Config::default();
        crate::config::save_config(&default_config)?;
    }

    let mut out = stdout();

    // Restore terminal for the external editor.
    disable_raw_mode().ok();
    out.execute(LeaveAlternateScreen).ok();

    let status = Command::new(&editor)
        .arg(&path)
        .status()
        .with_context(|| format!("Failed to launch editor: {}", editor))?;

    // Return to TUI mode.
    out.execute(EnterAlternateScreen).ok();
    enable_raw_mode().ok();

    if !status.success() {
        anyhow::bail!("Editor exited with non-zero status");
    }

    Ok(())
}
