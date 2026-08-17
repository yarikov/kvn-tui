use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::app::effect::Effect;
use crate::app::model::{AppStatus, ConnectionState, Model, Overlay, TrafficStats};
use crate::app::msg::{GeoResult, IpcCommand, Msg, StateSnapshot};
use crate::app::update::update;
use crate::ipc::{IpcServer, cleanup_socket};
use crate::singbox::process_handle::ProcessHandle;

/// Run the daemon main loop.
pub fn run(mut model: Model) -> Result<()> {
    let (tx, rx) = channel::<Msg>();
    let ipc_server = IpcServer::bind(tx.clone())?;

    spawn_ticker(tx.clone());
    spawn_suspend_watcher(tx.clone());
    if let Err(e) = spawn_signal_handler(tx.clone()) {
        tracing::warn!("Failed to install signal handler: {e}");
    }

    reconcile_kill_switch_state(&mut model);

    let process_slot = Arc::new(Mutex::new(None));

    let result = run_loop(&mut model, rx, &tx, process_slot.clone(), &ipc_server);

    // Cleanup
    if let Some(mut handle) = lock_process_slot(&process_slot).take()
        && let Err(e) = handle.kill_and_wait()
    {
        tracing::warn!("Failed to stop sing-box on exit: {}", e);
    }
    if model.config.settings.kill_switch
        && let Err(e) = crate::services::killswitch::revoke()
    {
        tracing::warn!("Failed to flush kill switch handshake set on exit: {}", e);
    }
    cleanup_socket();

    result
}

fn run_loop(
    model: &mut Model,
    rx: std::sync::mpsc::Receiver<Msg>,
    tx: &Sender<Msg>,
    process_slot: Arc<Mutex<Option<ProcessHandle>>>,
    ipc_server: &IpcServer,
) -> Result<()> {
    loop {
        let msg = rx.recv()?;
        let effects = update(model, msg);
        let mut should_broadcast = false;

        for effect in &effects {
            if matches!(
                effect,
                Effect::Connect { .. }
                    | Effect::Disconnect
                    | Effect::DownloadGeo
                    | Effect::WriteState
                    | Effect::SaveConfig
                    | Effect::UpdateSubscription { .. }
                    | Effect::BroadcastState
                    | Effect::ApplyKillSwitch { .. }
            ) {
                should_broadcast = true;
            }
        }

        for effect in effects {
            execute_daemon_effect(effect, tx, model, &process_slot)?;
        }

        if model.should_quit {
            break;
        }

        if should_broadcast {
            ipc_server.broadcast(&build_snapshot(model));
        }
    }
    Ok(())
}

