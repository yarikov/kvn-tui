use anyhow::Result;
use clap::Parser;

use crate::services::state_io;

#[derive(Parser)]
#[command(version, about)]
pub struct Cli {
    #[arg(long)]
    waybar_status: bool,

    #[arg(long)]
    install_omarchy: bool,
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

/// Parse CLI arguments and execute any non-TUI commands.
///
/// Returns `Some(Ok(()))` or `Some(Err(_))` if a CLI action was handled
/// and the application should exit. Returns `None` if the TUI should start.
pub fn try_run() -> Option<Result<()>> {
    let cli = Cli::parse();

    if cli.install_omarchy {
        return Some(install_omarchy());
    }
    if cli.waybar_status {
        state_io::print_waybar_status();
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
}
