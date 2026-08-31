//! Read-only diagnostics for the runtime environment.

use std::env;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::Result;

const MIN_SINGBOX_VERSION: (u64, u64, u64) = (1, 12, 0);
const USER_UNIT: &str = "kvn-tui.service";
const KILLSWITCH_HELPER: &str = "/usr/lib/kvn-tui/killswitch-helper.sh";
const POLKIT_DNS_ACTIONS: [&str; 3] = [
    "org.freedesktop.resolve1.set-dns-servers",
    "org.freedesktop.resolve1.set-domains",
    "org.freedesktop.resolve1.set-default-route",
];
const OMAKVN_PLUGIN_ID: &str = "yarikov.omakvn";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Pass,
    Warning,
    Failure,
    Optional,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Check {
    level: Level,
    message: String,
    remedy: Option<String>,
}

impl Check {
    fn pass(message: impl Into<String>) -> Self {
        Self::new(Level::Pass, message, None)
    }

    fn warning(message: impl Into<String>, remedy: impl Into<String>) -> Self {
        Self::new(Level::Warning, message, Some(remedy.into()))
    }

    fn failure(message: impl Into<String>, remedy: impl Into<String>) -> Self {
        Self::new(Level::Failure, message, Some(remedy.into()))
    }

    fn optional(message: impl Into<String>) -> Self {
        Self::new(Level::Optional, message, None)
    }

    fn new(level: Level, message: impl Into<String>, remedy: Option<String>) -> Self {
        Self {
            level,
            message: message.into(),
            remedy,
        }
    }
}

/// Run all diagnostics, print the report, and fail when a required component
/// is not usable.
pub fn run() -> Result<()> {
    let checks = collect();
    print_report(&checks)?;
    let failures = checks
        .iter()
        .filter(|check| check.level == Level::Failure)
        .count();
    if failures > 0 {
        anyhow::bail!("doctor found {failures} required check(s) that need attention");
    }
    Ok(())
}

fn collect() -> Vec<Check> {
    let mut checks = vec![Check::pass(format!(
        "kvn-tui {}",
        env!("CARGO_PKG_VERSION")
    ))];

    match find_singbox() {
        Some(path) => {
            checks.push(check_singbox_version(&path));
            checks.push(check_capabilities(&path));
        }
        None => checks.push(Check::failure(
            "sing-box was not found",
            "Install it with `sudo pacman -S sing-box` or set SING_BOX_PATH.",
        )),
    }

    checks.push(check_config());
    checks.extend(check_daemon());
    checks.push(check_clipboard());
    checks.push(check_killswitch());
    checks.push(check_polkit());
    checks.push(check_omarchy());
    checks
}

fn print_report(checks: &[Check]) -> io::Result<()> {
    let stdout = io::stdout();
    let use_color = stdout.is_terminal();
    write_report(stdout.lock(), checks, use_color)
}

fn write_report(mut writer: impl Write, checks: &[Check], use_color: bool) -> io::Result<()> {
    for check in checks {
        let symbol = match check.level {
            Level::Pass => "✓",
            Level::Warning => "!",
            Level::Failure => "✗",
            Level::Optional => "○",
        };
        let symbol = if use_color {
            match check.level {
                Level::Pass => format!("\x1b[38;5;10m{symbol}\x1b[39m"),
                Level::Warning => format!("\x1b[38;5;11m{symbol}\x1b[39m"),
                Level::Failure => format!("\x1b[38;5;9m{symbol}\x1b[39m"),
                Level::Optional => format!("\x1b[38;5;15m{symbol}\x1b[39m"),
            }
        } else {
            symbol.to_owned()
        };
        writeln!(writer, "{symbol} {}", check.message)?;
        if let Some(remedy) = &check.remedy {
            writeln!(writer, "  Fix: {remedy}")?;
        }
    }
    Ok(())
}

fn find_singbox() -> Option<PathBuf> {
    if let Some(value) = env::var_os("SING_BOX_PATH") {
        let path = PathBuf::from(value);
        return executable_file(&path).then_some(path);
    }
    find_on_path("sing-box")
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| executable_file(candidate))
}

fn executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

