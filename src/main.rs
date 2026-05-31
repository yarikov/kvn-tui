mod clipboard;
mod config;
mod editor;
mod effect;
mod geo;
mod model;
mod msg;
mod paths;
mod services;
mod singbox;
mod state_io;
mod suspend;
mod ui;
mod update;
mod runtime;

#[cfg(test)]
mod test_helpers;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::model::Model;
use crate::paths::ensure_config_dirs;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
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
    let status = std::process::Command::new("bash")
        .arg(&tmp)
        .status()?;
    std::fs::remove_file(&tmp).ok();
    if !status.success() {
        anyhow::bail!("install-omarchy.sh exited with status {}", status);
    }
    Ok(())
}

/// Entry point for the TUI VPN client.
fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.install_omarchy {
        return install_omarchy();
    }
    if cli.waybar_status {
        state_io::print_waybar_status();
        return Ok(());
    }

    // Initialize logging.
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer().without_time())
        .init();

    // Ensure configuration directories exist.
    ensure_config_dirs()?;

    // Initialize application state.
    let model = Model::new()?;

    // Run the terminal user interface.
    if let Err(e) = runtime::run(model) {
        tracing::error!("TUI error: {}", e);
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }

    Ok(())
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
