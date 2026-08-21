use anyhow::{Context, Result};
use clap::{ArgGroup, Parser, Subcommand};

use crate::services::waybar;

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
            .args(["omarchy"])
    ))]
    Clean {
        /// Remove backup files created by `setup --omarchy`.
        #[arg(long)]
        omarchy: bool,
    },
}

/// Run the embedded Omarchy integration installer script.
fn install_omarchy() -> Result<()> {
    let script = include_str!("../contrib/setup-omarchy.sh");
    let tmp = std::env::temp_dir().join("kvn-tui-setup-omarchy.sh");
    std::fs::write(&tmp, script)?;
    let status = std::process::Command::new("bash").arg(&tmp).status()?;
    std::fs::remove_file(&tmp).ok();
    if !status.success() {
        anyhow::bail!("setup-omarchy.sh exited with status {}", status);
    }
    Ok(())
}

/// Run the embedded Omarchy integration cleanup script.
fn clean_omarchy() -> Result<()> {
    let script = include_str!("../contrib/clean-omarchy.sh");
    let tmp = std::env::temp_dir().join("kvn-tui-clean-omarchy.sh");
    std::fs::write(&tmp, script)?;
    let status = std::process::Command::new("bash")
        .arg(&tmp)
        .status()
        .context("failed to run clean-omarchy.sh")?;
    std::fs::remove_file(&tmp).ok();
    if !status.success() {
        anyhow::bail!("clean-omarchy.sh exited with status {}", status);
    }
    Ok(())
}

/// Run the embedded polkit rule installer script.
fn install_polkit() -> Result<()> {
    let script = include_str!("../contrib/setup-polkit.sh");
    let tmp = std::env::temp_dir().join("kvn-tui-setup-polkit.sh");
    std::fs::write(&tmp, script)?;
    let status = std::process::Command::new("bash")
        .arg(&tmp)
        .status()
        .context("failed to run setup-polkit.sh")?;
    std::fs::remove_file(&tmp).ok();
    if !status.success() {
        anyhow::bail!("setup-polkit.sh exited with status {}", status);
    }
    Ok(())
}

/// Run the embedded kill switch installer script.
fn install_killswitch() -> Result<()> {
    let script = include_str!("../contrib/setup-killswitch.sh");
    let tmp = std::env::temp_dir().join("kvn-tui-setup-killswitch.sh");
    std::fs::write(&tmp, script)?;
    let status = std::process::Command::new("bash")
        .arg(&tmp)
        .status()
        .context("failed to run setup-killswitch.sh")?;
    std::fs::remove_file(&tmp).ok();
    if !status.success() {
        anyhow::bail!("setup-killswitch.sh exited with status {}", status);
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
    match &cli.command {
        Some(Command::Doctor) => return Some(crate::doctor::run()),
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
        Some(Command::Clean { omarchy }) => {
            let result = (|| {
                if *omarchy {
                    clean_omarchy()?;
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
        write_executable(
            &bin.join("omarchy"),
            &format!(
                "#!/bin/bash\nif [[ ${{1:-}} == version ]]; then echo '{version}.0.0-1'; fi\n"
            ),
        );
        write_executable(
            &bin.join("hyprctl"),
            "#!/bin/bash\ncase ${1:-} in configerrors) exit 0;; reload) exit 0;; esac\n",
        );
        write_executable(&bin.join("pgrep"), "#!/bin/bash\nexit 0\n");
        write_executable(&bin.join("sleep"), "#!/bin/bash\nexit 0\n");
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
        ProcessCommand::new("bash")
            .arg(&script)
            .env("HOME", home)
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
            Some(Command::Clean { omarchy: true })
        ));
    }

    #[test]
    fn clean_requires_at_least_one_option() {
        assert!(Cli::try_parse_from(["kvn-tui", "clean"]).is_err());
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
                .filter(|entry| entry["id"] == "kvn-tui")
                .count(),
            1
        );
        let kvn_index = right
            .iter()
            .position(|entry| entry["id"] == "kvn-tui")
            .unwrap();
        let bluetooth_index = right
            .iter()
            .position(|entry| entry["id"] == "omarchy.bluetooth")
            .unwrap();
        assert_eq!(kvn_index + 1, bluetooth_index);
        assert_eq!(right[kvn_index]["exec"], "kvn-tui --waybar-status");

        let bindings = fs::read_to_string(hypr.join("bindings.lua")).unwrap();
        assert_eq!(bindings.matches("-- kvn-tui keybinding: begin").count(), 1);
        assert!(bindings.contains(r#"hl.unbind("SUPER + CTRL + K")"#));
        assert!(bindings.contains("omarchy-launch-kvn-tui"));
        let rules = fs::read_to_string(hypr.join("hyprland.lua")).unwrap();
        assert_eq!(rules.matches("-- kvn-tui window rule: begin").count(), 1);
        assert!(rules.contains(r#"o.window("^org\\.omarchy\\.kvn-tui$""#));

        let launcher = fs::read_to_string(home.join(".local/bin/omarchy-launch-kvn-tui")).unwrap();
        assert!(launcher.contains("omarchy-launch-or-focus-tui"));
        assert!(launcher.contains("--app-id=org.omarchy.kvn-tui"));
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
    fn clean_omarchy_removes_legacy_and_timestamp_backups_only() {
        let (root, home) = installer_fixture(4);
        let omarchy = home.join(".config/omarchy");
        fs::create_dir_all(&omarchy).unwrap();
        let legacy = omarchy.join("shell.json.bak.before-kvn-tui");
        let timestamped = omarchy.join("shell.json.bak.before-kvn-tui.20260821143012");
        let unrelated = omarchy.join("shell.json.bak.before-kvn-tui.notes");
        fs::write(&legacy, "legacy").unwrap();
        fs::write(&timestamped, "timestamped").unwrap();
        fs::write(&unrelated, "keep").unwrap();

        assert_success(&run_omarchy_cleanup(&root, &home));

        assert!(!legacy.exists());
        assert!(!timestamped.exists());
        assert!(unrelated.exists());
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
