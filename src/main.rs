#![warn(unsafe_code)]
// Tests touch `std::env::set_var` (Rust 2024 unsafe) and other test-only
// shims that are inherently `unsafe`. These are scoped through
// `test_helpers::ENV_LOCK` and don't need to fight the lint.
#![cfg_attr(test, allow(unsafe_code))]

mod app;
mod atomic_write;
mod cli;
mod config;
mod daemon;
mod doctor;
mod geo;
mod ipc;
mod omarchy;
mod paths;
mod services;
mod singbox;
mod tui_client;
mod ui;

#[cfg(test)]
mod test_helpers;

use anyhow::{Context, Result};
use clap::Parser;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

use crate::app::model::Model;
use crate::paths::ensure_config_dirs;

/// Entry point for the TUI VPN client.
fn main() -> Result<()> {
    let cli = cli::Cli::parse();

    if let Some(result) = cli::try_run_from_parsed(&cli) {
        return result;
    }

    // Ensure configuration directories exist before reading the config so
    // logging can be initialized from `settings.logs.level`.
    ensure_config_dirs()?;

    let cfg = crate::config::load_config().unwrap_or_default();
    let (filter, bad_level) = resolve_log_filter(cfg.settings.logs.level.as_str());
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().without_time())
        .init();
    if let Some(invalid) = bad_level {
        tracing::warn!(
            "settings.logs.level={:?} is not one of trace/debug/info/warn/error; using info",
            invalid
        );
    }

    if cli.daemon {
        let model = Model::new()?;
        daemon::run(model)?;
    } else {
        if !ipc::is_daemon_running() {
            start_daemon()?;
            if !ipc::wait_for_daemon(std::time::Duration::from_millis(2000)) {
                anyhow::bail!("daemon failed to start within 2s");
            }
        }
        tui_client::run()?;
    }

    Ok(())
}

/// Resolve the tracing filter from environment and config. Precedence:
/// `RUST_LOG` > `settings.logs.level` (if it parses as one of the five
/// canonical levels) > `info`. Returns the bad value as the second tuple
/// element so the caller can warn after the subscriber is live.
fn resolve_log_filter(level: &str) -> (EnvFilter, Option<String>) {
    if let Ok(filter) = EnvFilter::try_from_default_env() {
        return (filter, None);
    }
    let normalized = crate::config::profile::normalized_log_level(level);
    if normalized == level {
        (EnvFilter::new(normalized), None)
    } else {
        (EnvFilter::new(normalized), Some(level.to_string()))
    }
}

/// Start the packaged systemd user service when available. Source builds and
/// manual installations may not have the unit, so retain the detached-process
/// fallback that lets the TUI remain self-contained.
fn start_daemon() -> Result<()> {
    use std::process::{Command, Stdio};

    let systemd_status = Command::new("systemctl")
        .args(["--user", "start", "kvn-tui.service"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    if matches!(systemd_status, Ok(status) if status.success()) {
        return Ok(());
    }

    spawn_daemon_process()
}

/// Re-exec ourselves as `kvn-tui --daemon` in a fresh process group so the
/// daemon outlives the TUI when no packaged systemd unit is available.
fn spawn_daemon_process() -> Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let exe = std::env::current_exe().context("Failed to determine current executable path")?;
    Command::new(exe)
        .arg("--daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .context("Failed to spawn daemon process")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::resolve_log_filter;

    fn with_no_rust_log<F: FnOnce() -> R, R>(f: F) -> R {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let previous = std::env::var_os("RUST_LOG");
        unsafe { std::env::remove_var("RUST_LOG") };
        let result = f();
        match previous {
            Some(v) => unsafe { std::env::set_var("RUST_LOG", v) },
            None => unsafe { std::env::remove_var("RUST_LOG") },
        }
        result
    }

    #[test]
    fn resolve_log_filter_accepts_five_levels() {
        with_no_rust_log(|| {
            for level in ["trace", "debug", "info", "warn", "error"] {
                let (_filter, bad) = resolve_log_filter(level);
                assert!(bad.is_none(), "level {level} should be accepted");
            }
        });
    }

    #[test]
    fn resolve_log_filter_falls_back_on_garbage() {
        with_no_rust_log(|| {
            for bad in ["verbose", "", "INFO ", "kvn_tui=debug"] {
                let (_filter, reported) = resolve_log_filter(bad);
                assert_eq!(reported.as_deref(), Some(bad));
            }
        });
    }

    #[test]
    fn resolve_log_filter_env_overrides_config() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let previous = std::env::var_os("RUST_LOG");
        unsafe { std::env::set_var("RUST_LOG", "warn") };
        let (_filter, bad) = resolve_log_filter("debug");
        assert!(bad.is_none());
        match previous {
            Some(v) => unsafe { std::env::set_var("RUST_LOG", v) },
            None => unsafe { std::env::remove_var("RUST_LOG") },
        }
    }
}
