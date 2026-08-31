use anyhow::{Context, Result};
use clap::{ArgGroup, Parser, Subcommand};
use std::time::Duration;
use uuid::Uuid;

use crate::app::model::ConnectionState;
use crate::app::msg::{IpcCommand, StateSnapshot};
use crate::ipc::IpcClient;
use crate::services::waybar;

/// How long a one-shot CLI client waits for the daemon to answer with a
/// state snapshot before giving up.
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Parser)]
#[command(version, about)]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[arg(
        long,
        help = "Print connection status as JSON for status-bar integrations"
    )]
    waybar_status: bool,

    #[arg(long, help = "Run the headless daemon that manages sing-box")]
    pub daemon: bool,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Check whether kvn-tui and its runtime dependencies are ready.
    Doctor,

    /// Show the daemon's current status as a summary line.
    Status {
        /// Print the full state snapshot as JSON instead of a summary.
        #[arg(long)]
        json: bool,
    },

    /// Connect to a profile (by UUID, exact name, or unique name prefix).
    Connect {
        /// Profile UUID or name.
        profile: String,
    },

    /// Disconnect the active VPN tunnel.
    Disconnect,

    /// Reconnect the active profile.
    Reconnect,

    /// Connect the last-used profile, or disconnect when connected.
    Toggle,

    /// Set up one or more optional kvn-tui integrations.
    #[command(group(
        ArgGroup::new("targets")
            .required(true)
            .multiple(true)
            .args(["omarchy", "polkit", "killswitch"])
    ))]
    Setup {
        /// Set up Omarchy Shell/Waybar, launcher, and Hyprland integration.
        #[arg(long)]
        omarchy: bool,

        /// Set up polkit access for passwordless DNS management.
        #[arg(long)]
        polkit: bool,

        /// Set up the nftables-based kill switch.
        #[arg(long)]
        killswitch: bool,
    },

    /// Remove files left behind by optional integration setup.
    #[command(group(
        ArgGroup::new("targets")
            .required(true)
            .multiple(true)
            .args(["omarchy", "polkit", "killswitch"])
    ))]
    Clean {
        /// Remove backup files created by `setup --omarchy`.
        #[arg(long, conflicts_with_all = ["polkit", "killswitch"])]
        omarchy: bool,

        /// Remove the optional passwordless DNS polkit rule.
        #[arg(long)]
        polkit: bool,

        /// Disable and remove the nftables-based kill switch.
        #[arg(long)]
        killswitch: bool,
    },
}

/// Run a trusted, compile-time embedded shell script without materializing it
/// in a predictable temporary file. `bash -c` preserves the caller's stdin,
/// which the interactive Omarchy installer needs for its prompts.
fn run_embedded_script(name: &str, script: &str, args: &[&str]) -> Result<()> {
    let status = std::process::Command::new("bash")
        .arg("-c")
        .arg(script)
        .arg(name)
        .args(args)
        .status()
        .with_context(|| format!("failed to run {name}"))?;
    if !status.success() {
        anyhow::bail!("{name} exited with status {status}");
    }
    Ok(())
}

/// Run the embedded Omarchy integration installer script.
fn install_omarchy() -> Result<()> {
    run_embedded_script(
        "setup-omarchy.sh",
        include_str!("../contrib/setup-omarchy.sh"),
        &[],
    )
}

/// Run the embedded Omarchy integration cleanup script.
fn clean_omarchy() -> Result<()> {
    run_embedded_script(
        "clean-omarchy.sh",
        include_str!("../contrib/clean-omarchy.sh"),
        &[],
    )
}

/// Run the embedded polkit rule installer script.
fn install_polkit() -> Result<()> {
    run_embedded_script(
        "setup-polkit.sh",
        include_str!("../contrib/setup-polkit.sh"),
        &[],
    )
}

/// Run the embedded kill switch installer script.
fn install_killswitch() -> Result<()> {
    run_embedded_script(
        "setup-killswitch.sh",
        include_str!("../contrib/setup-killswitch.sh"),
        &[include_str!("../contrib/killswitch-helper.sh")],
    )
}

fn clean_polkit() -> Result<()> {
    run_embedded_script(
        "clean-polkit.sh",
        include_str!("../contrib/clean-polkit.sh"),
        &[],
    )
}

fn clean_killswitch() -> Result<()> {
    run_embedded_script(
        "clean-killswitch.sh",
        include_str!("../contrib/clean-killswitch.sh"),
        &[],
    )
}

// ---- one-shot IPC clients (CLI + Omarchy bar module backends) ----

/// Connect to the daemon without starting it. For read-only commands where
/// silently spawning a daemon would be surprising.
fn attach_client() -> Result<IpcClient> {
    IpcClient::connect().context(
        "Cannot reach the kvn-tui daemon. Start it with `kvn-tui` or \
         `systemctl --user start kvn-tui.service`.",
    )
}

/// Connect to the daemon, auto-starting it first when it is not running
/// (same startup path as launching the TUI).
fn attach_or_start_client() -> Result<IpcClient> {
    if !crate::ipc::is_daemon_running() {
        crate::start_daemon()?;
        if !crate::ipc::wait_for_daemon(Duration::from_millis(2000)) {
            anyhow::bail!("daemon failed to start within 2s");
        }
    }
    attach_client()
}

fn fetch_snapshot(client: &mut IpcClient) -> Result<StateSnapshot> {
    client.send(&IpcCommand::Attach)?;
    client.read_snapshot(SNAPSHOT_TIMEOUT)
}

/// Send a command and return the snapshot the daemon broadcasts in response.
fn send_command(client: &mut IpcClient, cmd: IpcCommand) -> Result<StateSnapshot> {
    client.send(&cmd)?;
    client.read_snapshot(SNAPSHOT_TIMEOUT)
}

fn run_status(json: bool) -> Result<()> {
    let mut client = attach_client()?;
    let snap = fetch_snapshot(&mut client)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&snap)?);
    } else {
        println!("{}", format_status_line(&snap));
        if snap.status_is_error {
            eprintln!("last error: {}", snap.status);
        }
    }
    Ok(())
}

fn run_connect(query: &str) -> Result<()> {
    let mut client = attach_or_start_client()?;
    let snap = fetch_snapshot(&mut client)?;
    let id = resolve_profile(&snap, query).with_context(|| {
        format!(
            "no profile matches '{query}' ({} profiles configured)",
            snap.profiles.len()
        )
    })?;
    let snap = send_command(&mut client, IpcCommand::ConnectProfile { profile_id: id })?;
    println!("{}", format_status_line(&snap));
    Ok(())
}

