use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::app::effect::Effect;
use crate::app::model::{AppStatus, ConnectionState, Model, Overlay, TrafficStats};
use crate::app::msg::{GeoResult, IpcCommand, LogSessionOffsets, Msg, StateSnapshot};
use crate::app::update::update;
use crate::ipc::{IpcServer, cleanup_socket};
use crate::singbox::process_handle::ProcessHandle;

struct ProcessSlot {
    attempt_id: u64,
    handle: Option<ProcessHandle>,
}

struct DaemonShared {
    process_slot: Arc<Mutex<ProcessSlot>>,
    connect_coordinator: Arc<Mutex<()>>,
    singbox_log_pruned_at: Arc<Mutex<Option<Instant>>>,
}

const LOG_PRUNE_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Run the daemon main loop.
pub fn run(mut model: Model) -> Result<()> {
    let (tx, rx) = channel::<Msg>();
    let ipc_server = IpcServer::bind(tx.clone())?;

    let app_log_path = crate::paths::app_log_path();
    if let Err(error) = crate::services::log_tailer::prune_log_to_lines(
        &app_log_path,
        model.config.settings.logs.line_retention.app,
    ) {
        tracing::warn!(
            "Failed to enforce log line limit for {:?}: {}",
            app_log_path,
            error
        );
    }
    let singbox_log_path = crate::paths::singbox_log_path();
    let singbox_log_pruned_at = match crate::services::log_tailer::prune_log_to_lines(
        &singbox_log_path,
        model.config.settings.logs.line_retention.singbox,
    ) {
        Ok(()) => Some(Instant::now()),
        Err(error) => {
            tracing::warn!(
                "Failed to enforce log line limit for {:?}: {}",
                singbox_log_path,
                error
            );
            None
        }
    };
    let log_session_offsets = LogSessionOffsets {
        app: log_file_len(crate::paths::app_log_path()),
        singbox: log_file_len(crate::paths::singbox_log_path()),
    };

    spawn_suspend_watcher(tx.clone());
    if let Err(e) = spawn_signal_handler(tx.clone()) {
        tracing::warn!("Failed to install signal handler: {e}");
    }

    reconcile_kill_switch_state(&mut model);

    let shared = DaemonShared {
        process_slot: Arc::new(Mutex::new(ProcessSlot {
            attempt_id: model.connect_attempt_id,
            handle: None,
        })),
        connect_coordinator: Arc::new(Mutex::new(())),
        singbox_log_pruned_at: Arc::new(Mutex::new(singbox_log_pruned_at)),
    };
    spawn_ticker(tx.clone(), Arc::downgrade(&shared.process_slot));

    let result = run_loop(
        &mut model,
        rx,
        &tx,
        &shared,
        &ipc_server,
        log_session_offsets,
    );

    // Cleanup
    let _coordinator = shared
        .connect_coordinator
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if let Some(mut handle) = lock_process_slot(&shared.process_slot).handle.take()
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
    shared: &DaemonShared,
    ipc_server: &IpcServer,
    log_session_offsets: LogSessionOffsets,
) -> Result<()> {
    loop {
        let msg = rx.recv()?;
        let effects = update(model, msg);
        // `queue_connect` advances the generation before the next Tick emits
        // `Effect::Connect`. Publish that invalidation immediately so an old
        // worker cannot install its process during the intervening 250 ms.
        lock_process_slot(&shared.process_slot).attempt_id = model.connect_attempt_id;
        let mut should_broadcast = false;

        for effect in &effects {
            if matches!(
                effect,
                Effect::Connect { .. }
                    | Effect::Disconnect
                    | Effect::DownloadGeo
                    | Effect::RetryServiceRuleSets { .. }
                    | Effect::ResetGeoUpdateSchedules
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
            execute_daemon_effect(effect, tx, model, shared)?;
        }

        if model.should_quit {
            break;
        }

        if should_broadcast {
            ipc_server.broadcast(&build_snapshot(model, log_session_offsets));
        }
    }
    Ok(())
}

fn execute_daemon_effect(
    effect: Effect,
    tx: &Sender<Msg>,
    model: &mut Model,
    shared: &DaemonShared,
) -> Result<()> {
    match effect {
        Effect::Connect {
            profile,
            settings,
            attempt_id,
        } => {
            let previous = {
                let mut slot = lock_process_slot(&shared.process_slot);
                slot.attempt_id = attempt_id;
                slot.handle.take()
            };
            if let Some(mut handle) = previous
                && let Err(e) = handle.kill_and_wait()
            {
                tracing::warn!("Failed to stop sing-box process: {}", e);
            }
            model.connection = ConnectionState::ConnectPending;
            let tx = tx.clone();
            let slot = shared.process_slot.clone();
            let coordinator = shared.connect_coordinator.clone();
            let log_pruned_at = shared.singbox_log_pruned_at.clone();
            let kill_switch = model.config.settings.kill_switch;
            let dns = settings.dns.clone();
            thread::spawn(move || {
                let _coordinator = coordinator.lock().unwrap_or_else(|p| p.into_inner());
                if !is_current_attempt(&slot, attempt_id) {
                    return;
                }
                if kill_switch {
                    if let Err(e) = crate::services::killswitch::revoke() {
                        let err = crate::app::msg::IpcError::from(
                            e.context("failed to clear stale kill switch exceptions"),
                        );
                        let _ = tx.send(Msg::ConnectFailed {
                            attempt_id,
                            error: err,
                        });
                        return;
                    }
                    if !is_current_attempt(&slot, attempt_id) {
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
                        let _ = tx.send(Msg::ConnectFailed {
                            attempt_id,
                            error: err,
                        });
                        return;
                    }
                }
                if !is_current_attempt(&slot, attempt_id) {
                    return;
                }
                let now = Instant::now();
                let mut last_pruned = log_pruned_at.lock().unwrap_or_else(|p| p.into_inner());
                if log_prune_due(*last_pruned, now) {
                    let log_path = crate::paths::singbox_log_path();
                    match crate::services::log_tailer::prune_log_to_lines(
                        &log_path,
                        settings.logs.line_retention.singbox,
                    ) {
                        Ok(()) => *last_pruned = Some(now),
                        Err(error) => tracing::warn!(
                            "Failed to enforce log line limit for {:?}: {}",
                            log_path,
                            error
                        ),
                    }
                }
                drop(last_pruned);
                match crate::singbox::runner::start(&profile, &settings) {
                    Ok(handle) => {
                        let pid = handle.pid;
                        let stale_handle = {
                            let mut slot = lock_process_slot(&slot);
                            if slot.attempt_id == attempt_id {
                                slot.handle = Some(handle);
                                None
                            } else {
                                Some(handle)
                            }
                        };
                        if let Some(mut handle) = stale_handle {
                            let _ = handle.kill_and_wait();
                            return;
                        }
                        let _ = tx.send(Msg::Connected {
                            pid,
                            profile_id: profile.id,
                            attempt_id,
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
                        let _ = tx.send(Msg::ConnectFailed {
                            attempt_id,
                            error: crate::app::msg::IpcError::from(e),
                        });
                    }
                }
            });
        }
        Effect::Disconnect => {
            model.connect_attempt_id = model.connect_attempt_id.wrapping_add(1);
            {
                let mut slot = lock_process_slot(&shared.process_slot);
                slot.attempt_id = model.connect_attempt_id;
            }
            // Wait for an in-flight setup to observe invalidation and stop
            // before flushing its temporary kill-switch exceptions.
            let _coordinator = shared
                .connect_coordinator
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let previous = {
                let mut slot = lock_process_slot(&shared.process_slot);
                slot.handle.take()
            };
            if let Some(mut handle) = previous
                && let Err(e) = handle.kill_and_wait()
            {
                tracing::warn!("Failed to stop sing-box process: {}", e);
            }
            model.connection = ConnectionState::Idle;
            model.active_profile_id = None;
            model.connecting_profile_id = None;
            model.singbox_pid = None;
            model.traffic = TrafficStats::default();
            model.last_traffic_sample_at_ms = 0;
            model.traffic_request_id = 0;
            model.last_traffic_response_id = 0;
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
        Effect::RevokeKillSwitchExceptions => {
            if model.config.settings.kill_switch
                && let Err(e) = crate::services::killswitch::revoke()
            {
                tracing::warn!(
                    "Failed to flush kill switch handshake set after sing-box exit: {}",
                    e
                );
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
            let automatic = model.geo_automatic_update;
            let interval_days = (model
                .config
                .settings
                .geo_routing
                .auto_update
                .interval_minutes()
                / 1_440) as i64;
            let existing_service_retries = model.service_retry_states.clone();
            let existing_service_checked_at = model.service_checked_at.clone();
            thread::spawn(move || {
                let gm = match crate::geo::GeoManager::new() {
                    Ok(gm) => gm,
                    Err(e) => {
                        let _ = tx.send(Msg::GeoUpdated(GeoResult::Error {
                            message: e.to_string(),
                            retry_state: None,
                            service_retry_states: existing_service_retries,
                            service_checked_at: existing_service_checked_at,
                            next_update: None,
                            service_next_updates: Default::default(),
                            updated_parts: Vec::new(),
                        }));
                        return;
                    }
                };
                let regional = gm.update_if_needed(region);
                let services =
                    refresh_service_rule_sets(&gm, &services, automatic.then_some(interval_days));
                let result = finalize_geo_result(
                    &gm,
                    region,
                    regional,
                    services,
                    automatic,
                    true,
                    interval_days,
                );
                let _ = tx.send(Msg::GeoUpdated(result));
            });
        }
        Effect::DownloadServiceRuleSetsIfMissing => {
            // May run directly when the kill switch is off, or through a
            // tunnel after connect. Always report partial failures so the
            // reducer can log them and run any pending reconnect.
            model.geo_updating = true;
            let services = model.config.settings.geo_routing.enabled_services();
            let schedule_enabled = model.config.settings.geo_routing.auto_update
                != crate::config::profile::GeoAutoUpdate::Off;
            let tx = tx.clone();
            thread::spawn(move || {
                let (retry_states, checked_at, next_updates, updated_parts, errors) =
                    match crate::geo::GeoManager::new() {
                        Ok(gm) => {
                            let _ = gm.ensure_update_schedules(
                                crate::config::profile::GeoRegion::Global,
                                &services,
                                schedule_enabled,
                            );
                            let missing: Vec<_> = services
                                .into_iter()
                                .filter(|s| !gm.has_service_databases(*s))
                                .collect();
                            let refreshed = refresh_service_rule_sets(&gm, &missing, None);
                            (
                                gm.service_retry_states(),
                                gm.service_checked_at(),
                                gm.service_next_updates(),
                                refreshed.updated_parts,
                                refreshed.errors,
                            )
                        }
                        Err(e) => {
                            tracing::warn!(
                                "Failed to init geo manager for service rule-sets: {e:#}"
                            );
                            (
                                Default::default(),
                                Default::default(),
                                Default::default(),
                                Vec::new(),
                                vec![e.to_string()],
                            )
                        }
                    };
                let _ = tx.send(Msg::ServiceRuleSetsReady {
                    retry_states,
                    checked_at,
                    next_updates,
                    updated_parts,
                    errors,
                });
            });
        }
        Effect::RetryServiceRuleSets { services } => {
            model.geo_updating = true;
            let tx = tx.clone();
            let region = model
                .config
                .settings
                .geo_routing
                .current_region
                .unwrap_or(crate::config::profile::GeoRegion::Global);
            let existing_service_retries = model.service_retry_states.clone();
            let existing_service_checked_at = model.service_checked_at.clone();
            let automatic = model.geo_automatic_update;
            let interval_days = (model
                .config
                .settings
                .geo_routing
                .auto_update
                .interval_minutes()
                / 1_440) as i64;
            thread::spawn(move || {
                let result = match crate::geo::GeoManager::new() {
                    Ok(gm) => {
                        let checked_at = gm.last_checked_at(region);
                        let services = refresh_service_rule_sets(
                            &gm,
                            &services,
                            automatic.then_some(interval_days),
                        );
                        finalize_geo_result(
                            &gm,
                            region,
                            Ok(GeoResult::UpToDate {
                                checked_at,
                                retry_state: gm.retry_state(region),
                                service_retry_states: gm.service_retry_states(),
                                service_checked_at: gm.service_checked_at(),
                                next_update: gm.region_next_update(region),
                                service_next_updates: gm.service_next_updates(),
                                warnings: Vec::new(),
                            }),
                            services,
                            false,
                            false,
                            interval_days,
                        )
                    }
                    Err(e) => GeoResult::Error {
                        message: e.to_string(),
                        retry_state: None,
                        service_retry_states: existing_service_retries,
                        service_checked_at: existing_service_checked_at,
                        next_update: None,
                        service_next_updates: Default::default(),
                        updated_parts: Vec::new(),
                    },
                };
                let _ = tx.send(Msg::GeoUpdated(result));
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
                let last_checked_at = manager.as_ref().and_then(|g| g.last_checked_at(region));
                let retry_state = manager.as_ref().and_then(|g| g.retry_state(region));
                let service_retry_states = manager
                    .as_ref()
                    .map(|g| g.service_retry_states())
                    .unwrap_or_default();
                let service_checked_at = manager
                    .as_ref()
                    .map(|g| g.service_checked_at())
                    .unwrap_or_default();
                let _ = tx.send(Msg::GeoMetadataRefreshed {
                    last_updated,
                    last_checked_at,
                    retry_state,
                    service_retry_states,
                    service_checked_at,
                    next_update: manager.as_ref().and_then(|g| g.region_next_update(region)),
                    service_next_updates: manager
                        .as_ref()
                        .map(|g| g.service_next_updates())
                        .unwrap_or_default(),
                });
            });
        }
        Effect::ClearGeoRetryState { region } => {
            if let Ok(manager) = crate::geo::GeoManager::new()
                && let Err(e) = manager.clear_retry_state(region)
            {
                tracing::warn!("Failed to clear geo retry state: {e}");
            }
        }
        Effect::ResetGeoUpdateSchedules => {
            if let Ok(manager) = crate::geo::GeoManager::new() {
                let region = model
                    .config
                    .settings
                    .geo_routing
                    .current_region
                    .unwrap_or(crate::config::profile::GeoRegion::Global);
                let services = model.config.settings.geo_routing.enabled_services();
                let enabled = model.config.settings.geo_routing.auto_update
                    != crate::config::profile::GeoAutoUpdate::Off;
                manager.reset_update_schedules(region, &services, enabled)?;
                model.geo_next_update = manager.region_next_update(region);
                model.service_next_updates = manager.service_next_updates();
            }
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
            let retry_enabled = model.config.settings.geo_routing.auto_update
                != crate::config::profile::GeoAutoUpdate::Off;
            let interval_days = (model
                .config
                .settings
                .geo_routing
                .auto_update
                .interval_minutes()
                / 1_440) as i64;
            thread::spawn(move || {
                let result = match crate::geo::GeoManager::new() {
                    Ok(gm) => {
                        if gm.has_databases(region) {
                            let _ = gm.clear_retry_state(region);
                            GeoResult::UpToDate {
                                checked_at: gm.last_checked_at(region),
                                retry_state: None,
                                service_retry_states: gm.service_retry_states(),
                                service_checked_at: gm.service_checked_at(),
                                next_update: gm.region_next_update(region),
                                service_next_updates: gm.service_next_updates(),
                                warnings: Vec::new(),
                            }
                        } else {
                            finalize_geo_result(
                                &gm,
                                region,
                                gm.update_if_needed(region),
                                ServiceRefreshResult::default(),
                                retry_enabled,
                                true,
                                interval_days,
                            )
                        }
                    }
                    Err(e) => GeoResult::Error {
                        message: e.to_string(),
                        retry_state: None,
                        service_retry_states: Default::default(),
                        service_checked_at: Default::default(),
                        next_update: None,
                        service_next_updates: Default::default(),
                        updated_parts: Vec::new(),
                    },
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
                let sub = sub.clone();
                let settings = model.config.settings.clone();
                let tx = tx.clone();
                thread::spawn(move || {
                    let result = crate::config::subscription::fetch_subscription(&sub, &settings)
                        .map_err(crate::app::msg::IpcError::from);
                    let _ = tx.send(Msg::SubscriptionFetched { id, result });
                });
            }
        }
        Effect::BroadcastState => {}
        Effect::Quit => {
            model.connect_attempt_id = model.connect_attempt_id.wrapping_add(1);
            lock_process_slot(&shared.process_slot).attempt_id = model.connect_attempt_id;
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
        Effect::FetchTrafficStats {
            attempt_id,
            request_id,
        } => {
            let tx = tx.clone();
            thread::spawn(
                move || match crate::singbox::clash_api::fetch_connections() {
                    Ok(snap) => {
                        let sampled_at_ms = unix_now_ms();
                        let _ = tx.send(Msg::TrafficStatsUpdated {
                            attempt_id,
                            request_id,
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

    let config_path = write_test_config(profile, id, socks_port)?;

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

fn write_test_config(
    profile: &crate::config::profile::Profile,
    id: uuid::Uuid,
    socks_port: u16,
) -> anyhow::Result<std::path::PathBuf> {
    let config = crate::singbox::config::generate_test_config(profile, socks_port)?;
    let path = crate::paths::temp_test_config_path(&id);
    crate::atomic_write::write(&path, serde_json::to_string(&config)?.as_bytes())?;
    Ok(path)
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
    automatic_interval_days: Option<i64>,
) -> ServiceRefreshResult {
    let mut result = ServiceRefreshResult::default();
    for service in services {
        match gm.update_service_if_needed(*service) {
            Ok(updated) => {
                if let Some(days) = automatic_interval_days
                    && let Err(e) = gm.record_service_schedule_success(*service, days)
                {
                    tracing::warn!("Failed to schedule {} update: {e}", service.label());
                }
                if updated {
                    result
                        .updated_parts
                        .push(format!("service-{}", service.label().to_lowercase()));
                }
            }
            Err(e) => {
                let message = format!("{} rule-sets: {e:#}", service.label());
                tracing::warn!("Failed to update {message}");
                if automatic_interval_days.is_some() {
                    let _ = gm.record_service_failure(*service);
                }
                result.errors.push(message);
            }
        }
    }
    result
}

#[derive(Default)]
struct ServiceRefreshResult {
    updated_parts: Vec<String>,
    errors: Vec<String>,
}

fn finalize_geo_result(
    manager: &crate::geo::GeoManager,
    region: crate::config::profile::GeoRegion,
    regional: anyhow::Result<GeoResult>,
    services: ServiceRefreshResult,
    retry_enabled: bool,
    update_region: bool,
    interval_days: i64,
) -> GeoResult {
    let regional_failed = regional.is_err();
    let retry_state = if !update_region || !retry_enabled {
        manager.retry_state(region)
    } else if regional_failed && retry_enabled {
        manager.record_update_failure(region).ok()
    } else {
        if !regional_failed
            && let Err(e) = manager.record_region_schedule_success(region, interval_days)
        {
            tracing::warn!("Failed to schedule geo update: {e}");
        }
        None
    };
    let service_retry_states = manager.service_retry_states();
    let service_checked_at = manager.service_checked_at();
    let next_update = manager.region_next_update(region);
    let service_next_updates = manager.service_next_updates();

    match regional {
        Ok(GeoResult::Updated {
            mut parts,
            last_updated,
            checked_at,
            ..
        }) => {
            parts.extend(services.updated_parts);
            GeoResult::Updated {
                parts,
                last_updated,
                checked_at,
                retry_state,
                service_retry_states,
                service_checked_at,
                next_update,
                service_next_updates,
                warnings: services.errors,
            }
        }
        Ok(GeoResult::UpToDate { checked_at, .. }) if !services.updated_parts.is_empty() => {
            GeoResult::Updated {
                parts: services.updated_parts,
                last_updated: manager.last_updated(region),
                checked_at: checked_at.unwrap_or_else(chrono::Local::now),
                retry_state,
                service_retry_states,
                service_checked_at,
                next_update,
                service_next_updates,
                warnings: services.errors,
            }
        }
        Ok(GeoResult::UpToDate { checked_at, .. }) => GeoResult::UpToDate {
            checked_at,
            retry_state,
            service_retry_states,
            service_checked_at,
            next_update,
            service_next_updates,
            warnings: services.errors,
        },
        Ok(GeoResult::Error { .. }) => unreachable!("GeoManager never returns GeoResult::Error"),
        Err(e) => {
            let mut message = e.to_string();
            if !services.errors.is_empty() {
                message.push_str("; ");
                message.push_str(&services.errors.join("; "));
            }
            GeoResult::Error {
                message,
                retry_state,
                service_retry_states,
                service_checked_at,
                next_update,
                service_next_updates,
                updated_parts: services.updated_parts,
            }
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

fn build_snapshot(model: &Model, log_session_offsets: LogSessionOffsets) -> StateSnapshot {
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
        dns_fakeip_draft: model.dns_fakeip_draft,
        theme_selected: model.theme_selected,
        theme_draft: model.theme_draft.clone(),
        service_routing_selected: model.service_routing_selected,
        service_routing_draft: model.service_routing_draft.clone(),
        geo_updating: model.geo_updating,
        geo_last_updated: model.geo_last_updated.clone(),
        overlay: model.overlay,
        main_pane_focus: model.main_pane_focus,
        profiles: model.config.profiles.clone(),
        subscriptions: model.config.subscriptions.clone(),
        settings: model.config.settings.clone(),
        traffic: model.traffic.clone(),
        log_session_offsets: Some(log_session_offsets),
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

fn log_file_len(path: std::path::PathBuf) -> u64 {
    std::fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

fn spawn_ticker(tx: Sender<Msg>, process_slot: Weak<Mutex<ProcessSlot>>) {
    thread::spawn(move || {
        loop {
            thread::sleep(Duration::from_millis(250));
            let Some(process_slot) = process_slot.upgrade() else {
                break;
            };
            if let Some(msg) = poll_process_exit(&process_slot)
                && tx.send(msg).is_err()
            {
                break;
            }
            if tx.send(Msg::Tick).is_err() {
                break;
            }
        }
    });
}

fn poll_process_exit(process_slot: &Arc<Mutex<ProcessSlot>>) -> Option<Msg> {
    use std::os::unix::process::ExitStatusExt;

    let mut slot = lock_process_slot(process_slot);
    let status = match slot.handle.as_mut()?.try_wait() {
        Ok(Some(status)) => status,
        Ok(None) => return None,
        Err(e) => {
            tracing::warn!("Failed to monitor sing-box process: {e:#}");
            return None;
        }
    };
    let attempt_id = slot.attempt_id;
    slot.handle.take();
    Some(Msg::SingBoxExited {
        attempt_id,
        code: status.code(),
        signal: status.signal(),
    })
}

/// Lock the sing-box process slot, recovering from poisoned-mutex state.
///
/// If a worker thread panicked while holding this lock, the standard
/// `lock().unwrap()` would re-panic on the next access and we'd lose our
/// chance to kill sing-box on shutdown. The invariant we care about — an
/// `Option<ProcessHandle>` — cannot be left half-written across an `unwind`
/// boundary, so taking the inner guard is safe.
fn lock_process_slot(slot: &Arc<Mutex<ProcessSlot>>) -> std::sync::MutexGuard<'_, ProcessSlot> {
    slot.lock().unwrap_or_else(|p| p.into_inner())
}

fn is_current_attempt(slot: &Arc<Mutex<ProcessSlot>>, attempt_id: u64) -> bool {
    lock_process_slot(slot).attempt_id == attempt_id
}

fn log_prune_due(last_pruned: Option<Instant>, now: Instant) -> bool {
    last_pruned.is_none_or(|last| now.duration_since(last) >= LOG_PRUNE_INTERVAL)
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
    use super::{
        ProcessSlot, ServiceRefreshResult, finalize_geo_result, handshake_protocols,
        lock_process_slot, log_prune_due, poll_process_exit, write_test_config,
    };
    use crate::app::msg::{GeoResult, Msg};
    use crate::config::profile::Protocol;
    use crate::singbox::process_handle::ProcessHandle;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    #[test]
    fn latency_test_config_is_private() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let _runtime = crate::test_helpers::EnvVarGuard::set("XDG_RUNTIME_DIR", runtime.path());
        let profile = crate::config::profile::Profile::new_vless(
            "Test".into(),
            "1.2.3.4".into(),
            443,
            "secret-uuid".into(),
        );
        let path = write_test_config(&profile, uuid::Uuid::new_v4(), 1080).unwrap();

        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn log_prune_is_due_initially_and_after_twenty_four_hours() {
        let start = Instant::now();
        assert!(log_prune_due(None, start));
        assert!(!log_prune_due(
            Some(start),
            start + Duration::from_secs(24 * 60 * 60 - 1)
        ));
        assert!(log_prune_due(
            Some(start),
            start + Duration::from_secs(24 * 60 * 60)
        ));
    }

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

    #[test]
    fn process_poll_reports_exit_once_and_removes_handle() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let child = std::process::Command::new("sh")
            .args(["-c", "exit 7"])
            .spawn()
            .unwrap();
        let slot = Arc::new(Mutex::new(ProcessSlot {
            attempt_id: 42,
            handle: Some(ProcessHandle::new(child)),
        }));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let msg = loop {
            if let Some(msg) = poll_process_exit(&slot) {
                break msg;
            }
            assert!(std::time::Instant::now() < deadline, "child did not exit");
            std::thread::yield_now();
        };

        assert!(matches!(
            msg,
            Msg::SingBoxExited {
                attempt_id: 42,
                code: Some(7),
                signal: None,
            }
        ));
        assert!(lock_process_slot(&slot).handle.is_none());
        assert!(poll_process_exit(&slot).is_none());
    }

    #[test]
    fn process_poll_ignores_intentionally_removed_handle() {
        let slot = Arc::new(Mutex::new(ProcessSlot {
            attempt_id: 1,
            handle: None,
        }));
        assert!(poll_process_exit(&slot).is_none());
    }

    #[test]
    fn geo_batch_keeps_updates_and_schedules_retry_after_service_failure() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let manager = crate::geo::GeoManager::new().unwrap();
        let checked_at = chrono::Local::now();
        let regional = Ok(GeoResult::Updated {
            parts: vec!["geoip-ru".into()],
            last_updated: None,
            checked_at,
            retry_state: None,
            service_retry_states: Default::default(),
            service_checked_at: Default::default(),
            next_update: None,
            service_next_updates: Default::default(),
            warnings: Vec::new(),
        });
        let services = ServiceRefreshResult {
            updated_parts: vec!["service-steam".into()],
            errors: vec!["Telegram rule-sets: unavailable".into()],
        };
        manager
            .record_service_failure(crate::config::profile::RoutedService::Telegram)
            .unwrap();

        let result = finalize_geo_result(
            &manager,
            crate::config::profile::GeoRegion::Ru,
            regional,
            services,
            true,
            true,
            1,
        );

        let GeoResult::Updated {
            parts,
            retry_state,
            service_retry_states,
            warnings,
            ..
        } = result
        else {
            panic!("expected a partial updated result");
        };
        assert_eq!(parts, vec!["geoip-ru", "service-steam"]);
        assert_eq!(warnings, vec!["Telegram rule-sets: unavailable"]);
        assert!(retry_state.is_none());
        assert_eq!(
            service_retry_states[&crate::config::profile::RoutedService::Telegram]
                .consecutive_failures,
            1
        );
    }

    #[test]
    fn geo_batch_reports_service_update_when_region_failed() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let manager = crate::geo::GeoManager::new().unwrap();
        let services = ServiceRefreshResult {
            updated_parts: vec!["service-steam".into()],
            errors: Vec::new(),
        };

        let result = finalize_geo_result(
            &manager,
            crate::config::profile::GeoRegion::Ru,
            Err(anyhow::anyhow!("regional unavailable")),
            services,
            true,
            true,
            1,
        );

        let GeoResult::Error {
            updated_parts,
            retry_state,
            ..
        } = result
        else {
            panic!("expected a partial error result");
        };
        assert_eq!(updated_parts, vec!["service-steam"]);
        assert_eq!(retry_state.unwrap().consecutive_failures, 1);
    }
}
