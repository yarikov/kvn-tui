//! Network kill switch: a system-level firewall that blocks all non-VPN egress
//! when enabled. The actual firewall rules are loaded by a systemd unit
//! (`kvn-tui-killswitch.service`) that runs a wrapped `nft -f` at boot. This
//! module shells out to a helper script installed at
//! `/usr/lib/kvn-tui/killswitch-helper.sh` (via `sudo -n`, NOPASSWD for the
//! dedicated `kvn-tui` group) to enable/disable the unit and add/remove handshake
//! exceptions while sing-box is establishing the VPN tunnel.
//!
//! See `contrib/setup-killswitch.sh` for the one-time setup that the user
//! runs as `sudo kvn-tui setup --killswitch`.

use anyhow::{Context, Result, bail};
use std::net::{SocketAddr, ToSocketAddrs};
use std::process::Command;

const HELPER: &str = "/usr/lib/kvn-tui/killswitch-helper.sh";
const UNIT: &str = "kvn-tui-killswitch.service";

fn helper_present() -> bool {
    std::path::Path::new(HELPER).is_file()
}

fn run_helper(args: &[&str]) -> Result<()> {
    if !helper_present() {
        bail!(
            "kill switch helper not installed at {} — run `sudo kvn-tui setup --killswitch`",
            HELPER
        );
    }
    let output = Command::new("sudo")
        .arg("-n")
        .arg(HELPER)
        .args(args)
        .output()
        .with_context(|| format!("failed to spawn sudo {}", HELPER))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!(
            "{} {:?} failed ({}): {}{}",
            HELPER,
            args,
            output.status,
            stderr.trim(),
            if stdout.trim().is_empty() {
                String::new()
            } else {
                format!(" / {}", stdout.trim())
            }
        );
    }
    Ok(())
}

/// Enable or disable the kill switch systemd unit (also starts/stops it).
pub fn apply(enabled: bool) -> Result<()> {
    run_helper(&[if enabled { "enable" } else { "disable" }])
}

/// Add a temporary exception so sing-box can reach the given endpoint during
/// the TLS/REALITY handshake (before the tun interface is up). Idempotent at
/// the nft layer (set elements dedupe).
pub fn allow_endpoint(addr: &SocketAddr, proto: &str) -> Result<()> {
    let ip = addr.ip().to_string();
    let port = addr.port().to_string();
    run_helper(&["allow", &ip, proto, &port])
}

/// Flush the dynamic handshake set. Called on disconnect.
pub fn revoke() -> Result<()> {
    run_helper(&["revoke"])
}

/// Resolve a `host:port` to one or more socket addresses (blocking).
pub fn resolve_endpoints(host: &str, port: u16) -> Result<Vec<SocketAddr>> {
    let addrs: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .with_context(|| format!("DNS lookup failed for {}:{}", host, port))?
        .collect();
    if addrs.is_empty() {
        bail!("no addresses resolved for {}:{}", host, port);
    }
    Ok(addrs)
}

/// Query systemd for the actual unit state. Used at daemon startup to
/// reconcile `settings.kill_switch` (which may have been edited externally or
/// set on a host where the helper isn't installed).
pub fn is_active() -> Result<bool> {
    let output = Command::new("systemctl")
        .arg("is-active")
        .arg("--quiet")
        .arg(UNIT)
        .output()
        .context("failed to spawn systemctl")?;
    // `systemctl is-active` exits 0 when active, non-zero otherwise.
    Ok(output.status.success())
}