fn run_disconnect() -> Result<()> {
    let mut client = attach_or_start_client()?;
    fetch_snapshot(&mut client)?;
    let snap = send_command(&mut client, IpcCommand::Disconnect)?;
    println!("{}", format_status_line(&snap));
    Ok(())
}

fn run_reconnect() -> Result<()> {
    let mut client = attach_or_start_client()?;
    fetch_snapshot(&mut client)?;
    let snap = send_command(&mut client, IpcCommand::Reconnect)?;
    println!("{}", format_status_line(&snap));
    Ok(())
}

fn run_toggle() -> Result<()> {
    let mut client = attach_or_start_client()?;
    let snap = fetch_snapshot(&mut client)?;
    if snap.connection == ConnectionState::Connected {
        let snap = send_command(&mut client, IpcCommand::Disconnect)?;
        println!("{}", format_status_line(&snap));
        return Ok(());
    }
    let Some(id) = snap.settings.last_connected_profile else {
        anyhow::bail!("no previous profile to connect — run `kvn-tui connect <name>` first");
    };
    let snap = send_command(&mut client, IpcCommand::ConnectProfile { profile_id: id })?;
    println!("{}", format_status_line(&snap));
    Ok(())
}

/// Resolve a profile query: exact UUID, exact (case-insensitive) name, or
/// unique case-insensitive name prefix. Ambiguous prefixes resolve to none.
fn resolve_profile(snap: &StateSnapshot, query: &str) -> Option<Uuid> {
    if let Ok(id) = Uuid::parse_str(query)
        && snap.profiles.iter().any(|p| p.id == id)
    {
        return Some(id);
    }
    let lower = query.to_lowercase();
    let exact: Vec<_> = snap
        .profiles
        .iter()
        .filter(|p| p.name.to_lowercase() == lower)
        .collect();
    if exact.len() == 1 {
        return Some(exact[0].id);
    }
    let prefix: Vec<_> = snap
        .profiles
        .iter()
        .filter(|p| p.name.to_lowercase().starts_with(&lower))
        .collect();
    match prefix.len() {
        1 => Some(prefix[0].id),
        _ => None,
    }
}

fn active_profile_name(snap: &StateSnapshot) -> Option<&str> {
    let id = snap.active_profile_id.as_deref()?;
    snap.profiles
        .iter()
        .find(|p| p.id.to_string() == id)
        .map(|p| p.name.as_str())
}

/// One-line human summary of a snapshot, e.g.
/// `connected to Work VPN (↑ 1.2 MiB/s · ↓ 3.4 MiB/s) [kill switch]`.
fn format_status_line(snap: &StateSnapshot) -> String {
    match snap.connection {
        ConnectionState::Connected => {
            let name = active_profile_name(snap).unwrap_or("unknown profile");
            let mut line = format!("connected to {name}");
            if snap.traffic.up_rate_bps > 0 || snap.traffic.down_rate_bps > 0 {
                line.push_str(&format!(
                    " (↑ {}/s · ↓ {}/s)",
                    format_rate(snap.traffic.up_rate_bps),
                    format_rate(snap.traffic.down_rate_bps)
                ));
            }
            if snap.settings.kill_switch {
                line.push_str(" [kill switch]");
            }
            line
        }
        ConnectionState::Connecting | ConnectionState::ConnectPending => {
            format!("connecting — {}", snap.status)
        }
        ConnectionState::Idle => {
            if snap.status_is_error {
                format!("disconnected — {}", snap.status)
            } else {
                "disconnected".to_string()
            }
        }
    }
}

/// Human-readable byte rate (input is bytes per second).
fn format_rate(bytes_per_sec: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes_per_sec as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes_per_sec, UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
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
    match &cli.command {
        Some(Command::Doctor) => return Some(crate::doctor::run()),
        Some(Command::Status { json }) => return Some(run_status(*json)),
        Some(Command::Connect { profile }) => return Some(run_connect(profile)),
        Some(Command::Disconnect) => return Some(run_disconnect()),
        Some(Command::Reconnect) => return Some(run_reconnect()),
        Some(Command::Toggle) => return Some(run_toggle()),
        Some(Command::Setup {
            omarchy,
            polkit,
            killswitch,
        }) => {
            let result = (|| {
                if *omarchy {
                    install_omarchy()?;
                }
                if *polkit {
                    install_polkit()?;
                }
                if *killswitch {
                    install_killswitch()?;
                }
                Ok(())
            })();
            return Some(result);
        }
        Some(Command::Clean {
            omarchy,
            polkit,
            killswitch,
        }) => {
            let result = (|| {
                if *omarchy {
                    clean_omarchy()?;
                }
                // Stop and remove the firewall before removing the shared
                // group's remaining polkit authorization.
                if *killswitch {
                    clean_killswitch()?;
                }
                if *polkit {
                    clean_polkit()?;
                }
                Ok(())
            })();
            return Some(result);
        }
        None => {}
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
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command as ProcessCommand, Stdio};
    use tempfile::TempDir;

    fn write_executable(path: &Path, contents: &str) {
        fs::write(path, contents).unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }

    fn installer_fixture(version: u8) -> (TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let bin = root.path().join("bin");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&bin).unwrap();
        let omarchy_stub = r#"#!/bin/bash
case "${1:-}:${2:-}" in
version:)
  echo '@VERSION@.0.0-1'
  ;;
plugin:add)
  [[ ${3:-} == --help ]] && exit 0
  target="$HOME/.config/omarchy/plugins/yarikov.omakvn"
  mkdir -p "$target"
  git -C "$target" init -q
  git -C "$target" remote add origin "${3:-}"
  printf '%s\n' '{"schemaVersion":1,"id":"yarikov.omakvn","name":"kvn-tui VPN","version":"1.0.0","kinds":["bar-widget"],"entryPoints":{"barWidget":"Widget.qml"}}' >"$target/manifest.json"
  printf '%s\n' 'import QtQuick' >"$target/Widget.qml"
  printf '%s\n' 'import QtQuick' >"$target/KvnService.qml"
  ;;
plugin:update|plugin:validate|plugin:list|bar:put)
  exit 0
  ;;
