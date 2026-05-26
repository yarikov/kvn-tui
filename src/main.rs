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

/// Check whether any of the provided CLI arguments is a version flag.
fn should_show_version<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter()
        .any(|arg| matches!(arg.as_ref(), "-v" | "-V" | "--version"))
}

/// Check whether any of the provided CLI arguments is the waybar status flag.
fn should_show_waybar_status<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter().any(|arg| arg.as_ref() == "--waybar-status")
}

/// Return the version string shown for `--version`.
fn version_string() -> String {
    format!("kvn-tui {}", env!("CARGO_PKG_VERSION"))
}

/// Entry point for the TUI VPN client.
#[tokio::main]
async fn main() -> Result<()> {
    // Handle waybar status flag (before anything else — fast and stateless).
    if should_show_waybar_status(std::env::args()) {
        App::print_waybar_status();
        return Ok(());
    }

    // Handle version flag.
    if should_show_version(std::env::args()) {
        println!("{}", version_string());
        return Ok(());
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
    let app = App::new()?;

    // Run the terminal user interface.
    if let Err(e) = ui::run(app).await {
        tracing::error!("TUI error: {}", e);
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{should_show_version, should_show_waybar_status, version_string};

    #[test]
    fn version_flag_v() {
        assert!(should_show_version(["-v"]));
    }

    #[test]
    fn version_string_format() {
        let s = version_string();
        assert!(s.starts_with("kvn-tui "), "version string should start with 'kvn-tui ': got {}", s);
        let expected = format!("kvn-tui {}", env!("CARGO_PKG_VERSION"));
        assert_eq!(s, expected);
    }

    #[test]
    fn version_flag_uppercase_v() {
        assert!(should_show_version(["-V"]));
    }

    #[test]
    fn version_flag_long() {
        assert!(should_show_version(["--version"]));
    }

    #[test]
    fn no_version_flag() {
        assert!(!should_show_version(["kvn-tui"]));
    }

    #[test]
    fn empty_args() {
        assert!(!should_show_version(Vec::<&str>::new()));
    }

    #[test]
    fn help_flag_is_not_version() {
        assert!(!should_show_version(["kvn-tui", "--help"]));
    }

    #[test]
    fn waybar_status_flag_detected() {
        assert!(should_show_waybar_status(["kvn-tui", "--waybar-status"]));
    }


}
