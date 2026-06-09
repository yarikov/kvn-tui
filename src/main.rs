mod app;
mod cli;
mod config;
mod daemon;
mod infra;
mod ipc;
mod services;
mod singbox;
mod tui_client;
mod ui;

#[cfg(test)]
mod test_helpers;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::app::model::Model;
use crate::infra::paths::ensure_config_dirs;

/// Entry point for the TUI VPN client.
fn main() -> Result<()> {
    let cli = cli::Cli::parse();

    if let Some(result) = cli::try_run_from_parsed(&cli) {
        return result;
    }

    // Initialize logging.
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer().without_time())
        .init();

    // Ensure configuration directories exist.
    ensure_config_dirs()?;

    if cli.daemon {
        let model = Model::new()?;
        daemon::run(model)?;
    } else {
        if !ipc::is_daemon_running() {
            let model = Model::new()?;
            std::thread::spawn(move || {
                if let Err(e) = daemon::run(model) {
                    tracing::error!("Daemon error: {}", e);
                }
            });
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
        tui_client::run()?;
    }

    Ok(())
}