fn check_singbox_version(path: &Path) -> Check {
    let display = path.display();
    let output = Command::new(path).arg("version").output();
    let Ok(output) = output else {
        return Check::failure(
            format!("sing-box found at {display}, but could not be executed"),
            "Check the binary permissions or SING_BOX_PATH.",
        );
    };
    if !output.status.success() {
        return Check::failure(
            format!("sing-box found at {display}, but `version` failed"),
            "Reinstall sing-box and run `kvn-tui doctor` again.",
        );
    }

    let text = combined_output(&output);
    match parse_version(&text) {
        Some(version) if version >= MIN_SINGBOX_VERSION => Check::pass(format!(
            "sing-box found: {display} ({})",
            format_version(version)
        )),
        Some(version) => Check::failure(
            format!(
                "sing-box {} is too old; version 1.12.0 or newer is required",
                format_version(version)
            ),
            "Upgrade it with `sudo pacman -Syu sing-box`.",
        ),
        None => Check::failure(
            format!("could not determine the sing-box version from {display}"),
            "Run `sing-box version` and verify that the installation is valid.",
        ),
    }
}

fn combined_output(output: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    text.split_whitespace().find_map(|word| {
        let candidate = word
            .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.' && c != '-')
            .trim_start_matches('v');
        let core = candidate
            .split_once('-')
            .map_or(candidate, |(core, _)| core);
        let mut parts = core.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next()?.parse().ok()?;
        Some((major, minor, patch))
    })
}

fn format_version(version: (u64, u64, u64)) -> String {
    format!("{}.{}.{}", version.0, version.1, version.2)
}

fn check_capabilities(path: &Path) -> Check {
    let output = Command::new("getcap").arg(path).output();
    let Ok(output) = output else {
        return Check::warning(
            "could not inspect sing-box capabilities because `getcap` is unavailable",
            "Install `libcap` and run `kvn-tui doctor` again.",
        );
    };
    let text = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
    if output.status.success()
        && text.contains("cap_net_admin")
        && text.contains("cap_net_raw")
        && (text.contains("=ep") || text.contains("+ep"))
    {
        Check::pass("sing-box has cap_net_admin and cap_net_raw")
    } else {
        Check::failure(
            "sing-box is missing the capabilities required for TUN mode",
            format!(
                "Run `sudo setcap cap_net_admin,cap_net_raw+ep {}`.",
                path.display()
            ),
        )
    }
}

fn check_config() -> Check {
    let Some(path) = crate::paths::profiles_path() else {
        return Check::failure(
            "the configuration directory could not be resolved",
            "Ensure HOME or XDG_CONFIG_HOME points to a writable directory.",
        );
    };
    if !path.exists() {
        return Check::pass(format!(
            "configuration will be created on first launch: {}",
            path.display()
        ));
    }
    match crate::config::load_config_at(&path).and_then(|config| config.validate()) {
        Ok(()) => Check::pass(format!("configuration is valid: {}", path.display())),
        Err(error) => Check::failure(
            format!("configuration is invalid: {error:#}"),
            format!("Correct {} or restore its backup.", path.display()),
        ),
    }
}

fn check_daemon() -> Vec<Check> {
    let mut checks = Vec::with_capacity(2);
    match systemctl_user_is_enabled() {
        Some(true) => checks.push(Check::pass("daemon autostart is enabled")),
        Some(false) => checks.push(Check::warning(
            "daemon autostart is not enabled",
            "Run `systemctl --user enable --now kvn-tui.service`.",
        )),
        None => checks.push(Check::warning(
            "systemd user service status could not be checked",
            "Run `systemctl --user status kvn-tui.service` to inspect it.",
        )),
    }

    match crate::ipc::socket_path() {
        Err(error) => checks.push(Check::failure(
            format!("daemon IPC path is unavailable: {error}"),
            "Run kvn-tui from a desktop user session with XDG_RUNTIME_DIR set.",
        )),
        Ok(path) if crate::ipc::is_daemon_running() => checks.push(Check::pass(format!(
            "daemon IPC socket is reachable: {}",
            path.display()
        ))),
        Ok(_) => checks.push(Check::warning(
            "daemon IPC socket is not reachable",
            "Start it with `systemctl --user start kvn-tui.service`; kvn-tui can also start it on demand.",
        )),
    }
    checks
}