fn execute_daemon_effect(
    effect: Effect,
    tx: &Sender<Msg>,
    model: &mut Model,
    process_slot: &Arc<Mutex<Option<ProcessHandle>>>,
) -> Result<()> {
    match effect {
        Effect::Connect { profile, settings } => {
            if let Some(mut handle) = lock_process_slot(process_slot).take()
                && let Err(e) = handle.kill_and_wait()
            {
                tracing::warn!("Failed to stop sing-box process: {}", e);
            }
            model.connection = ConnectionState::ConnectPending;
            let tx = tx.clone();
            let slot = process_slot.clone();
            let kill_switch = model.config.settings.kill_switch;
            let dns = settings.dns.clone();
            thread::spawn(move || {
                if kill_switch {
                    if let Err(e) = crate::services::killswitch::revoke() {
                        let err = crate::app::msg::IpcError::from(
                            e.context("failed to clear stale kill switch exceptions"),
                        );
                        let _ = tx.send(Msg::ConnectFailed(err));
                        return;
                    }
                    if let Err(e) = open_handshake_window(&profile, &dns) {
                        if let Err(cleanup_err) = crate::services::killswitch::revoke() {
                            tracing::warn!(
                                "Failed to clean up kill switch exceptions after handshake error: {}",
                                cleanup_err
                            );
                        }
                        let err = crate::app::msg::IpcError::from(
                            e.context("kill switch handshake setup failed"),
                        );
                        let _ = tx.send(Msg::ConnectFailed(err));
                        return;
                    }
                }
                match crate::singbox::runner::start(&profile, &settings) {
                    Ok(handle) => {
                        let pid = handle.pid;
                        *lock_process_slot(&slot) = Some(handle);
                        let _ = tx.send(Msg::Connected {
                            pid,
                            profile_id: profile.id,
                        });
                    }
                    Err(e) => {
                        if kill_switch
                            && let Err(cleanup_err) = crate::services::killswitch::revoke()
                        {
                            tracing::warn!(
                                "Failed to clean up kill switch exceptions after connect failure: {}",
                                cleanup_err
                            );
                        }
                        let _ = tx.send(Msg::ConnectFailed(crate::app::msg::IpcError::from(e)));
                    }
                }
            });
        }
        Effect::Disconnect => {
            if let Some(mut handle) = lock_process_slot(process_slot).take()
                && let Err(e) = handle.kill_and_wait()
            {
                tracing::warn!("Failed to stop sing-box process: {}", e);
            }
            model.connection = ConnectionState::Idle;
            model.active_profile_id = None;
            model.singbox_pid = None;
            model.traffic = TrafficStats::default();
            model.last_traffic_sample_at_ms = 0;
            model.last_traffic_fetch_at = None;
            model.set_status(AppStatus::Info("Disconnected".into()));
            model.overlay = Overlay::None;
            crate::services::waybar::write_state(model);
            if model.config.settings.kill_switch
                && let Err(e) = crate::services::killswitch::revoke()
            {
                tracing::warn!("Failed to flush kill switch handshake set: {}", e);
            }
        }
        Effect::DownloadGeo => {
            model.geo_updating = true;
            let tx = tx.clone();
            let region = model
                .config
                .settings
                .geo_routing
                .current_region
                .unwrap_or(crate::config::profile::GeoRegion::Global);
            let services = model.config.settings.geo_routing.enabled_services();
            thread::spawn(move || {
                let gm = match crate::geo::GeoManager::new() {
                    Ok(gm) => gm,
                    Err(e) => {
                        let _ = tx.send(Msg::GeoUpdated(GeoResult::Error(e.to_string())));
                        return;
                    }
                };
                let result = match gm.update_if_needed(region) {
                    Ok(geo_result) => geo_result,
                    Err(e) => GeoResult::Error(e.to_string()),
                };
                let _ = tx.send(Msg::GeoUpdated(result));
                // Service rule-sets refresh only after the regional result is
                // reported: they can take minutes on a slow link and must not
                // pin the geo_updating spinner or delay further updates.
                // Their failures are non-fatal — the route builder simply
                // omits a service's rules until its files appear.
                refresh_service_rule_sets(&gm, &services);
            });
        }
        Effect::DownloadServiceRuleSetsIfMissing => {
            // Fired while a tunnel is up (after `Msg::Connected`, or on a
            // service-routing commit before its deferred reconnect), so the
            // fetch goes through the VPN — pre-tunnel, GitHub may be
            // unreachable (kill switch allowlists only the VPN endpoint, and
            // the ISP may block it). `Msg::ServiceRuleSetsReady` is sent in
            // every exit path, download failures included: the reducer uses
            // it to run a pending reconnect, and the route builder tolerates
            // files that never arrived.
            let services = model.config.settings.geo_routing.enabled_services();
            let tx = tx.clone();
            thread::spawn(move || {
                match crate::geo::GeoManager::new() {
                    Ok(gm) => {
                        let missing: Vec<_> = services
                            .into_iter()
                            .filter(|s| !gm.has_service_databases(*s))
                            .collect();
                        refresh_service_rule_sets(&gm, &missing);
                    }
                    Err(e) => {
                        tracing::warn!("Failed to init geo manager for service rule-sets: {e:#}");
                    }
                }
                let _ = tx.send(Msg::ServiceRuleSetsReady);
            });
        }
        Effect::RefreshGeoLastUpdated => {
            let tx = tx.clone();
            let region = model
                .config
                .settings
                .geo_routing
                .current_region
                .unwrap_or(crate::config::profile::GeoRegion::Global);
            thread::spawn(move || {
                let manager = crate::geo::GeoManager::new().ok();
                let last_updated = manager.as_ref().and_then(|g| g.last_updated(region));
                let last_checked_at = manager.and_then(|g| g.last_checked_at(region));
                let _ = tx.send(Msg::GeoMetadataRefreshed {
                    last_updated,
                    last_checked_at,
                });
            });
        }
        Effect::DownloadGeoIfMissing => {
            model.geo_updating = true;
            let tx = tx.clone();
            let region = model
                .config
                .settings
                .geo_routing
                .current_region
                .unwrap_or(crate::config::profile::GeoRegion::Global);
            thread::spawn(move || {
                let result = match crate::geo::GeoManager::new() {
                    Ok(gm) => {
                        if gm.has_databases(region) {
                            GeoResult::UpToDate {
                                checked_at: gm.last_checked_at(region),
                            }
                        } else {
                            match gm.update_if_needed(region) {
                                Ok(geo_result) => geo_result,
                                Err(e) => GeoResult::Error(e.to_string()),
                            }
                        }
                    }
                    Err(e) => GeoResult::Error(e.to_string()),
                };
                let _ = tx.send(Msg::GeoUpdated(result));
            });
        }
        Effect::WriteState => {
            crate::services::waybar::write_state(model);
        }
        Effect::SaveConfig => {
            if let Err(e) = model.save() {
                model.set_status(AppStatus::Error(format!("Failed to save config: {}", e)));
            }
        }
        Effect::UpdateSubscription { id } => {
            if let Some(sub) = model.config.subscriptions.iter().find(|s| s.id == id) {
                let url = sub.url.clone();
                let tx = tx.clone();
                thread::spawn(move || {
                    let result = crate::config::subscription::fetch_subscription(&url)
                        .map_err(crate::app::msg::IpcError::from);
                    let _ = tx.send(Msg::SubscriptionFetched { id, result });
                });
            }
        }
        Effect::BroadcastState => {}
        Effect::Quit => {
            model.should_quit = true;
        }
        Effect::AppendAppLog { level, message } => {
            crate::services::log_tailer::append_app_log(&level, &message);
        }
        Effect::ReloadConfig => {
            let tx = tx.clone();
            thread::spawn(move || {
                let result = crate::config::load_config()
                    .and_then(|c| c.validate().map(|_| c))
                    .map_err(crate::app::msg::IpcError::from);
                let _ = tx.send(Msg::ConfigReloaded(Box::new(result)));
            });
        }
        Effect::ApplyKillSwitch { enabled } => {
            let tx = tx.clone();
            thread::spawn(move || {
                let error = crate::services::killswitch::apply(enabled)
                    .err()
                    .map(crate::app::msg::IpcError::from);
                let _ = tx.send(Msg::KillSwitchApplied { enabled, error });
            });
        }
        Effect::FetchTrafficStats { .. } => {
            let tx = tx.clone();
            thread::spawn(
                move || match crate::singbox::clash_api::fetch_connections() {
                    Ok(snap) => {
                        let sampled_at_ms = unix_now_ms();
                        let _ = tx.send(Msg::TrafficStatsUpdated {
                            up_total: snap.up_total,
                            down_total: snap.down_total,
                            conn_count: snap.conn_count,
                            sampled_at_ms,
                        });
                    }
                    Err(e) => {
                        // The endpoint may legitimately be unreachable for a few
                        // hundred ms after sing-box spawns; don't surface this to
                        // the user.
                        tracing::debug!("clash_api fetch failed: {e}");
                    }
                },
            );
        }
        Effect::TestProfile { id } => {
            let profile = model.config.profiles.iter().find(|p| p.id == id).cloned();
            let tx = tx.clone();
            thread::spawn(move || {
                let latency_ms = profile.and_then(|p| match run_test(&p, id) {
                    Ok(ms) => Some(ms),
                    Err(e) => {
                        tracing::warn!("profile test failed: {e:#}");
                        None
                    }
                });
                let _ = tx.send(Msg::TestResult { id, latency_ms });
            });
        }
    }
    Ok(())
}

