mod app;
mod clipboard;
mod config;
mod editor;
mod geo;
mod input;
mod paths;
mod singbox;
mod suspend;
mod ui;

#[cfg(test)]
mod test_helpers;

use anyhow::Result;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::app::App;
use crate::paths::ensure_config_dirs;

/// Entry point for the TUI VPN client.
#[tokio::main]
async fn main() -> Result<()> {
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
    let app = App::new()?;

    // Run the terminal user interface.
    if let Err(e) = ui::run(app).await {
        tracing::error!("TUI error: {}", e);
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }

    Ok(())
}
