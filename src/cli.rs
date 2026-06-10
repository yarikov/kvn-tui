use anyhow::{Context, Result};
use clap::Parser;

use crate::services::waybar;

#[derive(Parser)]
#[command(version, about)]
pub struct Cli {
    #[arg(long, help = "Print connection status as JSON for Waybar integration")]
    waybar_status: bool,

    #[arg(
        long,
        help = "Install Omarchy integration (Waybar module and desktop entry for Walker)"
    )]
    install_omarchy: bool,

    #[arg(
        long,
        help = "Install polkit rule to allow network group to manage DNS without password prompts"
    )]
    install_polkit: bool,

    #[arg(long, help = "Run the headless daemon that manages sing-box")]
    pub daemon: bool,
}

/// Run the embedded Omarchy integration installer script.
fn install_omarchy() -> Result<()> {
    let script = include_str!("../contrib/install-omarchy.sh");
    let tmp = std::env::temp_dir().join("kvn-tui-install-omarchy.sh");
    std::fs::write(&tmp, script)?;
    let status = std::process::Command::new("bash").arg(&tmp).status()?;
    std::fs::remove_file(&tmp).ok();
    if !status.success() {
        anyhow::bail!("install-omarchy.sh exited with status {}", status);
    }
    Ok(())
}

/// Run the embedded polkit rule installer script.
fn install_polkit() -> Result<()> {
    let script = include_str!("../contrib/install-polkit.sh");
    let tmp = std::env::temp_dir().join("kvn-tui-install-polkit.sh");
    std::fs::write(&tmp, script)?;
    let status = std::process::Command::new("bash")
        .arg(&tmp)
        .status()
        .context("failed to run install-polkit.sh")?;
    std::fs::remove_file(&tmp).ok();
    if !status.success() {
        anyhow::bail!("install-polkit.sh exited with status {}", status);
    }
    Ok(())
}

/// Parse CLI arguments and execute any non-TUI commands.
///
/// Returns `Some(Ok(()))` or `Some(Err(_))` if a CLI action was handled
/// and the application should exit. Returns `None` if the TUI should start.
#[allow(dead_code)]
pub fn try_run() -> Option<Result<()>> {
    let cli = Cli::parse();
    try_run_from_parsed(&cli)
}

/// Same as `try_run` but takes an already-parsed `Cli`.
pub fn try_run_from_parsed(cli: &Cli) -> Option<Result<()>> {
    if cli.install_omarchy {
        return Some(install_omarchy());
    }
    if cli.install_polkit {
        return Some(install_polkit());
    }
    if cli.waybar_status {
        waybar::print_status();
        return Some(Ok(()));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_via_clap() {
        // clap handles --version automatically
        let cli = Cli::try_parse_from(["kvn-tui", "--version"]);
        assert!(cli.is_err()); // clap exits on --version, but in test it returns Err
    }

    #[test]
    fn waybar_status_flag_detected() {
        let cli = Cli::parse_from(["kvn-tui", "--waybar-status"]);
        assert!(cli.waybar_status);
    }

    #[test]
    fn install_omarchy_flag_detected() {
        let cli = Cli::parse_from(["kvn-tui", "--install-omarchy"]);
        assert!(cli.install_omarchy);
    }

    #[test]
    fn install_polkit_flag_detected() {
        let cli = Cli::parse_from(["kvn-tui", "--install-polkit"]);
        assert!(cli.install_polkit);
    }

    #[test]
    fn daemon_flag_detected() {
        let cli = Cli::parse_from(["kvn-tui", "--daemon"]);
        assert!(cli.daemon);
    }
}