/// Test a profile's reachability using a temporary sing-box instance.
///
/// Allocates a free loopback port, writes a minimal SOCKS5-inbound config,
/// spawns sing-box, waits for the port to open, then performs a SOCKS5
/// CONNECT to `connectivitycheck.gstatic.com:80` through the proxy and
/// returns the round-trip latency in milliseconds.
fn run_test(profile: &crate::config::profile::Profile, id: uuid::Uuid) -> anyhow::Result<u64> {
    use std::process::{Command, Stdio};

    // Find a free loopback port by binding to :0, recording the OS-assigned
    // port, then dropping the listener so sing-box can bind to it.
    let socks_port = {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.local_addr()?.port()
    };

    let config = crate::singbox::config::generate_test_config(profile, socks_port)?;
    let config_path = crate::paths::temp_test_config_path(&id);
    std::fs::write(&config_path, serde_json::to_string(&config)?)?;

    let singbox_bin = std::env::var("SING_BOX_PATH").unwrap_or_else(|_| "sing-box".to_string());
    let mut child = Command::new(&singbox_bin)
        .args(["run", "-c", config_path.to_string_lossy().as_ref()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to spawn {singbox_bin}"))?;

    let cleanup = |child: &mut std::process::Child| {
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(&config_path);
    };

    // Wait up to 3 s for sing-box to open the SOCKS5 port.
    let addr = format!("127.0.0.1:{socks_port}");
    let deadline = Instant::now() + Duration::from_secs(3);
    let ready = loop {
        if Instant::now() >= deadline {
            break false;
        }
        if TcpStream::connect(&addr).is_ok() {
            break true;
        }
        thread::sleep(Duration::from_millis(80));
    };

    if !ready {
        cleanup(&mut child);
        anyhow::bail!("sing-box did not open SOCKS5 port within 3 s");
    }

    // Perform SOCKS5 CONNECT to a well-known host and measure latency.
    let result = socks5_connect_latency(&addr);
    cleanup(&mut child);
    result
}

/// Tunnel through the SOCKS5 proxy at `addr` to `connectivitycheck.gstatic.com:80`,
/// send a minimal HTTP GET, and return the time from request-send to first
/// response byte in milliseconds.
///
/// sing-box replies to SOCKS5 CONNECT before the outbound tunnel is open, so
/// measuring CONNECT RTT gives ~0 ms. The HTTP round-trip through the actual
/// VPN tunnel is the meaningful latency number.
fn socks5_connect_latency(addr: &str) -> anyhow::Result<u64> {
    const HOST: &[u8] = b"connectivitycheck.gstatic.com";
    const HOST_STR: &str = "connectivitycheck.gstatic.com";
    const PORT: u16 = 80;

    let mut stream = TcpStream::connect(addr)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;

    // SOCKS5 greeting: no-auth method selection.
    stream.write_all(&[0x05, 0x01, 0x00])?;
    let mut resp = [0u8; 2];
    stream.read_exact(&mut resp)?;
    anyhow::ensure!(resp == [0x05, 0x00], "SOCKS5 auth negotiation failed");

    // SOCKS5 CONNECT to target host (domain ATYP 0x03).
    let mut req = vec![0x05, 0x01, 0x00, 0x03, HOST.len() as u8];
    req.extend_from_slice(HOST);
    req.push((PORT >> 8) as u8);
    req.push((PORT & 0xff) as u8);
    stream.write_all(&req)?;

    // Read and discard CONNECT reply — sing-box answers before the tunnel is
    // actually open, so this RTT is not meaningful.
    let mut hdr = [0u8; 4];
    stream.read_exact(&mut hdr)?;
    anyhow::ensure!(hdr[0] == 0x05 && hdr[1] == 0x00, "SOCKS5 CONNECT rejected");
    match hdr[3] {
        0x01 => {
            let mut b = [0u8; 6];
            stream.read_exact(&mut b)?;
        }
        0x03 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len)?;
            let mut b = vec![0u8; len[0] as usize + 2];
            stream.read_exact(&mut b)?;
        }
        0x04 => {
            let mut b = [0u8; 18];
            stream.read_exact(&mut b)?;
        }
        _ => anyhow::bail!("unknown SOCKS5 address type"),
    }

    // Now the tunnel is open. Send an HTTP GET and time the first response byte
    // — this is the real VPN round-trip latency.
    let http_req =
        format!("GET /generate_204 HTTP/1.1\r\nHost: {HOST_STR}\r\nConnection: close\r\n\r\n");
    let start = Instant::now();
    stream.write_all(http_req.as_bytes())?;
    let mut buf = [0u8; 16];
    stream.read_exact(&mut buf)?;
    Ok(start.elapsed().as_millis() as u64)
}

