mod app;
mod cli;
mod clipboard;
mod config;
mod editor;
mod geo;
mod paths;
mod process_handle;
mod runtime;
mod services;
mod singbox;
mod ui;

#[cfg(test)]
mod test_helpers;

use anyhow::Result;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::app::model::Model;
use crate::paths::ensure_config_dirs;

/// Entry point for the TUI VPN client.
fn main() -> Result<()> {
    if let Some(result) = cli::try_run() {
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