esac
"#
        .replace("@VERSION@", &version.to_string());
        write_executable(&bin.join("omarchy"), &omarchy_stub);
        write_executable(
            &bin.join("hyprctl"),
            "#!/bin/bash\ncase ${1:-} in configerrors) exit 0;; reload) exit 0;; esac\n",
        );
        write_executable(&bin.join("pgrep"), "#!/bin/bash\nexit 0\n");
        write_executable(&bin.join("sleep"), "#!/bin/bash\nexit 0\n");
        // Avoid sending rescanPlugins IPC to the user's live Omarchy Shell.
        write_executable(&bin.join("omarchy-shell"), "#!/bin/bash\nexit 0\n");
        (root, home)
    }

    fn run_installer(root: &TempDir, home: &Path, input: &str) -> std::process::Output {
        let _lock = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let script = root.path().join("setup-omarchy.sh");
        fs::write(&script, include_str!("../contrib/setup-omarchy.sh")).unwrap();
        let path = format!(
            "{}:{}",
            root.path().join("bin").display(),
            std::env::var("PATH").unwrap()
        );
        let mut child = ProcessCommand::new("bash")
            .arg(&script)
            .env("HOME", home)
            .env("PATH", path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    fn run_omarchy_cleanup(root: &TempDir, home: &Path) -> std::process::Output {
        let _lock = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let script = root.path().join("clean-omarchy.sh");
        fs::write(&script, include_str!("../contrib/clean-omarchy.sh")).unwrap();
        let path = format!(
            "{}:{}",
            root.path().join("bin").display(),
            std::env::var("PATH").unwrap()
        );
        ProcessCommand::new("bash")
            .arg(&script)
            .env("HOME", home)
            .env("PATH", path)
            .output()
            .unwrap()
    }

    fn backup_files(path: &Path) -> Vec<PathBuf> {
        let file_name = path.file_name().unwrap().to_string_lossy();
        let prefix = format!("{file_name}.bak.before-kvn-tui");
        let mut backups = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                (name == prefix || name.starts_with(&format!("{prefix}."))).then_some(entry.path())
            })
            .collect::<Vec<_>>();
        backups.sort();
        backups
    }

    fn assert_success(output: &std::process::Output) {
        assert!(
            output.status.success(),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn write_omarchy_v4_config(home: &Path) {
        let omarchy = home.join(".config/omarchy");
        let hypr = home.join(".config/hypr");
        fs::create_dir_all(&omarchy).unwrap();
        fs::create_dir_all(&hypr).unwrap();
        fs::write(
            omarchy.join("shell.json"),
            r#"{"version":1,"bar":{"layout":{"left":[],"center":[],"right":[{"id":"omarchy.tray"},{"id":"omarchy.bluetooth"},{"id":"omarchy.network"}]}}}"#,
        )
        .unwrap();
        fs::write(hypr.join("bindings.lua"), "-- personal bindings\n").unwrap();
        fs::write(hypr.join("hyprland.lua"), "-- personal rules\n").unwrap();
    }

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
    fn setup_omarchy_option_detected() {
        let cli = Cli::parse_from(["kvn-tui", "setup", "--omarchy"]);
        assert!(matches!(
            cli.command,
            Some(Command::Setup {
                omarchy: true,
                polkit: false,
                killswitch: false,
            })
        ));
    }

    #[test]
    fn setup_polkit_option_detected() {
        let cli = Cli::parse_from(["kvn-tui", "setup", "--polkit"]);
        assert!(matches!(
            cli.command,
            Some(Command::Setup {
                omarchy: false,
                polkit: true,
                killswitch: false,
            })
        ));
    }

    #[test]
    fn setup_killswitch_option_detected() {
        let cli = Cli::parse_from(["kvn-tui", "setup", "--killswitch"]);
        assert!(matches!(
            cli.command,
            Some(Command::Setup {
                omarchy: false,
                polkit: false,
                killswitch: true,
            })
        ));
    }

    #[test]
    fn setup_options_can_be_combined() {
        let cli = Cli::parse_from(["kvn-tui", "setup", "--omarchy", "--polkit", "--killswitch"]);
        assert!(matches!(
            cli.command,
            Some(Command::Setup {
                omarchy: true,
                polkit: true,
                killswitch: true,
            })
        ));
    }

    #[test]
    fn setup_requires_at_least_one_option() {
        assert!(Cli::try_parse_from(["kvn-tui", "setup"]).is_err());
    }

    #[test]
    fn clean_omarchy_option_detected() {
        let cli = Cli::parse_from(["kvn-tui", "clean", "--omarchy"]);
        assert!(matches!(
            cli.command,
            Some(Command::Clean {
                omarchy: true,
                polkit: false,
                killswitch: false,
            })
        ));
    }

    #[test]
    fn clean_system_options_can_be_combined() {
        let cli = Cli::parse_from(["kvn-tui", "clean", "--polkit", "--killswitch"]);
        assert!(matches!(
            cli.command,
            Some(Command::Clean {
                omarchy: false,
                polkit: true,
                killswitch: true,
            })
        ));
    }

    #[test]
    fn clean_omarchy_conflicts_with_system_cleanup() {
        assert!(Cli::try_parse_from(["kvn-tui", "clean", "--omarchy", "--polkit"]).is_err());
        assert!(Cli::try_parse_from(["kvn-tui", "clean", "--omarchy", "--killswitch"]).is_err());
    }

    #[test]
    fn clean_requires_at_least_one_option() {
        assert!(Cli::try_parse_from(["kvn-tui", "clean"]).is_err());
    }

    #[test]
    fn embedded_system_scripts_have_valid_bash_syntax() {
        let _lock = crate::test_helpers::ENV_LOCK.lock().unwrap();
        for (name, script) in [
            ("setup-polkit", include_str!("../contrib/setup-polkit.sh")),
            (
                "setup-killswitch",
                include_str!("../contrib/setup-killswitch.sh"),
            ),
            ("clean-polkit", include_str!("../contrib/clean-polkit.sh")),
            (
                "clean-killswitch",
                include_str!("../contrib/clean-killswitch.sh"),
            ),
            (
                "killswitch-helper",
                include_str!("../contrib/killswitch-helper.sh"),
            ),
        ] {
            let status = ProcessCommand::new("bash")
                .args(["-n", "-c", script])
                .status()
                .unwrap();
            assert!(status.success(), "invalid bash syntax in {name}");
        }
    }

    #[test]
    fn polkit_rule_is_narrow_and_uses_dedicated_group() {
        let script = include_str!("../contrib/setup-polkit.sh");
        for action in [
            "org.freedesktop.resolve1.set-dns-servers",
            "org.freedesktop.resolve1.set-domains",
            "org.freedesktop.resolve1.set-default-route",
        ] {
            assert!(script.contains(action));
        }
        assert!(script.contains("subject.isInGroup(\"kvn-tui\")"));
        assert!(!script.contains("org.freedesktop.NetworkManager"));
        assert!(!script.contains("subject.isInGroup(\"network\")"));
    }

    #[test]
    fn killswitch_sudoers_uses_dedicated_group() {
        let script = include_str!("../contrib/setup-killswitch.sh");
        assert!(script.contains("%kvn-tui ALL=(root) NOPASSWD:"));
        assert!(!script.contains("%network ALL=(root) NOPASSWD:"));
    }

    fn helper_accepts_allow_args(args: &[&str]) -> bool {
        let _lock = crate::test_helpers::ENV_LOCK.lock().unwrap();
        ProcessCommand::new("bash")
            .args([
                "-c",
                "source \"$1\"; shift; validate_allow_args \"$@\"",
                "helper-test",
                concat!(env!("CARGO_MANIFEST_DIR"), "/contrib/killswitch-helper.sh"),
            ])
            .args(args)
            .status()
            .unwrap()
            .success()
    }

    fn helper_accepts_command_args(args: &[&str]) -> bool {
        let _lock = crate::test_helpers::ENV_LOCK.lock().unwrap();
        ProcessCommand::new("bash")
            .args([
                "-c",
                "source \"$1\"; shift; validate_command_args \"$@\"",
                "helper-test",
                concat!(env!("CARGO_MANIFEST_DIR"), "/contrib/killswitch-helper.sh"),
            ])
            .args(args)
            .status()
            .unwrap()
            .success()
    }

    #[test]
    fn killswitch_helper_enforces_the_command_surface() {
        for args in [
            &["check"][..],
            &["enable"][..],
            &["disable"][..],
            &["revoke"][..],
            &["allow", "192.0.2.1", "tcp", "443"][..],
        ] {
            assert!(helper_accepts_command_args(args), "rejected {args:?}");
        }
        for args in [
            &[][..],
            &["unknown"][..],
            &["check", "extra"][..],
            &["enable", "extra"][..],
            &["disable", "extra"][..],
            &["revoke", "extra"][..],
            &["allow", "192.0.2.1", "tcp", "443", "extra"][..],
        ] {
            assert!(!helper_accepts_command_args(args), "accepted {args:?}");
        }
    }

    #[test]
    fn killswitch_helper_validates_allow_arguments() {
        for args in [
            &["192.0.2.1", "tcp", "443"][..],
            &["255.255.255.255", "udp", "65535"][..],
            &["2001:db8::1", "tcp", "1"][..],
            &["::1", "udp", "53"][..],
        ] {
            assert!(helper_accepts_allow_args(args), "rejected {args:?}");
        }

        for args in [
            &[][..],
            &["192.0.2.1", "tcp"][..],
            &["192.0.2.1", "tcp", "443", "extra"][..],
            &["256.1.1.1", "tcp", "443"][..],
            &["::::", "tcp", "443"][..],
            &["example.com", "tcp", "443"][..],
            &["192.0.2.1;id", "tcp", "443"][..],
            &["192.0.2.1", "sctp", "443"][..],
            &["192.0.2.1", "tcp", "0"][..],
            &["192.0.2.1", "tcp", "65536"][..],
            &["192.0.2.1", "tcp", "$(id)"][..],
            &["192.0.2.1\n127.0.0.1", "tcp", "443"][..],
        ] {
            assert!(!helper_accepts_allow_args(args), "accepted {args:?}");
        }
    }

    #[test]
    fn daemon_flag_detected() {
        let cli = Cli::parse_from(["kvn-tui", "--daemon"]);
        assert!(cli.daemon);
    }

    #[test]
    fn doctor_subcommand_detected() {
        let cli = Cli::parse_from(["kvn-tui", "doctor"]);
        assert!(matches!(cli.command, Some(Command::Doctor)));
    }

    #[test]
    fn ipc_subcommands_detected() {
        let cli = Cli::parse_from(["kvn-tui", "status", "--json"]);
        assert!(matches!(cli.command, Some(Command::Status { json: true })));
        let cli = Cli::parse_from(["kvn-tui", "status"]);
        assert!(matches!(cli.command, Some(Command::Status { json: false })));
        let cli = Cli::parse_from(["kvn-tui", "connect", "Work"]);
        assert!(matches!(
            cli.command,
            Some(Command::Connect { profile }) if profile == "Work"
        ));
        assert!(matches!(
            Cli::parse_from(["kvn-tui", "disconnect"]).command,
            Some(Command::Disconnect)
        ));
        assert!(matches!(
            Cli::parse_from(["kvn-tui", "reconnect"]).command,
            Some(Command::Reconnect)
        ));
        assert!(matches!(
            Cli::parse_from(["kvn-tui", "toggle"]).command,
            Some(Command::Toggle)
        ));
    }

    fn snapshot_with_profiles() -> StateSnapshot {
        let mut snap = StateSnapshot {
            connection: ConnectionState::Idle,
            status: "ok".into(),
            status_is_error: false,
            singbox_pid: None,
            active_profile_id: None,
            selected: 0,
            routing_selected: 0,
            geo_region_selected: 0,
            dns_selected: 0,
            dns_strategy_draft: None,
            dns_fakeip_draft: None,
            theme_selected: 0,
            theme_draft: None,
            service_routing_selected: 0,
            service_routing_draft: None,
            geo_updating: false,
            geo_last_updated: None,
            overlay: crate::app::model::Overlay::None,
            main_pane_focus: Default::default(),
            profiles: vec![
                crate::config::profile::Profile::new_vless(
                    "Work VPN".into(),
                    "1.1.1.1".into(),
                    443,
                    "u1".into(),
                ),
                crate::config::profile::Profile::new_vless(
                    "Home".into(),
                    "2.2.2.2".into(),
                    443,
                    "u2".into(),
                ),
            ],
            subscriptions: vec![],
            settings: crate::config::profile::Settings::default(),
            traffic: Default::default(),
            log_session_offsets: None,
            profile_latencies: Default::default(),
            testing_profiles: Default::default(),
        };
        snap.settings.last_connected_profile = Some(snap.profiles[0].id);
        snap
    }

    #[test]
    fn resolve_profile_by_uuid_exact_and_prefix() {
        let snap = snapshot_with_profiles();
        let work_id = snap.profiles[0].id;

        assert_eq!(resolve_profile(&snap, &work_id.to_string()), Some(work_id));
        assert_eq!(resolve_profile(&snap, "work"), Some(work_id));
        assert_eq!(resolve_profile(&snap, "WORK VPN"), Some(work_id));
        assert_eq!(resolve_profile(&snap, "home"), Some(snap.profiles[1].id));
        // Ambiguous / unknown.
        assert_eq!(resolve_profile(&snap, "nope"), None);
        assert_eq!(resolve_profile(&snap, &Uuid::new_v4().to_string()), None);
    }

    #[test]
    fn format_status_line_variants() {
        let mut snap = snapshot_with_profiles();
        assert_eq!(format_status_line(&snap), "disconnected");

        snap.status = "Connect failed: timeout".into();
        snap.status_is_error = true;
        assert_eq!(
            format_status_line(&snap),
            "disconnected — Connect failed: timeout"
        );

        snap.status_is_error = false;
        snap.status = "Connecting to Work VPN…".into();
        snap.connection = ConnectionState::Connecting;
        assert_eq!(
            format_status_line(&snap),
            "connecting — Connecting to Work VPN…"
        );

        snap.connection = ConnectionState::Connected;
        snap.active_profile_id = Some(snap.profiles[0].id.to_string());
        assert_eq!(format_status_line(&snap), "connected to Work VPN");

        snap.traffic.up_rate_bps = 1536;
        snap.traffic.down_rate_bps = 5 * 1024 * 1024;
        assert_eq!(
            format_status_line(&snap),
            "connected to Work VPN (↑ 1.5 KiB/s · ↓ 5.0 MiB/s)"
        );

        snap.settings.kill_switch = true;
        assert!(format_status_line(&snap).ends_with(" [kill switch]"));
    }

    #[test]
    fn format_rate_units() {
        assert_eq!(format_rate(0), "0 B");
        assert_eq!(format_rate(999), "999 B");
        assert_eq!(format_rate(1024), "1.0 KiB");
        assert_eq!(format_rate(1024 * 1024), "1.0 MiB");
        assert_eq!(format_rate(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }

    #[test]
    fn omarchy_v4_installer_updates_shell_and_lua_idempotently() {
        let (root, home) = installer_fixture(4);
        let omarchy = home.join(".config/omarchy");
        let hypr = home.join(".config/hypr");
        write_omarchy_v4_config(&home);

        assert_success(&run_installer(&root, &home, "y\n\n"));
        assert_success(&run_installer(&root, &home, ""));

        let shell: serde_json::Value =
            serde_json::from_slice(&fs::read(omarchy.join("shell.json")).unwrap()).unwrap();
        let right = shell["bar"]["layout"]["right"].as_array().unwrap();
        assert_eq!(
            right
                .iter()
                .filter(|entry| entry["id"] == "yarikov.omakvn")
                .count(),
            1
        );
        // The legacy command-module entry must be gone.
        assert_eq!(
            right
                .iter()
                .filter(|entry| entry["id"] == "kvn-tui")
                .count(),
            0
        );
        let kvn_index = right
            .iter()
            .position(|entry| entry["id"] == "yarikov.omakvn")
            .unwrap();
        let bluetooth_index = right
            .iter()
            .position(|entry| entry["id"] == "omarchy.bluetooth")
            .unwrap();
        assert_eq!(kvn_index + 1, bluetooth_index);
        // A plugin entry carries no command-module fields.
        assert!(right[kvn_index].get("exec").is_none());
        assert!(right[kvn_index].get("type").is_none());

        // Plugin files are installed.
        let plugin = home.join(".config/omarchy/plugins/yarikov.omakvn");
        for file in ["manifest.json", "Widget.qml", "KvnService.qml"] {
            assert!(plugin.join(file).is_file(), "missing {file}");
        }
        let manifest = fs::read_to_string(plugin.join("manifest.json")).unwrap();
        assert!(manifest.contains("\"yarikov.omakvn\""));

        let bindings = fs::read_to_string(hypr.join("bindings.lua")).unwrap();
        assert_eq!(bindings.matches("-- kvn-tui keybinding: begin").count(), 1);
        assert!(bindings.contains(r#"hl.unbind("SUPER + CTRL + K")"#));
        assert!(bindings.contains("omarchy-launch-kvn-tui"));
        let rules = fs::read_to_string(hypr.join("hyprland.lua")).unwrap();
        assert_eq!(rules.matches("-- kvn-tui window rule: begin").count(), 1);
        assert!(rules.contains(r#"o.window("^org\\.omarchy\\.kvn-tui$""#));

        let launcher = fs::read_to_string(home.join(".local/bin/omarchy-launch-kvn-tui")).unwrap();
        assert!(launcher.contains("omarchy/plugins/yarikov.omakvn"));
        assert!(launcher.contains("$plugin_dir/manifest.json"));
        assert!(launcher.contains("omarchy-shell shell summon yarikov.omakvn '{}'"));
        assert!(launcher.contains("omarchy-launch-or-focus-tui"));
        assert!(launcher.contains("--app-id=org.omarchy.kvn-tui"));
        let desktop_entry =
            fs::read_to_string(home.join(".local/share/applications/kvn-tui.desktop")).unwrap();
        for expected in [
            "Type=Application",
            "Name=kvn-tui",
            "GenericName=VPN Client",
            "Exec=omarchy-launch-or-focus-tui --app-id=org.omarchy.kvn-tui kvn-tui",
            "TryExec=kvn-tui",
            "Terminal=false",
            "Icon=kvn-tui",
            "Categories=Network;Utility;",
            "Keywords=VPN;TUI;Terminal;sing-box;",
        ] {
            assert!(desktop_entry.contains(expected), "missing {expected}");
        }
        let app_icon =
            fs::read_to_string(home.join(".local/share/icons/hicolor/scalable/apps/kvn-tui.svg"))
                .unwrap();
        for expected in [
            r#"viewBox="0 0 128 128""#,
            r##"fill="#101010""##,
            r##"fill="#f5f1e8""##,
        ] {
            assert!(app_icon.contains(expected), "missing {expected}");
        }
        let shell_backups = backup_files(&omarchy.join("shell.json"));
        assert_eq!(shell_backups.len(), 1);
        assert!(
            shell_backups[0]
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("shell.json.bak.before-kvn-tui.")
        );
        for file in ["bindings.lua", "hyprland.lua"] {
            assert_eq!(backup_files(&hypr.join(file)).len(), 1);
        }
    }

    #[test]
    fn omarchy_v4_installer_falls_back_to_command_module_without_plugin_registry() {
        let (root, home) = installer_fixture(4);
        // Simulate an Omarchy 4 build without the shell plugin registry.
        write_executable(
            &root.path().join("bin/omarchy"),
            "#!/bin/bash\ncase ${1:-} in version) echo '4.0.0-1';; plugin) exit 1;; esac\n",
        );
        write_omarchy_v4_config(&home);

        assert_success(&run_installer(&root, &home, "y\n\n"));

        let shell: serde_json::Value =
            serde_json::from_slice(&fs::read(home.join(".config/omarchy/shell.json")).unwrap())
                .unwrap();
        let right = shell["bar"]["layout"]["right"].as_array().unwrap();
        let entry = right
            .iter()
            .find(|entry| entry["id"] == "kvn-tui")
            .expect("legacy command module entry");
        assert_eq!(entry["exec"], "kvn-tui --waybar-status");
        assert!(!home.join(".config/omarchy/plugins/yarikov.omakvn").exists());
    }

    #[test]
    fn omarchy_v4_installer_upgrades_existing_tui_launcher() {
        let (root, home) = installer_fixture(4);
        write_omarchy_v4_config(&home);
        let launcher = home.join(".local/bin/omarchy-launch-kvn-tui");
        fs::create_dir_all(launcher.parent().unwrap()).unwrap();
        fs::write(
            &launcher,
            "#!/bin/bash\nexec omarchy-launch-or-focus-tui --app-id=org.omarchy.kvn-tui kvn-tui\n",
        )
        .unwrap();

        assert_success(&run_installer(&root, &home, "n\n"));

        let contents = fs::read_to_string(&launcher).unwrap();
        assert!(contents.contains("omarchy-shell shell summon yarikov.omakvn '{}'"));
        assert_eq!(backup_files(&launcher).len(), 1);
    }

    #[test]
    fn omarchy_v4_installer_upgrades_desktop_entry_idempotently() {
        let (root, home) = installer_fixture(4);
        write_omarchy_v4_config(&home);
        let desktop_entry = home.join(".local/share/applications/kvn-tui.desktop");
        let app_icon = home.join(".local/share/icons/hicolor/scalable/apps/kvn-tui.svg");
        fs::create_dir_all(desktop_entry.parent().unwrap()).unwrap();
        fs::create_dir_all(app_icon.parent().unwrap()).unwrap();
        fs::write(&desktop_entry, "[Desktop Entry]\nName=Old kvn-tui\n").unwrap();
        fs::write(&app_icon, "<svg>old</svg>\n").unwrap();

        assert_success(&run_installer(&root, &home, "y\n\n"));

        let contents = fs::read_to_string(&desktop_entry).unwrap();
        assert!(contents.contains("Name=kvn-tui"));
        assert!(contents.contains("Keywords=VPN;TUI;Terminal;sing-box;"));
        assert_eq!(backup_files(&desktop_entry).len(), 1);
        let icon_contents = fs::read_to_string(&app_icon).unwrap();
        assert!(icon_contents.contains(r##"fill="#f5f1e8""##));
        assert_eq!(backup_files(&app_icon).len(), 1);

        assert_success(&run_installer(&root, &home, ""));
        assert_eq!(backup_files(&desktop_entry).len(), 1);
        assert_eq!(backup_files(&app_icon).len(), 1);
    }

    #[test]
    fn omarchy_v4_installer_upgrades_command_module_to_plugin() {
        let (root, home) = installer_fixture(4);
        let shell_config = home.join(".config/omarchy/shell.json");
        write_omarchy_v4_config(&home);
        // Simulate the previous release's command module.
        let mut shell: serde_json::Value =
            serde_json::from_slice(&fs::read(&shell_config).unwrap()).unwrap();
        shell["bar"]["layout"]["right"]
            .as_array_mut()
            .unwrap()
            .insert(
                0,
                serde_json::json!({"id": "kvn-tui", "type": "command", "exec": "kvn-tui --waybar-status", "interval": 5, "onClick": "omarchy-launch-kvn-tui"}),
            );
        fs::write(&shell_config, serde_json::to_vec_pretty(&shell).unwrap()).unwrap();

        assert_success(&run_installer(&root, &home, "y\n\n"));

        let shell: serde_json::Value =
            serde_json::from_slice(&fs::read(&shell_config).unwrap()).unwrap();
        let ids: Vec<&str> = shell["bar"]["layout"]["right"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids.iter().filter(|id| **id == "yarikov.omakvn").count(), 1);
        assert!(!ids.contains(&"kvn-tui"));
    }

    #[test]
    fn omarchy_v4_installer_migrates_embedded_plugin_to_git_checkout() {
        let (root, home) = installer_fixture(4);
        write_omarchy_v4_config(&home);
        let legacy_plugin = home.join(".config/omarchy/plugins/kvn.tui");
        fs::create_dir_all(&legacy_plugin).unwrap();
        fs::write(legacy_plugin.join("manifest.json"), r#"{"id":"kvn.tui"}"#).unwrap();
        fs::write(legacy_plugin.join("Widget.qml"), "legacy").unwrap();

        assert_success(&run_installer(&root, &home, "\n"));

        let plugin = home.join(".config/omarchy/plugins/yarikov.omakvn");
        assert!(!legacy_plugin.exists());
        assert!(plugin.join(".git").is_dir());
        assert_eq!(
            ProcessCommand::new("git")
                .args([
                    "-C",
                    plugin.to_str().unwrap(),
                    "remote",
                    "get-url",
                    "origin"
                ])
                .output()
                .unwrap()
                .stdout,
            b"https://github.com/yarikov/omakvn.git\n"
        );
        assert_ne!(
            fs::read_to_string(plugin.join("Widget.qml")).unwrap(),
            "legacy"
        );
    }

    #[test]
    fn omarchy_v4_installer_restores_embedded_plugin_when_remote_install_fails() {
        let (root, home) = installer_fixture(4);
        write_omarchy_v4_config(&home);
        let plugin = home.join(".config/omarchy/plugins/kvn.tui");
        fs::create_dir_all(&plugin).unwrap();
        fs::write(plugin.join("manifest.json"), r#"{"id":"kvn.tui"}"#).unwrap();
        fs::write(plugin.join("Widget.qml"), "legacy").unwrap();
        write_executable(
            &root.path().join("bin/omarchy"),
            "#!/bin/bash\nif [[ ${1:-} == version ]]; then echo '4.0.0-1'; elif [[ ${1:-}:${2:-}:${3:-} == plugin:add:--help ]]; then exit 0; else exit 1; fi\n",
        );

        assert_success(&run_installer(&root, &home, "\n"));

        assert!(!plugin.join(".git").exists());
        assert_eq!(
            fs::read_to_string(plugin.join("Widget.qml")).unwrap(),
            "legacy"
        );
    }

    #[test]
    fn omarchy_v4_installer_refuses_conflicting_git_origin() {
        let (root, home) = installer_fixture(4);
        write_omarchy_v4_config(&home);
        let plugin = home.join(".config/omarchy/plugins/yarikov.omakvn");
        fs::create_dir_all(&plugin).unwrap();
        assert_success(
            &ProcessCommand::new("git")
                .args(["-C", plugin.to_str().unwrap(), "init", "-q"])
                .output()
                .unwrap(),
        );
        assert_success(
            &ProcessCommand::new("git")
                .args([
                    "-C",
                    plugin.to_str().unwrap(),
                    "remote",
                    "add",
                    "origin",
                    "https://example.com/not-omakvn.git",
                ])
                .output()
                .unwrap(),
        );

        let output = run_installer(&root, &home, "\n");
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("managed by a different Git repository")
        );
    }

    #[test]
    fn omarchy_v4_failure_removes_new_desktop_entry() {
        let (root, home) = installer_fixture(4);
        write_omarchy_v4_config(&home);
        write_executable(
            &root.path().join("bin/hyprctl"),
            "#!/bin/bash\ncase ${1:-} in\nconfigerrors) marker=$HOME/.hypr-errors-seen; if [[ -e $marker ]]; then echo 'new error'; else touch $marker; fi;;\nreload) exit 0;;\nesac\n",
        );
        let desktop_entry = home.join(".local/share/applications/kvn-tui.desktop");
        let app_icon = home.join(".local/share/icons/hicolor/scalable/apps/kvn-tui.svg");

        let output = run_installer(&root, &home, "n\n");

        assert!(!output.status.success());
        assert!(!desktop_entry.exists());
        assert!(!app_icon.exists());
    }

    #[test]
    fn omarchy_v4_failure_restores_existing_desktop_entry() {
        let (root, home) = installer_fixture(4);
        write_omarchy_v4_config(&home);
        write_executable(
            &root.path().join("bin/hyprctl"),
            "#!/bin/bash\ncase ${1:-} in\nconfigerrors) marker=$HOME/.hypr-errors-seen; if [[ -e $marker ]]; then echo 'new error'; else touch $marker; fi;;\nreload) exit 0;;\nesac\n",
        );
        let desktop_entry = home.join(".local/share/applications/kvn-tui.desktop");
        let app_icon = home.join(".local/share/icons/hicolor/scalable/apps/kvn-tui.svg");
        fs::create_dir_all(desktop_entry.parent().unwrap()).unwrap();
        fs::create_dir_all(app_icon.parent().unwrap()).unwrap();
        let original = "[Desktop Entry]\nName=Personal kvn-tui\n";
        let original_icon = "<svg>personal</svg>\n";
        fs::write(&desktop_entry, original).unwrap();
        fs::write(&app_icon, original_icon).unwrap();

        let output = run_installer(&root, &home, "n\n");

        assert!(!output.status.success());
        assert_eq!(fs::read_to_string(desktop_entry).unwrap(), original);
        assert_eq!(fs::read_to_string(app_icon).unwrap(), original_icon);
    }

    #[test]
    fn omarchy_installer_keeps_only_five_backups_per_changed_file() {
        let (root, home) = installer_fixture(4);
        let shell_config = home.join(".config/omarchy/shell.json");
        write_omarchy_v4_config(&home);
        let legacy = shell_config.with_file_name("shell.json.bak.before-kvn-tui");
        fs::write(&legacy, "legacy backup").unwrap();

        assert_success(&run_installer(&root, &home, "y\n\n"));
        assert!(legacy.is_file());
        assert_eq!(backup_files(&shell_config).len(), 2);

        for revision in 1..=5 {
            let mut shell: serde_json::Value =
                serde_json::from_slice(&fs::read(&shell_config).unwrap()).unwrap();
            shell["test_revision"] = revision.into();
            fs::write(&shell_config, serde_json::to_vec_pretty(&shell).unwrap()).unwrap();
            assert_success(&run_installer(&root, &home, ""));
        }

        let backups = backup_files(&shell_config);
        assert_eq!(backups.len(), 5);
        assert!(!legacy.exists());
        let contents = backups
            .iter()
            .map(|path| fs::read_to_string(path).unwrap())
            .collect::<Vec<_>>();
        for revision in 1..=5 {
            assert!(
                contents
                    .iter()
                    .any(|content| content.contains(&format!("\"test_revision\": {revision}")))
            );
        }
    }

    #[test]
    fn clean_omarchy_removes_only_backups() {
        let (root, home) = installer_fixture(4);
        let omarchy = home.join(".config/omarchy");
        write_omarchy_v4_config(&home);
        let legacy = omarchy.join("shell.json.bak.before-kvn-tui");
        let timestamped = omarchy.join("shell.json.bak.before-kvn-tui.20260821143012");
        let unrelated = omarchy.join("shell.json.bak.before-kvn-tui.notes");
        fs::write(&legacy, "legacy").unwrap();
        fs::write(&timestamped, "timestamped").unwrap();
        fs::write(&unrelated, "keep").unwrap();
        let desktop_entry = home.join(".local/share/applications/kvn-tui.desktop");
        fs::create_dir_all(desktop_entry.parent().unwrap()).unwrap();
        fs::write(&desktop_entry, "active").unwrap();
        let desktop_backup =
            desktop_entry.with_file_name("kvn-tui.desktop.bak.before-kvn-tui.20260821143012");
        fs::write(&desktop_backup, "backup").unwrap();
        let app_icon = home.join(".local/share/icons/hicolor/scalable/apps/kvn-tui.svg");
        fs::create_dir_all(app_icon.parent().unwrap()).unwrap();
        fs::write(&app_icon, "active").unwrap();
        let icon_backup = app_icon.with_file_name("kvn-tui.svg.bak.before-kvn-tui.20260821143012");
        fs::write(&icon_backup, "backup").unwrap();
        let plugin = omarchy.join("plugins/yarikov.omakvn");
        fs::create_dir_all(&plugin).unwrap();
        fs::write(plugin.join("manifest.json"), "{}").unwrap();
        let legacy_plugin = omarchy.join("plugins/kvn.tui");
        fs::create_dir_all(&legacy_plugin).unwrap();
        fs::write(legacy_plugin.join("manifest.json"), "{}").unwrap();

        let shell_config = omarchy.join("shell.json");
        let mut shell: serde_json::Value =
            serde_json::from_slice(&fs::read(&shell_config).unwrap()).unwrap();
        shell["bar"]["layout"]["right"]
            .as_array_mut()
            .unwrap()
            .insert(0, serde_json::json!({"id": "yarikov.omakvn"}));
        shell["bar"]["layout"]["right"]
            .as_array_mut()
            .unwrap()
            .insert(0, serde_json::json!({"id": "kvn.tui"}));
        fs::write(&shell_config, serde_json::to_vec_pretty(&shell).unwrap()).unwrap();

        assert_success(&run_omarchy_cleanup(&root, &home));

        assert!(!legacy.exists());
        assert!(!timestamped.exists());
        assert!(unrelated.exists());
        assert!(desktop_entry.exists());
        assert!(!desktop_backup.exists());
        assert!(app_icon.exists());
        assert!(!icon_backup.exists());
        assert!(plugin.exists(), "bar plugin directory should be preserved");
        assert!(
            legacy_plugin.exists(),
            "legacy bar plugin directory should be preserved"
        );
        let shell: serde_json::Value =
            serde_json::from_slice(&fs::read(shell_config).unwrap()).unwrap();
        assert!(
            shell["bar"]["layout"]["right"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["id"] == "yarikov.omakvn")
        );
        assert!(
            shell["bar"]["layout"]["right"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["id"] == "kvn.tui")
        );
    }

    #[test]
    fn omarchy_v4_installer_accepts_custom_keybinding() {
        let (root, home) = installer_fixture(4);
        write_omarchy_v4_config(&home);

        assert_success(&run_installer(&root, &home, "y\nSUPER SHIFT, V\n"));

        let bindings = fs::read_to_string(home.join(".config/hypr/bindings.lua")).unwrap();
        assert!(bindings.contains(r#"hl.unbind("SUPER + SHIFT + V")"#));
        assert!(bindings.contains(
            r#"o.bind("SUPER + SHIFT + V", "kvn-tui VPN client", "omarchy-launch-kvn-tui")"#
        ));
        assert!(!bindings.contains("SUPER + CTRL + K"));
    }

    #[test]
    fn omarchy_v3_installer_keeps_legacy_waybar_integration() {
        let (root, home) = installer_fixture(3);
        let waybar = home.join(".config/waybar");
        let hypr = home.join(".config/hypr");
        fs::create_dir_all(&waybar).unwrap();
        fs::create_dir_all(&hypr).unwrap();
        fs::write(
            waybar.join("config.jsonc"),
            "{\n  \"modules-right\": [\n    \"bluetooth\"\n  ]\n}\n",
        )
        .unwrap();
        fs::write(waybar.join("style.css"), "* { color: white; }\n").unwrap();
        fs::write(
            hypr.join("autostart.conf"),
            "exec-once = kvn-tui --daemon\n",
        )
        .unwrap();
        fs::write(hypr.join("bindings.conf"), "# bindings\n").unwrap();
        fs::write(hypr.join("hyprland.conf"), "# rules\n").unwrap();

        assert_success(&run_installer(&root, &home, "y\n\n"));

        let config = fs::read_to_string(waybar.join("config.jsonc")).unwrap();
        assert!(config.contains(r#""custom/kvn-tui""#));
        assert!(config.contains(r#""exec": "kvn-tui --waybar-status""#));
        assert!(
            fs::read_to_string(waybar.join("style.css"))
                .unwrap()
                .contains("#custom-kvn-tui")
        );
        assert!(
            !fs::read_to_string(hypr.join("autostart.conf"))
                .unwrap()
                .contains("kvn-tui --daemon")
        );
        assert!(
            fs::read_to_string(hypr.join("bindings.conf"))
                .unwrap()
                .contains("SUPER CTRL, K, exec, omarchy-launch-kvn-tui")
        );
        assert!(
            fs::read_to_string(hypr.join("hyprland.conf"))
                .unwrap()
                .contains("org.omarchy.kvn-tui")
        );
        assert!(
            !home
                .join(".local/share/applications/kvn-tui.desktop")
                .exists()
        );
        assert!(
            !home
                .join(".local/share/icons/hicolor/scalable/apps/kvn-tui.svg")
                .exists()
        );
    }

    #[test]
    fn omarchy_v3_failure_restores_the_current_run_snapshot() {
        let (root, home) = installer_fixture(3);
        let waybar = home.join(".config/waybar");
        fs::create_dir_all(&waybar).unwrap();
        let config = waybar.join("config.jsonc");
        let style = waybar.join("style.css");
        let original_config = "{\n  \"modules-right\": [\n    \"bluetooth\"\n  ]\n}\n";
        let original_style = "* { color: white; }\n";
        fs::write(&config, original_config).unwrap();
        fs::write(&style, original_style).unwrap();
        fs::write(
            waybar.join("config.jsonc.bak.before-kvn-tui"),
            "stale config backup",
        )
        .unwrap();
        fs::write(
            waybar.join("style.css.bak.before-kvn-tui"),
            "stale style backup",
        )
        .unwrap();
        write_executable(&root.path().join("bin/pgrep"), "#!/bin/bash\nexit 1\n");

        let output = run_installer(&root, &home, "n\n");

        assert!(!output.status.success());
        assert_eq!(fs::read_to_string(config).unwrap(), original_config);
        assert_eq!(fs::read_to_string(style).unwrap(), original_style);
    }
}