/// Best-effort check/download of service rule-sets, shared by the periodic
/// geo-update thread and the post-connect fetch. Failures are logged, never
/// surfaced — the route builder just omits a service's rules until its files
/// appear.
fn refresh_service_rule_sets(
    gm: &crate::geo::GeoManager,
    services: &[crate::config::profile::RoutedService],
) {
    for service in services {
        if let Err(e) = gm.update_service_if_needed(*service) {
            tracing::warn!("Failed to update {} rule-sets: {e:#}", service.label());
        }
    }
}

/// Wall-clock time in milliseconds since the Unix epoch.
fn unix_now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// At daemon startup, align `settings.kill_switch` with the actual systemd
/// unit state. systemd is the source of truth — if a user disabled the unit
/// manually or never installed the helper, the persisted bool would otherwise
/// drift and the TUI would render `[KS]` against an open firewall.
fn reconcile_kill_switch_state(model: &mut Model) {
    let active = match crate::services::killswitch::is_active() {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("Failed to query kill switch unit state: {}", e);
            return;
        }
    };
    if model.config.settings.kill_switch != active {
        tracing::info!(
            "Reconciling kill switch state: config={}, systemd={}",
            model.config.settings.kill_switch,
            active
        );
        model.config.settings.kill_switch = active;
        if let Err(e) = model.save() {
            tracing::warn!("Failed to persist reconciled kill switch state: {}", e);
        }
    }
}

