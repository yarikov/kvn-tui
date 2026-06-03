use std::path::{Path, PathBuf};
use std::process::Command;

/// Check whether the process is running as `root` elevated via `sudo`.
pub fn is_elevated() -> bool {
    std::env::var("USER").ok() == Some("root".to_string()) && std::env::var("SUDO_USER").is_ok()
}

/// Return the original user's name set by `sudo`.
pub fn sudo_user() -> Option<String> {
    std::env::var("SUDO_USER").ok().filter(|s| !s.is_empty())
}

/// Return the original user's UID set by `sudo`.
pub fn sudo_uid() -> Option<u32> {
    std::env::var("SUDO_UID").ok().and_then(|s| s.parse().ok())
}

/// Look up a user's home directory via `getent passwd`.
#[cfg(not(test))]
pub fn home_dir(username: &str) -> Option<PathBuf> {
    let output = Command::new("getent")
        .args(["passwd", username])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let line = String::from_utf8(output.stdout).ok()?;
    let home = line.trim().split(':').nth(5)?;
    Some(PathBuf::from(home))
}

/// Return the original user's XDG runtime directory (`/run/user/<uid>`).
pub fn runtime_dir() -> Option<PathBuf> {
    let uid = sudo_uid()?;
    let path = PathBuf::from(format!("/run/user/{}", uid));
    if path.exists() { Some(path) } else { None }
}

/// Find a Wayland display socket (`wayland-N`) in the given directory.
pub fn wayland_display(runtime_dir: &Path) -> Option<String> {
    let entries = std::fs::read_dir(runtime_dir).ok()?;
    for entry in entries {
        let entry = entry.ok()?;
        let name = entry.file_name().into_string().ok()?;
        if name.starts_with("wayland-")
            && name
                .strip_prefix("wayland-")
                .and_then(|s| s.parse::<u32>().ok())
                .is_some()
        {
            return Some(name);
        }
    }
    None
}

/// Build a [`Command`] that runs as the original user when elevated via `sudo`.
///
/// If not elevated, returns a command for `program` directly.
pub fn command_as_user(program: &str) -> Command {
    if let Some(user) = sudo_user() {
        let mut cmd = Command::new("sudo");
        cmd.arg("-u").arg(&user);
        // Preserve TERM so terminal apps render correctly under the user's config.
        if let Ok(term) = std::env::var("TERM") {
            cmd.env("TERM", term);
        }
        cmd.arg(program);
        cmd
    } else {
        Command::new(program)
    }
}