fn systemctl_user_is_enabled() -> Option<bool> {
    let output = Command::new("systemctl")
        .arg("--user")
        .args(["is-enabled", USER_UNIT])
        .output()
        .ok()?;
    if output.status.success() {
        return Some(true);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    matches!(stdout.trim(), "disabled" | "static" | "indirect").then_some(false)
}

fn check_clipboard() -> Check {
    let wayland = env::var_os("WAYLAND_DISPLAY").is_some();
    let x11 = env::var("XDG_SESSION_TYPE").is_ok_and(|value| value.eq_ignore_ascii_case("x11"));
    if wayland && find_on_path("wl-paste").is_some() && find_on_path("wl-copy").is_some() {
        Check::pass("clipboard backend: wl-clipboard")
    } else if x11 && find_on_path("xclip").is_some() {
        Check::pass("clipboard backend: xclip")
    } else if x11 && find_on_path("xsel").is_some() {
        Check::pass("clipboard backend: xsel")
    } else if find_on_path("wl-paste").is_some() && find_on_path("wl-copy").is_some() {
        Check::pass("clipboard backend: wl-clipboard")
    } else if find_on_path("xclip").is_some() {
        Check::pass("clipboard backend: xclip")
    } else if find_on_path("xsel").is_some() {
        Check::pass("clipboard backend: xsel")
    } else {
        Check::warning(
            "no clipboard backend was found; import and export will be unavailable",
            "Install `wl-clipboard` on Wayland or `xclip`/`xsel` on X11.",
        )
    }
}

fn check_killswitch() -> Check {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let path = Path::new(KILLSWITCH_HELPER);
    if !path.is_file() {
        return Check::optional("kill switch is not installed (optional)");
    }
    let Ok(metadata) = path.metadata() else {
        return Check::warning(
            "kill switch helper metadata could not be read",
            "Reinstall it with `sudo kvn-tui setup --killswitch`.",
        );
    };
    let mode = metadata.permissions().mode() & 0o777;
    if metadata.uid() != 0 || mode & 0o022 != 0 {
        return Check::warning(
            format!(
                "kill switch helper has unsafe ownership or mode (uid {}, {:03o})",
                metadata.uid(),
                mode
            ),
            "Reinstall it with `sudo kvn-tui setup --killswitch`.",
        );
    }

    match Command::new("sudo")
        .args(["-n", KILLSWITCH_HELPER, "check"])
        .output()
    {
        Ok(output) if output.status.success() => {
            Check::pass("kill switch helper and passwordless authorization are active")
        }
        Ok(_) => Check::warning(
            "kill switch helper is installed but passwordless authorization is unavailable",
            "Run `sudo kvn-tui setup --killswitch`, then log out and back in.",
        ),
        Err(_) => Check::warning(
            "kill switch helper is installed but `sudo` could not be executed",
            "Install sudo and run `sudo kvn-tui setup --killswitch`.",
        ),
    }
}

fn check_polkit() -> Check {
    let Some(identity) = polkit_process_identity() else {
        return Check::warning(
            "polkit authorization could not be checked",
            "Run `kvn-tui doctor` again or inspect polkit with `sudo kvn-tui setup --polkit`.",
        );
    };
    for action in POLKIT_DNS_ACTIONS {
        let output = Command::new("pkcheck")
            .args(["--action-id", action, "--process", &identity])
            .output();
        let Ok(output) = output else {
            return Check::warning(
                "`pkcheck` is unavailable, so polkit authorization could not be checked",
                "Install the `polkit` package and run `kvn-tui doctor` again.",
            );
        };

        match output.status.code() {
            Some(0) => {}
            // 1 means denied. 2 means authorization would require interaction;
            // doctor deliberately never opens an authentication prompt.
            Some(1 | 2) => {
                return Check::warning(
                    format!("passwordless polkit authorization is missing for {action}"),
                    "Run `sudo kvn-tui setup --polkit`, then log out and back in.",
                );
            }
            _ => {
                let error = String::from_utf8_lossy(&output.stderr);
                return Check::warning(
                    format!(
                        "polkit authorization for {action} could not be checked: {}",
                        error.trim()
                    ),
                    "Verify that polkit is running, then run `kvn-tui doctor` again.",
                );
            }
        }
    }
    Check::pass("all required polkit DNS authorizations are active")
}

/// Build the non-racy `PID,START_TIME,UID` identity recommended by pkcheck.
fn polkit_process_identity() -> Option<String> {
    use std::os::unix::fs::MetadataExt;

    let pid = std::process::id();
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let start_time = proc_start_time(&stat)?;
    let uid = std::fs::metadata("/proc/self").ok()?.uid();
    Some(format!("{pid},{start_time},{uid}"))
}

fn proc_start_time(stat: &str) -> Option<&str> {
    // `/proc/<pid>/stat` fields 2 and 3 are `(comm)` and state. The command
    // may contain spaces or parentheses, so split only after its final `)`.
    // starttime is field 22, i.e. token 19 when counting from field 3.
    stat.rsplit_once(") ")?.1.split_whitespace().nth(19)
}

fn check_omarchy() -> Check {
    match crate::omarchy::detect_omarchy_theme() {
        Some(_) if omarchy_v4_detected() && !omakvn_plugin_installed() => Check::warning(
            format!("Omarchy 4 detected; {OMAKVN_PLUGIN_ID} plugin is not installed"),
            "Run `kvn-tui setup --omarchy` to install the Omarchy Shell plugin.",
        ),
        Some(theme) => Check::pass(format!("Omarchy detected; active theme: {theme}")),
        None => Check::optional("Omarchy was not detected (optional)"),
    }
}

fn omarchy_v4_detected() -> bool {
    dirs::state_dir().is_some_and(|state| state.join("omarchy/current/theme.name").is_file())
}

fn omakvn_plugin_installed() -> bool {
    let Some(manifest) = dirs::config_dir().map(|config| {
        config
            .join("omarchy/plugins")
            .join(OMAKVN_PLUGIN_ID)
            .join("manifest.json")
    }) else {
        return false;
    };
    let Ok(raw) = std::fs::read_to_string(manifest) else {
        return false;
    };
    serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|manifest| manifest.get("id")?.as_str().map(str::to_owned))
        .is_some_and(|id| id == OMAKVN_PLUGIN_ID)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    struct EnvGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let previous = env::var_os(key);
            unsafe { env::set_var(key, value) };
            Self { key, previous }
        }

        fn remove(key: &'static str) -> Self {
            let previous = env::var_os(key);
            unsafe { env::remove_var(key) };
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => unsafe { env::set_var(self.key, value) },
                None => unsafe { env::remove_var(self.key) },
            }
        }
    }

    fn executable(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn parses_release_and_prerelease_versions() {
        assert_eq!(parse_version("sing-box version 1.13.15"), Some((1, 13, 15)));
        assert_eq!(parse_version("sing-box v1.12.0-beta.1"), Some((1, 12, 0)));
    }

    #[test]
    fn rejects_output_without_semver() {
        assert_eq!(parse_version("sing-box development build"), None);
    }

    #[test]
    fn version_comparison_rejects_old_releases() {
        assert!((1, 11, 9) < MIN_SINGBOX_VERSION);
        assert!((1, 12, 0) >= MIN_SINGBOX_VERSION);
    }

    #[test]
    fn executable_file_checks_execute_bits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tool");
        std::fs::write(&path, b"tool").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!executable_file(&path));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(executable_file(&path));
    }

    #[test]
    fn failure_check_has_remedy() {
        let check = Check::failure("broken", "fix it");
        assert_eq!(check.level, Level::Failure);
        assert_eq!(check.remedy.as_deref(), Some("fix it"));
    }

    #[test]
    fn optional_check_does_not_have_remedy() {
        let check = Check::optional("not installed");
        assert_eq!(check.level, Level::Optional);
        assert!(check.remedy.is_none());
    }

    #[test]
    fn parses_start_time_from_proc_stat() {
        let stat = "123 (kvn tui) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 424242 20";
        assert_eq!(proc_start_time(stat), Some("424242"));
    }

    #[test]
    fn rejects_malformed_proc_stat() {
        assert_eq!(proc_start_time("not proc stat"), None);
    }

    #[test]
    fn singbox_version_check_covers_success_and_failures() {
        // Spawns the stub scripts (execve reads the process env) — hold
        // ENV_LOCK against env-mutating tests.
        let _lock = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();

        let current = executable(dir.path(), "current", "echo 'sing-box version 1.13.2'");
        assert_eq!(check_singbox_version(&current).level, Level::Pass);

        let old = executable(dir.path(), "old", "echo 'sing-box version 1.11.9'");
        assert_eq!(check_singbox_version(&old).level, Level::Failure);

        let unknown = executable(dir.path(), "unknown", "echo 'development build'");
        assert_eq!(check_singbox_version(&unknown).level, Level::Failure);

        let failing = executable(dir.path(), "failing", "exit 7");
        assert_eq!(check_singbox_version(&failing).level, Level::Failure);

        assert_eq!(
            check_singbox_version(&dir.path().join("missing")).level,
            Level::Failure
        );
    }

    #[test]
    fn path_and_singbox_resolution_use_environment() {
        let _lock = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let binary = executable(dir.path(), "sing-box", "exit 0");
        let _path = EnvGuard::set("PATH", dir.path());
        let _override = EnvGuard::remove("SING_BOX_PATH");
        assert_eq!(find_on_path("sing-box"), Some(binary.clone()));
        assert_eq!(find_singbox(), Some(binary.clone()));

        let _override = EnvGuard::set("SING_BOX_PATH", &binary);
        assert_eq!(find_singbox(), Some(binary));
    }

    #[test]
    fn capability_check_classifies_getcap_output() {
        let _lock = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let binary = executable(dir.path(), "sing-box", "exit 0");

        executable(
            dir.path(),
            "getcap",
            "echo \"$1 cap_net_admin,cap_net_raw=ep\"",
        );
        let _path = EnvGuard::set("PATH", dir.path());
        assert_eq!(check_capabilities(&binary).level, Level::Pass);

        executable(dir.path(), "getcap", "exit 0");
        assert_eq!(check_capabilities(&binary).level, Level::Failure);
    }

    #[test]
    fn capability_check_warns_when_getcap_is_missing() {
        let _lock = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let binary = executable(dir.path(), "sing-box", "exit 0");
        let empty = tempfile::tempdir().unwrap();
        let _path = EnvGuard::set("PATH", empty.path());
        assert_eq!(check_capabilities(&binary).level, Level::Warning);
    }

    #[test]
    fn config_check_accepts_missing_and_valid_files_and_rejects_invalid_json() {
        let _lock = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let _xdg = EnvGuard::set("XDG_CONFIG_HOME", dir.path());

        assert_eq!(check_config().level, Level::Pass);
        let path = crate::paths::profiles_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{}").unwrap();
        assert_eq!(check_config().level, Level::Pass);
        std::fs::write(path, "not json").unwrap();
        assert_eq!(check_config().level, Level::Failure);
    }

    #[test]
    fn systemd_check_classifies_enabled_disabled_and_errors() {
        let _lock = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let _path = EnvGuard::set("PATH", dir.path());

        executable(dir.path(), "systemctl", "echo enabled");
        assert_eq!(systemctl_user_is_enabled(), Some(true));
        executable(dir.path(), "systemctl", "echo disabled; exit 1");
        assert_eq!(systemctl_user_is_enabled(), Some(false));
        executable(dir.path(), "systemctl", "echo error >&2; exit 1");
        assert_eq!(systemctl_user_is_enabled(), None);
    }

    #[test]
    fn clipboard_check_detects_backends_and_missing_tools() {
        let _lock = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let _path = EnvGuard::set("PATH", dir.path());
        let _wayland = EnvGuard::set("WAYLAND_DISPLAY", "wayland-1");
        let _session = EnvGuard::remove("XDG_SESSION_TYPE");

        assert_eq!(check_clipboard().level, Level::Warning);
        executable(dir.path(), "wl-paste", "exit 0");
        executable(dir.path(), "wl-copy", "exit 0");
        assert_eq!(check_clipboard().level, Level::Pass);
    }

    #[test]
    fn omarchy_four_check_requires_omakvn_plugin() {
        let _lock = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let state = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        let _state = EnvGuard::set("XDG_STATE_HOME", state.path());
        let _config = EnvGuard::set("XDG_CONFIG_HOME", config.path());
        let current = state.path().join("omarchy/current");
        std::fs::create_dir_all(&current).unwrap();
        std::fs::write(current.join("theme.name"), "tokyo-night\n").unwrap();

        let missing = check_omarchy();
        assert_eq!(missing.level, Level::Warning);
        assert!(missing.message.contains(OMAKVN_PLUGIN_ID));
        assert_eq!(
            missing.remedy.as_deref(),
            Some("Run `kvn-tui setup --omarchy` to install the Omarchy Shell plugin.")
        );

        let plugin = config.path().join("omarchy/plugins").join(OMAKVN_PLUGIN_ID);
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(
            plugin.join("manifest.json"),
            format!(r#"{{"id":"{OMAKVN_PLUGIN_ID}"}}"#),
        )
        .unwrap();

        let installed = check_omarchy();
        assert_eq!(installed.level, Level::Pass);
        assert!(installed.message.contains("active theme: tokyo-night"));
    }

    #[test]
    fn polkit_check_classifies_authorized_denied_and_errors() {
        let _lock = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let _path = EnvGuard::set("PATH", dir.path());

        executable(dir.path(), "pkcheck", "exit 0");
        assert_eq!(check_polkit().level, Level::Pass);
        executable(dir.path(), "pkcheck", "exit 2");
        let check = check_polkit();
        assert_eq!(check.level, Level::Warning);
        assert_eq!(
            check.remedy.as_deref(),
            Some("Run `sudo kvn-tui setup --polkit`, then log out and back in.")
        );
        executable(dir.path(), "pkcheck", "echo unavailable >&2; exit 127");
        assert_eq!(check_polkit().level, Level::Warning);
    }

    #[test]
    fn polkit_check_warns_when_pkcheck_is_missing() {
        let _lock = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let _path = EnvGuard::set("PATH", dir.path());
        assert_eq!(check_polkit().level, Level::Warning);
    }

    #[test]
    fn report_without_color_is_plain_text() {
        let checks = [
            Check::pass("ready"),
            Check::warning("warning", "fix"),
            Check::failure("failure", "fix"),
            Check::optional("optional"),
        ];
        let mut output = Vec::new();

        write_report(&mut output, &checks, false).unwrap();

        assert_eq!(
            String::from_utf8(output).unwrap(),
            concat!(
                "✓ ready\n",
                "! warning\n",
                "  Fix: fix\n",
                "✗ failure\n",
                "  Fix: fix\n",
                "○ optional\n",
            )
        );
    }

    #[test]
    fn report_colors_only_status_symbols() {
        let checks = [
            Check::pass("ready"),
            Check::warning("warning", "fix"),
            Check::failure("failure", "fix"),
            Check::optional("optional"),
        ];
        let mut output = Vec::new();

        write_report(&mut output, &checks, true).unwrap();

        assert_eq!(
            String::from_utf8(output).unwrap(),
            concat!(
                "\x1b[38;5;10m✓\x1b[39m ready\n",
                "\x1b[38;5;11m!\x1b[39m warning\n",
                "  Fix: fix\n",
                "\x1b[38;5;9m✗\x1b[39m failure\n",
                "  Fix: fix\n",
                "\x1b[38;5;15m○\x1b[39m optional\n",
            )
        );
    }

    #[test]
    fn process_identity_has_three_numeric_fields() {
        let identity = polkit_process_identity().unwrap();
        let fields: Vec<_> = identity.split(',').collect();
        assert_eq!(fields.len(), 3);
        assert!(fields.iter().all(|field| field.parse::<u64>().is_ok()));
    }

    #[test]
    fn full_doctor_run_succeeds_with_required_dependencies() {
        let _lock = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        executable(bin.path(), "sing-box", "echo 'sing-box version 1.13.2'");
        executable(
            bin.path(),
            "getcap",
            "echo \"$1 cap_net_admin,cap_net_raw=ep\"",
        );
        executable(bin.path(), "systemctl", "echo disabled; exit 1");
        executable(bin.path(), "pkcheck", "exit 0");
        executable(bin.path(), "wl-paste", "exit 0");
        executable(bin.path(), "wl-copy", "exit 0");

        let _path = EnvGuard::set("PATH", bin.path());
        let _override = EnvGuard::remove("SING_BOX_PATH");
        let _xdg = EnvGuard::set("XDG_CONFIG_HOME", config.path());
        let _wayland = EnvGuard::set("WAYLAND_DISPLAY", "wayland-test");
        assert!(run().is_ok());
    }

    #[test]
    fn full_doctor_run_fails_without_singbox() {
        let _lock = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        executable(bin.path(), "systemctl", "echo disabled; exit 1");
        executable(bin.path(), "pkcheck", "exit 2");

        let _path = EnvGuard::set("PATH", bin.path());
        let _override = EnvGuard::remove("SING_BOX_PATH");
        let _xdg = EnvGuard::set("XDG_CONFIG_HOME", config.path());
        let _wayland = EnvGuard::remove("WAYLAND_DISPLAY");
        let _session = EnvGuard::remove("XDG_SESSION_TYPE");
        assert!(run().is_err());
    }
}