/// Pre-resolve the VPN endpoint and open a temporary nft exception so the
/// initial handshake can pass through the kill switch. Also allowlists every
/// non-`local`, non-`fakeip` DNS upstream the user has configured so sing-box
/// can resolve the VPN server hostname (see `src/singbox/config.rs`).
///
/// Set elements are deduplicated by nftables and remain until disconnect, so
/// repeated calls are idempotent and safe across reconnects.
fn open_handshake_window(
    profile: &crate::config::profile::Profile,
    dns: &crate::config::profile::DnsConfig,
) -> Result<()> {
    let endpoints = crate::services::killswitch::resolve_endpoints(&profile.address, profile.port)?;
    for addr in &endpoints {
        for protocol in handshake_protocols(profile.protocol()) {
            crate::services::killswitch::allow_endpoint(addr, protocol)?;
        }
    }
    for (host, port, proto) in dns_bootstrap_endpoints(dns) {
        match crate::services::killswitch::resolve_endpoints(&host, port) {
            Ok(addrs) => {
                for addr in &addrs {
                    crate::services::killswitch::allow_endpoint(addr, proto)?;
                }
            }
            Err(e) => {
                tracing::warn!("DNS upstream {host}:{port} resolution failed: {e}");
            }
        }
    }
    Ok(())
}

/// Network protocols that must be allowed to reach a VPN endpoint before the
/// tunnel is established. QUIC-based outbounds use UDP, while SOCKS and
/// Shadowsocks may carry traffic over either transport.
fn handshake_protocols(protocol: crate::config::profile::Protocol) -> &'static [&'static str] {
    use crate::config::profile::Protocol;

    match protocol {
        Protocol::Hysteria2 | Protocol::Tuic => &["udp"],
        Protocol::Shadowsocks | Protocol::Socks => &["tcp", "udp"],
        Protocol::Vless
        | Protocol::Vmess
        | Protocol::Trojan
        | Protocol::Shadowtls
        | Protocol::Anytls
        | Protocol::Http
        | Protocol::Ssh => &["tcp"],
    }
}

/// Return `(host, port, proto)` triples for every DNS server that needs an
/// outbound network allowlist before the tun interface is up. `local` and
/// `fakeip` servers are skipped — they never leave the host.
fn dns_bootstrap_endpoints(
    dns: &crate::config::profile::DnsConfig,
) -> Vec<(String, u16, &'static str)> {
    use crate::config::profile::DnsServer;
    dns.servers
        .iter()
        .filter_map(|s| match s {
            DnsServer::Local { .. } | DnsServer::FakeIp { .. } => None,
            DnsServer::Udp {
                server,
                server_port,
                ..
            } => Some((server.clone(), server_port.unwrap_or(53), "udp")),
            DnsServer::Tcp {
                server,
                server_port,
                ..
            } => Some((server.clone(), server_port.unwrap_or(53), "tcp")),
            DnsServer::Tls {
                server,
                server_port,
                ..
            } => Some((server.clone(), server_port.unwrap_or(853), "tcp")),
            DnsServer::Https {
                server,
                server_port,
                ..
            } => Some((server.clone(), server_port.unwrap_or(443), "tcp")),
            DnsServer::Quic {
                server,
                server_port,
                ..
            } => Some((server.clone(), server_port.unwrap_or(853), "udp")),
        })
        .collect()
}

fn build_snapshot(model: &Model) -> StateSnapshot {
    StateSnapshot {
        connection: model.connection,
        status: model.status.text().to_string(),
        status_is_error: matches!(model.status, AppStatus::Error(_)),
        singbox_pid: model.singbox_pid,
        active_profile_id: model.active_profile_id.map(|id| id.to_string()),
        selected: model.selected,
        routing_selected: model.routing_selected,
        geo_region_selected: model.geo_region_selected,
        dns_selected: model.dns_selected,
        dns_strategy_draft: model.dns_strategy_draft.clone(),
        theme_selected: model.theme_selected,
        theme_draft: model.theme_draft.clone(),
        service_routing_selected: model.service_routing_selected,
        service_routing_draft: model.service_routing_draft.clone(),
        geo_updating: model.geo_updating,
        geo_last_updated: model.geo_last_updated.clone(),
        overlay: model.overlay,
        profiles: model.config.profiles.clone(),
        subscriptions: model.config.subscriptions.clone(),
        settings: model.config.settings.clone(),
        traffic: model.traffic.clone(),
        profile_latencies: model
            .profile_latencies
            .iter()
            .map(|(id, ms)| (id.to_string(), *ms))
            .collect(),
        testing_profiles: model
            .testing_profiles
            .iter()
            .map(|id| id.to_string())
            .collect(),
    }
}

fn spawn_ticker(tx: Sender<Msg>) {
    thread::spawn(move || {
        loop {
            thread::sleep(Duration::from_millis(250));
            if tx.send(Msg::Tick).is_err() {
                break;
            }
        }
    });
}

/// Lock the sing-box process slot, recovering from poisoned-mutex state.
///
/// If a worker thread panicked while holding this lock, the standard
/// `lock().unwrap()` would re-panic on the next access and we'd lose our
/// chance to kill sing-box on shutdown. The invariant we care about — an
/// `Option<ProcessHandle>` — cannot be left half-written across an `unwind`
/// boundary, so taking the inner guard is safe.
fn lock_process_slot(
    slot: &Arc<Mutex<Option<ProcessHandle>>>,
) -> std::sync::MutexGuard<'_, Option<ProcessHandle>> {
    slot.lock().unwrap_or_else(|p| p.into_inner())
}

fn spawn_suspend_watcher(tx: Sender<Msg>) {
    thread::spawn(move || {
        crate::services::suspend::listen_blocking(tx);
    });
}

/// Translate SIGTERM/SIGINT into an `IpcCommand::Quit` message so the daemon
/// can run its normal cleanup path (kill sing-box, remove socket) instead of
/// being torn down mid-flight by the kernel. Only the first signal is acted
/// on; further signals fall through to the default disposition.
fn spawn_signal_handler(tx: Sender<Msg>) -> Result<()> {
    use signal_hook::consts::{SIGINT, SIGTERM};
    use signal_hook::iterator::Signals;
    let mut signals =
        Signals::new([SIGTERM, SIGINT]).context("Failed to register SIGTERM/SIGINT handler")?;
    thread::spawn(move || {
        if let Some(sig) = signals.forever().next() {
            tracing::info!("Received signal {sig}, shutting down");
            let _ = tx.send(Msg::IpcCommand(IpcCommand::Quit));
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::handshake_protocols;
    use crate::config::profile::Protocol;

    #[test]
    fn handshake_protocols_match_outbound_transports() {
        for protocol in [Protocol::Hysteria2, Protocol::Tuic] {
            assert_eq!(handshake_protocols(protocol), &["udp"]);
        }

        for protocol in [Protocol::Shadowsocks, Protocol::Socks] {
            assert_eq!(handshake_protocols(protocol), &["tcp", "udp"]);
        }

        for protocol in [
            Protocol::Vless,
            Protocol::Vmess,
            Protocol::Trojan,
            Protocol::Shadowtls,
            Protocol::Anytls,
            Protocol::Http,
            Protocol::Ssh,
        ] {
            assert_eq!(handshake_protocols(protocol), &["tcp"]);
        }
    }
}
