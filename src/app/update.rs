use crate::app::effect::Effect;
use crate::app::model::{AppStatus, ConnectionState, Model, Overlay, TrafficStats};
use crate::app::msg::{GeoResult, Msg};
use crate::config::profile::{
    GeoRegion, Profile, RoutedService, RoutingMode, Subscription, SubscriptionAutoUpdate,
};
use chrono::Local;
use crossterm::event::KeyCode;
#[cfg(test)]
use crossterm::event::KeyEvent;
use std::time::{Duration, Instant};
use uuid::Uuid;

mod key;

use key::{derive_subscription_name, handle_ipc_command, handle_key};
#[cfg(test)]
use key::{
    handle_confirm_delete, handle_geo_region, handle_routing_mode, handle_sources,
    rebuild_key_event,
};
pub use key::{theme_picker_labels, theme_picker_slugs};

/// Minimum interval between Clash-API scrapes. The daemon ticker fires every
/// 250 ms; we only emit `Effect::FetchTrafficStats` once per second.
const TRAFFIC_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Pure function: Model + Msg → updated Model + list of Effects.
/// No I/O, no threads, no system calls.
pub fn update(model: &mut Model, msg: Msg) -> Vec<Effect> {
    match msg {
        Msg::Key(key) => handle_key(model, key),
        Msg::Tick => handle_tick(model),
        Msg::GeoUpdated(result) => handle_geo_result(model, result),
        Msg::GeoMetadataRefreshed {
            last_updated,
            last_checked_at,
            retry_state,
            service_retry_states,
            service_checked_at,
            next_update,
            service_next_updates,
        } => {
            model.geo_last_updated = last_updated;
            model.geo_last_checked_at = last_checked_at;
            model.geo_last_attempt_at = None;
            model.geo_retry_state = retry_state;
            model.service_retry_states = service_retry_states;
            model.service_checked_at = service_checked_at;
            model.geo_next_update = next_update;
            model.service_next_updates = service_next_updates;
            vec![Effect::BroadcastState]
        }
        Msg::SystemResumed => {
            if model.connection == ConnectionState::Connected
                && model
                    .active_profile_id
                    .is_some_and(|id| queue_connect(model, id))
            {
                let mut effects = vec![];
                push_status(
                    &mut effects,
                    model,
                    crate::app::model::AppStatus::Info("Resumed — reconnecting…".into()),
                );
                effects
            } else {
                vec![]
            }
        }
        Msg::Connected {
            pid,
            profile_id,
            attempt_id,
        } => {
            if attempt_id != model.connect_attempt_id {
                return vec![];
            }
            model.singbox_pid = Some(pid);
            model.connection = ConnectionState::Connected;
            model.connecting_profile_id = None;
            model.overlay = Overlay::None;
            // Fresh sing-box → fresh counters. Drop the previous sample so the
            // first delta is computed against zero rather than a stale value.
            model.traffic = TrafficStats::default();
            model.last_traffic_sample_at_ms = 0;
            model.traffic_request_id = 0;
            model.last_traffic_response_id = 0;
            model.last_traffic_fetch_at = None;
            let mut effects = vec![Effect::WriteState];
            // The tunnel is up — fetch rule-sets for enabled service routes
            // through it if any are still missing (they apply on the next
            // reconnect).
            if !model
                .config
                .settings
                .geo_routing
                .enabled_services()
                .is_empty()
            {
                effects.push(Effect::DownloadServiceRuleSetsIfMissing);
            }
            // Attribute the connection to the profile that actually connected
            // (carried in the message) — never to the cursor's row, which may
            // have moved since the connect was issued.
            model.active_profile_id = Some(profile_id);
            let profile_name = model
                .config
                .profiles
                .iter()
                .find(|p| p.id == profile_id)
                .map(|p| p.name.clone());
            push_status(
                &mut effects,
                model,
                crate::app::model::AppStatus::Info(match profile_name {
                    Some(name) => format!("Connected to {}", name),
                    // Profile deleted while the connect was in flight.
                    None => "Connected".to_string(),
                }),
            );
            // Persist last connected profile for auto-connect on next startup.
            if model.config.settings.last_connected_profile != Some(profile_id) {
                model.config.settings.last_connected_profile = Some(profile_id);
                effects.push(Effect::SaveConfig);
            }
            effects
        }
        Msg::SubscriptionFetched { id, result } => {
            let mut effects = handle_subscription_result(model, id, result);
            effects.push(Effect::BroadcastState);
            effects
        }
        Msg::ServiceRuleSetsReady {
            retry_states,
            checked_at,
            next_updates,
            updated_parts,
            errors,
        } => {
            model.geo_updating = false;
            model.service_retry_states = retry_states;
            model.service_checked_at = checked_at;
            model.service_next_updates = next_updates;
            let mut effects = Vec::new();
            for part in updated_parts {
                effects.push(Effect::AppendAppLog {
                    level: "INFO".to_string(),
                    message: format!("Updated: {part}"),
                });
            }
            if !errors.is_empty() {
                push_status(
                    &mut effects,
                    model,
                    AppStatus::Error(format!(
                        "Service rule-set update failed: {}",
                        errors.join("; ")
                    )),
                );
                append_download_hint(&mut effects, model, DownloadKind::Geo);
            }
            if !model.pending_service_reconnect {
                if !effects.is_empty() {
                    effects.push(Effect::BroadcastState);
                }
                return effects;
            }
            model.pending_service_reconnect = false;
            if model.connection != ConnectionState::Connected {
                // Disconnected while the download ran — the files are on
                // disk and apply on whatever connect happens next.
                effects.push(Effect::BroadcastState);
                return effects;
            }
            effects.push(Effect::BroadcastState);
            // Reconnect the ACTIVE profile explicitly — never the cursor's
            // row, which may have moved since the routing change.
            let active = model
                .active_profile_id
                .and_then(|id| model.config.profiles.iter().find(|p| p.id == id).cloned());
            if let Some(profile) = active {
                queue_connect(model, profile.id);
                push_status(
                    &mut effects,
                    model,
                    crate::app::model::AppStatus::Info(
                        "Service routing changed — reconnecting".into(),
                    ),
                );
            } else {
                push_status(
                    &mut effects,
                    model,
                    crate::app::model::AppStatus::Info(
                        "Service routing saved — reconnect to apply".into(),
                    ),
                );
            }
            effects
        }
        Msg::ConnectFailed { attempt_id, error } => {
            if attempt_id != model.connect_attempt_id {
                return vec![];
            }
            model.connection = ConnectionState::Idle;
            model.connecting_profile_id = None;
            model.singbox_pid = None;
            model.active_profile_id = None;
            model.traffic = TrafficStats::default();
            model.last_traffic_sample_at_ms = 0;
            model.traffic_request_id = 0;
            model.last_traffic_response_id = 0;
            model.last_traffic_fetch_at = None;
            let mut effects = vec![Effect::BroadcastState];
            push_status(
                &mut effects,
                model,
                crate::app::model::AppStatus::Error(format!("Connection failed: {}", error)),
            );
            effects
        }
        Msg::SingBoxExited {
            attempt_id,
            code,
            signal,
        } => {
            if attempt_id != model.connect_attempt_id {
                return vec![];
            }
            // Invalidate any Connected/traffic reply that was already queued
            // by the worker for the process that just exited.
            model.connect_attempt_id = model.connect_attempt_id.wrapping_add(1);
            model.connection = ConnectionState::Idle;
            model.connecting_profile_id = None;
            model.singbox_pid = None;
            model.active_profile_id = None;
            model.traffic = TrafficStats::default();
            model.last_traffic_sample_at_ms = 0;
            model.traffic_request_id = 0;
            model.last_traffic_response_id = 0;
            model.last_traffic_fetch_at = None;

            let reason = match (code, signal) {
                (Some(code), _) => format!("sing-box exited unexpectedly (code {code})"),
                (None, Some(signal)) => {
                    format!("sing-box terminated unexpectedly (signal {signal})")
                }
                (None, None) => "sing-box exited unexpectedly".to_string(),
            };
            let mut effects = vec![
                Effect::WriteState,
                Effect::RevokeKillSwitchExceptions,
                Effect::BroadcastState,
            ];
            push_status(&mut effects, model, AppStatus::Error(reason));
            effects
        }

        Msg::Resize => {
            model.needs_redraw = true;
            vec![]
        }
        Msg::IpcCommand(cmd) => handle_ipc_command(model, cmd),
        Msg::StateUpdate(_) => vec![],
        Msg::ConfigReloaded(result) => handle_config_reloaded(model, *result),
        Msg::KillSwitchApplied { enabled, error } => {
            handle_kill_switch_applied(model, enabled, error)
        }
        Msg::TrafficStatsUpdated {
            attempt_id,
            request_id,
            up_total,
            down_total,
            conn_count,
            sampled_at_ms,
        } => handle_traffic_stats_updated(
            model,
            attempt_id,
            request_id,
            up_total,
            down_total,
            conn_count,
            sampled_at_ms,
        ),
        Msg::ThemeChanged(theme) => {
            // Manual picker override wins: ignore Omarchy watcher events
            // unless the user has explicitly opted into auto-follow.
            if model.config.settings.theme != "omarchy" {
                return vec![];
            }
            model.theme = theme;
            model.needs_redraw = true;
            vec![]
        }
        Msg::TestResult { id, latency_ms } => {
            model.testing_profiles.remove(&id);
            model.profile_latencies.insert(id, latency_ms);
            vec![Effect::BroadcastState]
        }
        Msg::Mouse(_) => vec![],
    }
}

/// Compute a per-second byte rate from two cumulative samples. Returns 0 when
/// no time has passed, when the counter went backwards (e.g. sing-box restart
/// reset its totals), or when there's no prior sample yet (`prev_at_ms == 0`).
pub(crate) fn compute_rate(prev_total: u64, curr_total: u64, elapsed_ms: u64) -> u64 {
    if elapsed_ms == 0 {
        return 0;
    }
    let delta = curr_total.saturating_sub(prev_total);
    // (delta bytes / elapsed ms) * 1000 — done in u128 to avoid overflow.
    ((delta as u128 * 1000) / elapsed_ms as u128) as u64
}

fn handle_traffic_stats_updated(
    model: &mut Model,
    attempt_id: u64,
    request_id: u64,
    up_total: u64,
    down_total: u64,
    conn_count: usize,
    sampled_at_ms: u64,
) -> Vec<Effect> {
    if model.connection != ConnectionState::Connected
        || attempt_id != model.connect_attempt_id
        || request_id <= model.last_traffic_response_id
    {
        return vec![];
    }
    let prev_at_ms = model.last_traffic_sample_at_ms;
    // No prior sample yet — record totals but leave rates at zero. The next
    // tick produces the first instantaneous reading against this baseline.
    let (up_rate, down_rate) = if prev_at_ms == 0 {
        (0, 0)
    } else {
        let elapsed = sampled_at_ms.saturating_sub(prev_at_ms);
        (
            compute_rate(model.traffic.up_total, up_total, elapsed),
            compute_rate(model.traffic.down_total, down_total, elapsed),
        )
    };
    model.traffic = TrafficStats {
        up_rate_bps: up_rate,
        down_rate_bps: down_rate,
        up_total,
        down_total,
        conn_count,
    };
    model.last_traffic_sample_at_ms = sampled_at_ms;
    model.last_traffic_response_id = request_id;
    vec![Effect::BroadcastState]
}

fn handle_kill_switch_applied(
    model: &mut Model,
    enabled: bool,
    error: Option<crate::app::msg::IpcError>,
) -> Vec<Effect> {
    if model.kill_switch_pending != Some(enabled) {
        return Vec::new();
    }
    model.kill_switch_pending = None;
    let mut effects = Vec::new();
    match error {
        None => {
            model.config.settings.kill_switch = enabled;
            push_status(
                &mut effects,
                model,
                AppStatus::Info(format!(
                    "Kill switch {}",
                    if enabled { "enabled" } else { "disabled" }
                )),
            );
            effects.push(Effect::SaveConfig);
            effects.push(Effect::BroadcastState);
        }
        Some(err) => {
            push_status(
                &mut effects,
                model,
                AppStatus::Error(format!("Kill switch: {}", err)),
            );
            effects.push(Effect::BroadcastState);
        }
    }
    effects
}

/// Set the application status (pure, in-memory) and return an effect that
/// appends the same message to the on-disk log file.
fn set_status(model: &mut Model, status: AppStatus) -> Option<Effect> {
    let text = status.text();
    let effect = if text.is_empty() {
        None
    } else {
        let level = match &status {
            AppStatus::Info(_) => "INFO",
            AppStatus::Error(_) => "ERROR",
        };
        Some(Effect::AppendAppLog {
            level: level.to_string(),
            message: text.to_string(),
        })
    };
    model.set_status(status);
    effect
}

fn push_status(effects: &mut Vec<Effect>, model: &mut Model, status: AppStatus) {
    if let Some(e) = set_status(model, status) {
        effects.push(e);
    }
}

#[derive(Clone, Copy)]
pub(super) enum DownloadKind {
    Geo,
    Subscription,
}

pub(super) fn download_allowed(model: &Model) -> bool {
    !model.config.settings.kill_switch || model.connection == ConnectionState::Connected
}

pub(super) fn push_download_blocked(
    effects: &mut Vec<Effect>,
    model: &mut Model,
    kind: DownloadKind,
) {
    let message = match kind {
        DownloadKind::Geo => {
            "Geo download is blocked by the kill switch. Connect to VPN and retry."
        }
        DownloadKind::Subscription => {
            "Subscription update is blocked by the kill switch. Connect to VPN and retry."
        }
    };
    push_status(effects, model, AppStatus::Error(message.into()));
}

fn append_download_hint(effects: &mut Vec<Effect>, model: &Model, kind: DownloadKind) {
    if model.connection == ConnectionState::Connected {
        return;
    }
    let message = match (model.config.settings.kill_switch, kind) {
        (true, DownloadKind::Geo) => {
            "Kill switch is enabled and VPN is disconnected. Reconnect VPN and retry the geo download."
        }
        (true, DownloadKind::Subscription) => {
            "Kill switch is enabled and VPN is disconnected. Reconnect VPN and retry the subscription update."
        }
        (false, DownloadKind::Geo) => {
            "VPN is disconnected. Try connecting to VPN and retrying the geo download."
        }
        (false, DownloadKind::Subscription) => {
            "VPN is disconnected. Try connecting to VPN and retrying the subscription update."
        }
    };
    effects.push(Effect::AppendAppLog {
        level: "WARN".to_string(),
        message: message.into(),
    });
}

fn handle_config_reloaded(
    model: &mut Model,
    result: Result<crate::config::profile::Config, crate::app::msg::IpcError>,
) -> Vec<Effect> {
    match result {
        Ok(mut config) => {
            let old_settings = model.config.settings.clone();
            let kill_switch_ignored = config.settings.kill_switch != old_settings.kill_switch;
            // The persisted flag is not the source of truth for the live
            // firewall. Only ApplyKillSwitch may change it after the helper
            // succeeds, so an editor reload must not create config/systemd
            // drift.
            config.settings.kill_switch = old_settings.kill_switch;
            let runtime_settings_changed =
                connection_settings_changed(&old_settings, &config.settings);
            let service_routes_changed = old_settings.geo_routing.service_routes
                != config.settings.geo_routing.service_routes;
            let active_profile_changed = model.active_profile_id.is_some_and(|id| {
                profile_runtime_changed_for_id(&model.config.profiles, &config.profiles, id)
            });
            let connecting_profile_changed = model.connecting_profile_id.is_some_and(|id| {
                profile_runtime_changed_for_id(&model.config.profiles, &config.profiles, id)
            });
            let active_missing = model.connection == ConnectionState::Connected
                && model
                    .active_profile_id
                    .is_some_and(|id| !config.profiles.iter().any(|profile| profile.id == id));
            let connecting_missing = matches!(
                model.connection,
                ConnectionState::Connecting | ConnectionState::ConnectPending
            ) && model
                .connecting_profile_id
                .is_some_and(|id| !config.profiles.iter().any(|profile| profile.id == id));
            let region_changed = model.config.settings.geo_routing.current_region
                != config.settings.geo_routing.current_region;
            model.replace_config_preserving_selection(config);
            let mut effects = vec![Effect::BroadcastState];
            if active_missing || connecting_missing {
                effects.push(Effect::Disconnect);
            } else {
                let runtime_changed = runtime_settings_changed
                    || match model.connection {
                        ConnectionState::Connected => active_profile_changed,
                        ConnectionState::Connecting | ConnectionState::ConnectPending => {
                            connecting_profile_changed
                        }
                        _ => false,
                    };
                if runtime_changed {
                    match model.connection {
                        ConnectionState::Connected if service_routes_changed => {
                            model.pending_service_reconnect = true;
                            effects.push(Effect::DownloadServiceRuleSetsIfMissing);
                        }
                        ConnectionState::Connected => {
                            if let Some(id) = model.active_profile_id {
                                queue_connect(model, id);
                            }
                        }
                        ConnectionState::ConnectPending => {
                            if let Some(id) = model.connecting_profile_id {
                                queue_connect(model, id);
                            }
                        }
                        // A queued attempt has not captured its Profile and
                        // Settings yet; the next Tick reads the new config.
                        ConnectionState::Connecting | ConnectionState::Idle => {}
                    }
                }
            }
            if region_changed {
                model.geo_last_updated = None;
                model.geo_last_checked_at = None;
                model.geo_last_attempt_at = None;
                effects.push(Effect::RefreshGeoLastUpdated);
            }
            let reconnecting = model.connection == ConnectionState::Connecting
                && (active_profile_changed
                    || connecting_profile_changed
                    || runtime_settings_changed);
            let status = if kill_switch_ignored && reconnecting {
                "Kill switch edit ignored (use K); configuration changed — reconnecting"
            } else if kill_switch_ignored
                && model.pending_service_reconnect
                && service_routes_changed
            {
                "Kill switch edit ignored (use K); configuration changed — updating rule-sets"
            } else if kill_switch_ignored {
                "Kill switch edit ignored — use K to change it"
            } else if reconnecting {
                "Configuration changed — reconnecting"
            } else if model.pending_service_reconnect && service_routes_changed {
                "Configuration changed — updating rule-sets"
            } else {
                "Profiles reloaded"
            };
            push_status(&mut effects, model, AppStatus::Info(status.into()));
            effects
        }
        Err(e) => {
            let mut effects = vec![Effect::BroadcastState];
            push_status(
                &mut effects,
                model,
                AppStatus::Error(format!("Failed to reload: {}", e)),
            );
            effects
        }
    }
}

fn profile_runtime_changed_for_id(old: &[Profile], new: &[Profile], id: Uuid) -> bool {
    let old = old.iter().find(|profile| profile.id == id);
    let new = new.iter().find(|profile| profile.id == id);
    match (old, new) {
        (Some(old), Some(new)) => profile_runtime_changed(old, new),
        _ => false,
    }
}

fn profile_runtime_changed(old: &Profile, new: &Profile) -> bool {
    old.address != new.address || old.port != new.port || old.config != new.config
}

fn connection_settings_changed(
    old: &crate::config::profile::Settings,
    new: &crate::config::profile::Settings,
) -> bool {
    old.tun_interface != new.tun_interface
        || old.dns != new.dns
        || old.geo_routing.mode() != new.geo_routing.mode()
        || old.geo_routing.service_routes != new.geo_routing.service_routes
        || old.logs.level != new.logs.level
}

fn handle_tick(model: &mut Model) -> Vec<Effect> {
    handle_tick_at(model, Local::now())
}

fn handle_tick_at(model: &mut Model, now: chrono::DateTime<Local>) -> Vec<Effect> {
    let mut effects = Vec::new();

    if geo_update_due(model, now) {
        model.geo_updating = true;
        model.geo_automatic_update = true;
        model.geo_last_attempt_at = Some(now);
        effects.push(Effect::DownloadGeo);
    } else if !model.geo_updating && download_allowed(model) {
        let due_services: Vec<_> = model
            .config
            .settings
            .geo_routing
            .enabled_services()
            .into_iter()
            .filter(|service| service_update_due(model, *service, now))
            .collect();
        if !due_services.is_empty() {
            model.geo_updating = true;
            model.geo_automatic_update = true;
            effects.push(Effect::RetryServiceRuleSets {
                services: due_services,
            });
        }
    }

    // Connection handling
    if model.connection == ConnectionState::Connecting {
        let profile = model.connecting_profile_id.and_then(|id| {
            model
                .config
                .profiles
                .iter()
                .find(|profile| profile.id == id)
                .cloned()
        });
        if let Some(profile) = profile {
            let settings = model.config.settings.clone();
            effects.push(Effect::Connect {
                profile,
                settings,
                attempt_id: model.connect_attempt_id,
            });
        } else {
            model.connection = ConnectionState::Idle;
            model.connecting_profile_id = None;
            model.overlay = Overlay::None;
            effects.push(Effect::BroadcastState);
        }
    }

    // Auto-update subscriptions that are due.
    effects.extend(check_due_subscriptions_at(model, now));

    // Dispatch pending profile tests, max 4 concurrent.
    while model.testing_profiles.len() < 4 {
        let Some(id) = model.pending_tests.pop_front() else {
            break;
        };
        model.testing_profiles.insert(id);
        effects.push(Effect::TestProfile { id });
    }

    // Throttled Clash-API poll for live traffic stats.
    if model.connection == ConnectionState::Connected {
        let now = Instant::now();
        let due = match model.last_traffic_fetch_at {
            None => true,
            Some(prev) => now.duration_since(prev) >= TRAFFIC_POLL_INTERVAL,
        };
        if due {
            model.last_traffic_fetch_at = Some(now);
            model.traffic_request_id = model.traffic_request_id.wrapping_add(1);
            effects.push(Effect::FetchTrafficStats {
                attempt_id: model.connect_attempt_id,
                request_id: model.traffic_request_id,
            });
        }
    }

    effects
}

fn service_update_due(model: &Model, service: RoutedService, now: chrono::DateTime<Local>) -> bool {
    if let Some(state) = model.service_retry_states.get(&service)
        && (state.attempt_date.is_none() || state.attempt_date == Some(now.date_naive()))
    {
        return state.consecutive_failures < 5 && now >= state.retry_at;
    }
    model
        .config
        .settings
        .geo_routing
        .auto_update
        .interval_minutes()
        != 0
        && now.time() >= chrono::NaiveTime::from_hms_opt(8, 0, 0).unwrap()
        && model.service_next_updates.get(&service).map_or_else(
            || {
                model
                    .service_checked_at
                    .get(&service)
                    .is_none_or(|checked| {
                        now.signed_duration_since(*checked).num_minutes()
                            >= model
                                .config
                                .settings
                                .geo_routing
                                .auto_update
                                .interval_minutes() as i64
                    })
            },
            |date| now.date_naive() >= *date,
        )
}

pub(super) fn queue_connect(model: &mut Model, profile_id: Uuid) -> bool {
    if model
        .config
        .profiles
        .iter()
        .any(|profile| profile.id == profile_id)
    {
        model.connecting_profile_id = Some(profile_id);
        model.connect_attempt_id = model.connect_attempt_id.wrapping_add(1);
        model.connection = ConnectionState::Connecting;
        true
    } else {
        false
    }
}

/// Commit a routing-mode change: shared by the routing overlay's Enter key
/// and the `SetRoutingMode` IPC command so both run identical logic.
/// Rejects modes unavailable for the current geo region.
pub(super) fn commit_routing_mode(model: &mut Model, mode: RoutingMode) -> Vec<Effect> {
    let region = model.config.settings.geo_routing.current_region;
    if !RoutingMode::available(region).contains(&mode) {
        let mut effects = vec![];
        push_status(
            &mut effects,
            model,
            AppStatus::Error(format!(
                "Routing mode {mode} is unavailable for region {}",
                region.map(|r| r.code_upper()).unwrap_or("GLOBAL")
            )),
        );
        return effects;
    }
    let changed = model.config.settings.geo_routing.mode() != mode;
    model.config.settings.geo_routing.set_mode(mode);
    let mut effects = vec![Effect::SaveConfig];
    push_status(
        &mut effects,
        model,
        AppStatus::Info(format!("Routing mode: {mode}")),
    );

    if changed
        && model.connection == ConnectionState::Connected
        && let Some(active_id) = model.active_profile_id
        && queue_connect(model, active_id)
    {
        push_status(
            &mut effects,
            model,
            AppStatus::Info(format!("Mode changed to {mode} — reconnecting")),
        );
    }
    effects
}

/// Commit a geo-region switch: shared by the region overlay's Enter key and
/// the `SetGeoRegion` IPC command. Persists the old region's routing mode,
/// restores the new region's stored mode, kicks off missing-database
/// downloads, and reconnects/auto-connects as the overlay does.
pub(super) fn commit_geo_region(model: &mut Model, region: GeoRegion) -> Vec<Effect> {
    let old_region = model.config.settings.geo_routing.current_region;
    let old_mode = model.config.settings.geo_routing.mode();
    let changed = old_region != Some(region);
    model.config.settings.geo_routing.set_region(region);
    let mut effects = vec![Effect::SaveConfig];
    if changed {
        effects.push(Effect::RefreshGeoLastUpdated);
    }
    push_status(
        &mut effects,
        model,
        AppStatus::Info(format!("Geo region: {}", region.as_str())),
    );

    // If the region changed and is not Global, check whether geo databases
    // are present and download them automatically if they are missing.
    if changed && region != GeoRegion::Global {
        if download_allowed(model) {
            model.geo_updating = true;
            model.geo_last_attempt_at = Some(chrono::Local::now());
            push_status(
                &mut effects,
                model,
                AppStatus::Info("Checking geo databases...".to_string()),
            );
            effects.push(Effect::DownloadGeoIfMissing);
        } else {
            push_download_blocked(&mut effects, model, DownloadKind::Geo);
        }
    }

    // Persist the previously active routing mode under the old region
    // and restore the mode stored for the newly selected region.
    if changed {
        if let Some(old_region) = old_region {
            model
                .config
                .settings
                .geo_routing
                .selected_region_modes
                .insert(old_region, old_mode);
        }
        let new_mode = model.config.settings.geo_routing.mode();
        if new_mode != old_mode {
            push_status(
                &mut effects,
                model,
                AppStatus::Info(format!("Routing mode: {new_mode}")),
            );
        }
    }

    // Trigger auto-connect immediately after picking a region
    // so the user does not have to restart the app.
    if model.connection == ConnectionState::Idle
        && model.config.settings.auto_connect
        && let Some(profile_id) = model.config.settings.last_connected_profile
        && let Some(idx) = model
            .config
            .profiles
            .iter()
            .position(|p| p.id == profile_id)
    {
        model.selected = crate::app::model::row_for_profile(&model.config, idx);
        queue_connect(model, profile_id);
        if let Some(profile) = model.config.profiles.get(idx) {
            push_status(
                &mut effects,
                model,
                AppStatus::Info(format!("Auto-connecting to {}…", profile.name)),
            );
        }
    }

    if changed
        && model.connection == ConnectionState::Connected
        && let Some(active_id) = model.active_profile_id
        && queue_connect(model, active_id)
    {
        model.logs.push_back("Region changed — reconnecting".into());
    }
    effects
}

fn geo_update_due(model: &Model, now: chrono::DateTime<Local>) -> bool {
    if model.geo_updating {
        return false;
    }
    // With the kill switch active, ordinary application traffic cannot leave
    // through the physical interface. Wait for sing-box to bring up the TUN
    // before starting the HTTP check; otherwise startup auto-connect and the
    // geo download race each other and the request is dropped by nftables.
    if model.config.settings.kill_switch && model.connection != ConnectionState::Connected {
        return false;
    }
    let Some(region) = model.config.settings.geo_routing.current_region else {
        return false;
    };
    if region == GeoRegion::Global {
        return false;
    }
    let interval = model
        .config
        .settings
        .geo_routing
        .auto_update
        .interval_minutes();
    if interval == 0 {
        return false;
    }
    if let Some(retry) = model.geo_retry_state
        && (retry.attempt_date.is_none() || retry.attempt_date == Some(now.date_naive()))
    {
        return retry.consecutive_failures < 5 && now >= retry.retry_at;
    }
    now.time() >= chrono::NaiveTime::from_hms_opt(8, 0, 0).unwrap()
        && model.geo_next_update.map_or_else(
            || {
                let reference = match (model.geo_last_checked_at, model.geo_last_attempt_at) {
                    (Some(checked), Some(attempt)) => Some(checked.max(attempt)),
                    (Some(checked), None) => Some(checked),
                    (None, Some(attempt)) => Some(attempt),
                    (None, None) => None,
                };
                reference.is_none_or(|last| {
                    now.signed_duration_since(last).num_minutes() >= interval as i64
                })
            },
            |date| now.date_naive() >= date,
        )
}

fn check_due_subscriptions_at(model: &mut Model, now: chrono::DateTime<Local>) -> Vec<Effect> {
    // Subscription fetches use ordinary application networking, so with the
    // kill switch active they must wait until sing-box has created the TUN.
    if model.config.settings.kill_switch && model.connection != ConnectionState::Connected {
        return Vec::new();
    }

    let mut effects = Vec::new();
    for sub in &model.config.subscriptions {
        let interval = sub.auto_update.interval_minutes();
        if interval == 0 {
            continue;
        }
        if model.subscription_updates.contains(&sub.id) {
            continue;
        }
        let today = now.date_naive();
        let after_window = now.time() >= chrono::NaiveTime::from_hms_opt(8, 0, 0).unwrap();
        let due = match &sub.retry_state {
            Some(state) if state.attempt_date.is_none() || state.attempt_date == Some(today) => {
                state.consecutive_failures < 5 && now >= state.retry_at
            }
            _ => {
                after_window
                    && sub.next_auto_update.map_or_else(
                        || {
                            sub.last_updated.is_none_or(|last| {
                                now.signed_duration_since(last).num_minutes() >= interval as i64
                            })
                        },
                        |date| today >= date,
                    )
            }
        };
        if due {
            model.subscription_updates.insert(sub.id);
            model.automatic_subscription_updates.insert(sub.id);
            effects.push(Effect::UpdateSubscription { id: sub.id });
        }
    }
    effects
}

fn handle_copied_status(model: &mut Model, name: String, count: usize) -> Vec<Effect> {
    let msg = if name == "log" && count > 1 {
        format!("Copied {count} logs")
    } else if count <= 1 {
        format!("Copied: {name}")
    } else {
        format!("Copied {count} links from {name}")
    };
    let mut effects = Vec::new();
    push_status(&mut effects, model, crate::app::model::AppStatus::Info(msg));
    effects
}

fn handle_clipboard_text(model: &mut Model, text: &str) -> Vec<Effect> {
    let trimmed = text.trim();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return add_and_fetch_subscription(model, trimmed);
    }

    match crate::config::profile::parse_share_link(trimmed) {
        Ok(profile) => {
            if model.has_duplicate(&profile) {
                let mut effects = Vec::new();
                push_status(
                    &mut effects,
                    model,
                    crate::app::model::AppStatus::Error("Profile already exists".into()),
                );
                return effects;
            }
            let name = profile.name.clone();
            model.add_profile(profile);
            let mut effects = vec![Effect::SaveConfig];
            push_status(
                &mut effects,
                model,
                crate::app::model::AppStatus::Info(format!("Pasted profile: {}", name)),
            );
            effects
        }
        Err(e) => {
            let mut effects = Vec::new();
            push_status(
                &mut effects,
                model,
                crate::app::model::AppStatus::Error(format!("Invalid URI: {}", e)),
            );
            effects
        }
    }
}

fn add_and_fetch_subscription(model: &mut Model, url: &str) -> Vec<Effect> {
    let name = derive_subscription_name(url);
    let id = Uuid::new_v4();
    let sub = Subscription {
        id,
        name: name.clone(),
        url: url.to_string(),
        auto_update: SubscriptionAutoUpdate::default(),
        last_updated: None,
        next_auto_update: None,
        retry_state: None,
    };
    model.config.subscriptions.push(sub);
    model.selected = crate::app::model::row_for_subscription_header(
        &model.config,
        model.config.subscriptions.len().saturating_sub(1),
    );
    let mut effects = vec![Effect::SaveConfig];
    if !download_allowed(model) {
        push_download_blocked(&mut effects, model, DownloadKind::Subscription);
        return effects;
    }
    model.subscription_fetching = true;
    model.subscription_updates.insert(id);
    effects.push(Effect::UpdateSubscription { id });
    push_status(
        &mut effects,
        model,
        crate::app::model::AppStatus::Info(format!(
            "Added subscription '{}' and fetching profiles…",
            name
        )),
    );
    effects
}

fn handle_subscription_result(
    model: &mut Model,
    id: Uuid,
    result: Result<Vec<Profile>, crate::app::msg::IpcError>,
) -> Vec<Effect> {
    handle_subscription_result_at(model, id, result, Local::now())
}

fn handle_subscription_result_at(
    model: &mut Model,
    id: Uuid,
    result: Result<Vec<Profile>, crate::app::msg::IpcError>,
    now: chrono::DateTime<Local>,
) -> Vec<Effect> {
    let managed = !id.is_nil();
    let automatic = model.automatic_subscription_updates.remove(&id);
    model.subscription_updates.remove(&id);
    if model.subscription_updates.is_empty() {
        model.subscription_fetching = false;
    }

    // Remember where the cursor is so we can restore it to the subscription
    // header after import (add_profile moves selection on every call).
    let saved_selected = model.selected;
    let old_active_profile = model
        .active_profile_id
        .and_then(|id| model.config.profiles.iter().find(|p| p.id == id).cloned());
    let old_connecting_profile = model
        .connecting_profile_id
        .and_then(|id| model.config.profiles.iter().find(|p| p.id == id).cloned());

    // Capture old dedup_key → UUID mapping before removing subscription profiles,
    // so we can reuse UUIDs for servers that survive the update.
    let old_sub_ids: std::collections::HashMap<String, Uuid> = model
        .config
        .profiles
        .iter()
        .filter(|p| p.subscription_id == Some(id))
        .map(|p| (p.dedup_key(), p.id))
        .collect();

    let mut effects = match result {
        Ok(profiles) => {
            if managed {
                if let Some(sub) = model.config.subscriptions.iter_mut().find(|s| s.id == id) {
                    sub.last_updated = Some(now);
                    if automatic {
                        sub.schedule_next_after_success(now);
                    }
                }
                // Only replace the previous snapshot after the fetch and parse
                // succeeded. A transient subscription error must leave the
                // user's working profiles and update timestamp untouched.
                model
                    .config
                    .profiles
                    .retain(|p| p.subscription_id != Some(id));
            }
            let mut imported = 0;
            for mut profile in profiles {
                let key = profile.dedup_key();
                if let Some(&old_id) = old_sub_ids.get(&key) {
                    // Same server was in this subscription before — reuse its UUID so
                    // active_profile_id stays valid across updates.
                    profile.id = old_id;
                    if managed {
                        profile.subscription_id = Some(id);
                    }
                    model.add_profile(profile);
                    imported += 1;
                } else if let Some(idx) = model
                    .config
                    .profiles
                    .iter()
                    .position(|p| p.dedup_key() == key)
                {
                    let existing = &model.config.profiles[idx];
                    if existing.subscription_id.is_none() {
                        // Update the standalone profile in place and link it to
                        // the subscription, preserving its identity.
                        profile.id = existing.id;
                        if managed {
                            profile.subscription_id = Some(id);
                        }
                        model.config.profiles[idx] = profile;
                        imported += 1;
                    }
                    // Profiles belonging to other subscriptions are skipped.
                } else {
                    if managed {
                        profile.subscription_id = Some(id);
                    }
                    model.add_profile(profile);
                    imported += 1;
                }
            }

            let mut effects = Vec::new();
            if imported > 0 || managed {
                effects.push(Effect::SaveConfig);
            }
            if imported > 0 {
                push_status(
                    &mut effects,
                    model,
                    crate::app::model::AppStatus::Info(format!(
                        "Imported {} profile(s) from subscription",
                        imported
                    )),
                );
            } else {
                push_status(
                    &mut effects,
                    model,
                    crate::app::model::AppStatus::Info("No new profiles in subscription".into()),
                );
            }
            effects
        }
        Err(err) => {
            let mut effects = if managed {
                if let Some(sub) = model.config.subscriptions.iter_mut().find(|s| s.id == id)
                    && automatic
                    && sub.auto_update != SubscriptionAutoUpdate::Off
                {
                    sub.record_fetch_failure(now);
                    vec![Effect::SaveConfig]
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            };
            push_status(
                &mut effects,
                model,
                crate::app::model::AppStatus::Error(format!("Subscription failed: {}", err)),
            );
            append_download_hint(&mut effects, model, DownloadKind::Subscription);
            effects
        }
    };

    // If either the active or in-flight profile was removed from the
    // subscription, invalidate the attempt and stop any process it spawned.
    let active_missing = model
        .active_profile_id
        .is_some_and(|id| !model.config.profiles.iter().any(|p| p.id == id));
    let connecting_missing = matches!(
        model.connection,
        ConnectionState::Connecting | ConnectionState::ConnectPending
    ) && model
        .connecting_profile_id
        .is_some_and(|id| !model.config.profiles.iter().any(|p| p.id == id));
    if active_missing || connecting_missing {
        effects.push(Effect::Disconnect);
    } else {
        let active_changed = old_active_profile.as_ref().is_some_and(|old| {
            model
                .config
                .profiles
                .iter()
                .find(|p| p.id == old.id)
                .is_some_and(|new| profile_runtime_changed(old, new))
        });
        let connecting_changed = old_connecting_profile.as_ref().is_some_and(|old| {
            model
                .config
                .profiles
                .iter()
                .find(|p| p.id == old.id)
                .is_some_and(|new| profile_runtime_changed(old, new))
        });
        let reconnect_id = match model.connection {
            ConnectionState::Connected if active_changed => model.active_profile_id,
            ConnectionState::ConnectPending if connecting_changed => model.connecting_profile_id,
            _ => None,
        };
        if let Some(id) = reconnect_id
            && queue_connect(model, id)
        {
            push_status(
                &mut effects,
                model,
                AppStatus::Info("Subscription changed — reconnecting".into()),
            );
        }
    }

    // Restore cursor to the subscription header (add_profile moves it on every
    // call, so without this the focus would land on the last imported profile).
    if managed {
        if let Some(sub_idx) = model.config.subscriptions.iter().position(|s| s.id == id) {
            model.selected = crate::app::model::row_for_subscription_header(&model.config, sub_idx);
        }
    } else {
        model.selected = saved_selected;
    }

    effects
}

fn handle_geo_result(model: &mut Model, result: GeoResult) -> Vec<Effect> {
    model.geo_updating = false;
    model.geo_automatic_update = false;
    let mut effects = match result {
        GeoResult::Updated {
            parts,
            last_updated: _,
            checked_at,
            retry_state,
            service_retry_states,
            service_checked_at,
            next_update,
            service_next_updates,
            warnings,
        } => {
            model.geo_retry_state = retry_state;
            model.service_retry_states = service_retry_states;
            model.service_checked_at = service_checked_at;
            model.geo_next_update = next_update;
            model.service_next_updates = service_next_updates;
            model.geo_last_updated = Some(checked_at.format("%d %b %H:%M").to_string());
            model.geo_last_checked_at = Some(checked_at);
            let mut log_effects = Vec::new();
            for part in &parts {
                let text = format!("Updated: {}", part);
                log_effects.push(Effect::AppendAppLog {
                    level: "INFO".to_string(),
                    message: text.clone(),
                });
                model.logs.push_back(text);
            }
            for warning in &warnings {
                log_effects.push(Effect::AppendAppLog {
                    level: "ERROR".to_string(),
                    message: format!("Geo batch partial failure: {warning}"),
                });
            }
            if !warnings.is_empty() {
                append_download_hint(&mut log_effects, model, DownloadKind::Geo);
            }
            let status = if warnings.is_empty() {
                AppStatus::Info("Geo databases updated".into())
            } else {
                AppStatus::Error(format!("Geo updated partially: {}", warnings.join("; ")))
            };
            push_status(&mut log_effects, model, status);
            if model.connection == ConnectionState::Connected
                && let Some(active_id) = model.active_profile_id
                && queue_connect(model, active_id)
            {
                model
                    .logs
                    .push_back("Reconnecting to apply new geo databases".into());
            }
            log_effects
        }
        GeoResult::UpToDate {
            checked_at,
            retry_state,
            service_retry_states,
            service_checked_at,
            next_update,
            service_next_updates,
            warnings,
        } => {
            model.geo_retry_state = retry_state;
            model.service_retry_states = service_retry_states;
            model.service_checked_at = service_checked_at;
            model.geo_next_update = next_update;
            model.service_next_updates = service_next_updates;
            model.geo_last_checked_at = checked_at;
            if let Some(checked_at) = checked_at {
                model.geo_last_updated = Some(checked_at.format("%d %b %H:%M").to_string());
            }
            let mut effects = Vec::new();
            let status = if warnings.is_empty() {
                AppStatus::Info("Geo databases are up to date".into())
            } else {
                AppStatus::Error(format!(
                    "Service rule-set update failed: {}",
                    warnings.join("; ")
                ))
            };
            push_status(&mut effects, model, status);
            if !warnings.is_empty() {
                append_download_hint(&mut effects, model, DownloadKind::Geo);
            }
            effects
        }
        GeoResult::Error {
            message,
            retry_state,
            service_retry_states,
            service_checked_at,
            next_update,
            service_next_updates,
            updated_parts,
        } => {
            model.geo_retry_state = retry_state;
            model.service_retry_states = service_retry_states;
            model.service_checked_at = service_checked_at;
            model.geo_next_update = next_update;
            model.service_next_updates = service_next_updates;
            let mut effects = Vec::new();
            let has_updates = !updated_parts.is_empty();
            push_status(
                &mut effects,
                model,
                crate::app::model::AppStatus::Error(message),
            );
            append_download_hint(&mut effects, model, DownloadKind::Geo);
            for part in updated_parts {
                effects.push(Effect::AppendAppLog {
                    level: "INFO".to_string(),
                    message: format!("Updated: {part}"),
                });
            }
            if has_updates
                && model.connection == ConnectionState::Connected
                && let Some(active_id) = model.active_profile_id
            {
                queue_connect(model, active_id);
            }
            effects
        }
    };
    effects.push(Effect::BroadcastState);
    effects
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::profile::{GeoAutoUpdate, RoutingMode, SubscriptionAutoUpdate};
    use crate::test_helpers::*;
    use chrono::Timelike;
    use crossterm::event::KeyCode;

    fn after_update_window() -> chrono::DateTime<Local> {
        Local::now()
            .with_hour(9)
            .unwrap()
            .with_minute(0)
            .unwrap()
            .with_second(0)
            .unwrap()
    }

    fn app_log_info(message: &str) -> Effect {
        Effect::AppendAppLog {
            level: "INFO".to_string(),
            message: message.to_string(),
        }
    }

    fn app_log_error(message: &str) -> Effect {
        Effect::AppendAppLog {
            level: "ERROR".to_string(),
            message: message.to_string(),
        }
    }

    #[test]
    fn handle_event_non_key_is_noop() {
        let mut model = model_with_profiles(vec![]);
        let effects = update(&mut model, Msg::Resize);
        assert!(effects.is_empty());
        assert_eq!(model.overlay, Overlay::None);
    }

    #[test]
    fn normal_mode_navigates() {
        let mut model = model_with_profiles(vec![
            Profile::new_vless(
                "A".to_string(),
                "1.1.1.1".to_string(),
                443,
                "u1".to_string(),
            ),
            Profile::new_vless(
                "B".to_string(),
                "2.2.2.2".to_string(),
                443,
                "u2".to_string(),
            ),
        ]);
        assert_eq!(model.selected, 0);
        let _ = handle_sources(&mut model, key('j'));
        assert_eq!(model.selected, 1);
        let _ = handle_sources(&mut model, key('k'));
        assert_eq!(model.selected, 0);
        let _ = handle_sources(&mut model, key('G'));
        assert_eq!(model.selected, 1);
        let _ = handle_sources(&mut model, key('g'));
        assert_eq!(model.selected, 1); // a lone g is only a client-side prefix
        let _ = handle_ipc_command(&mut model, crate::app::msg::IpcCommand::GoFirst);
        assert_eq!(model.selected, 0);
    }

    #[test]
    fn normal_mode_enter_connects() {
        let a = Profile::new_vless("A".into(), "1.1.1.1".into(), 443, "u1".into());
        let b = Profile::new_vless("B".into(), "2.2.2.2".into(), 443, "u2".into());
        let a_id = a.id;
        let mut model = model_with_profiles(vec![a, b]);
        let effects = handle_sources(&mut model, KeyEvent::from(KeyCode::Enter));
        assert_eq!(model.connection, ConnectionState::Connecting);
        assert_eq!(model.connecting_profile_id, Some(a_id));
        assert_eq!(effects, vec![app_log_info("Connecting to A…")]);

        // Moving the cursor before the daemon tick must not retarget the attempt.
        model.select_next();
        let effects = handle_tick(&mut model);
        assert!(
            effects.iter().any(
                |effect| matches!(effect, Effect::Connect { profile, .. } if profile.id == a_id)
            )
        );
    }

    #[test]
    fn normal_mode_enter_no_profile() {
        let mut model = model_with_profiles(vec![]);
        let effects = handle_sources(&mut model, KeyEvent::from(KeyCode::Enter));
        assert_eq!(model.overlay, Overlay::None);
        assert_eq!(
            effects,
            vec![app_log_info("No sources. Press p to paste or e to edit.")]
        );
    }

    #[test]
    fn normal_mode_enter_on_subscription_header_does_nothing() {
        use crate::config::profile::Subscription;
        use uuid::Uuid;

        let sub_id = Uuid::new_v4();
        let mut profile = Profile::new_vless(
            "SubProfile".to_string(),
            "2.2.2.2".to_string(),
            443,
            "u2".to_string(),
        );
        profile.subscription_id = Some(sub_id);
        let mut model = model_with_profiles(vec![profile]);
        model.config.subscriptions.push(Subscription {
            id: sub_id,
            name: "Sub".to_string(),
            url: "http://example.com".to_string(),
            auto_update: SubscriptionAutoUpdate::Every1d,
            last_updated: None,
            next_auto_update: None,
            retry_state: None,
        });
        model.selected = 0; // subscription header
        let effects = handle_sources(&mut model, KeyEvent::from(KeyCode::Enter));
        assert_eq!(model.connection, ConnectionState::Idle);
        assert!(effects.is_empty());
    }

    #[test]
    fn normal_mode_enter_on_empty_subscription_updates_it() {
        use crate::config::profile::Subscription;
        use uuid::Uuid;

        let sub_id = Uuid::new_v4();
        let mut model = model_with_profiles(vec![]);
        model.config.subscriptions.push(Subscription {
            id: sub_id,
            name: "Sub".to_string(),
            url: "http://example.com".to_string(),
            auto_update: SubscriptionAutoUpdate::Every1d,
            last_updated: None,
            next_auto_update: None,
            retry_state: None,
        });
        model.selected = 0; // subscription header
        let effects = handle_sources(&mut model, KeyEvent::from(KeyCode::Enter));
        assert!(model.subscription_fetching);
        assert!(effects.contains(&Effect::UpdateSubscription { id: sub_id }));
        assert!(effects.contains(&Effect::SaveConfig));
    }

    #[test]
    fn normal_mode_d_confirms_delete() {
        let mut model = model_with_profiles(vec![Profile::new_vless(
            "A".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        let effects = handle_sources(&mut model, key('d'));
        assert_eq!(model.overlay, Overlay::ConfirmDelete);
        assert!(effects.is_empty());
    }

    #[test]
    fn normal_mode_m_opens_routing() {
        let mut model = model_with_profiles(vec![]);
        model.config.settings.geo_routing.set_region(GeoRegion::Ru);
        model
            .config
            .settings
            .geo_routing
            .set_mode(RoutingMode::Bypass(GeoRegion::Ru));
        let effects = handle_sources(&mut model, key('m'));
        assert_eq!(model.overlay, Overlay::RoutingMode);
        assert_eq!(model.routing_selected, 1);
        assert!(effects.is_empty());
    }

    #[test]
    fn ipc_command_attach_broadcasts_state() {
        let mut model = model_with_profiles(vec![]);
        let effects = handle_ipc_command(&mut model, crate::app::msg::IpcCommand::Attach);
        assert_eq!(effects, vec![Effect::BroadcastState]);
    }

    #[test]
    fn ipc_command_client_error_sets_status_and_logs() {
        let mut model = model_with_profiles(vec![]);
        let effects = handle_ipc_command(
            &mut model,
            crate::app::msg::IpcCommand::ClientError {
                message: "Edit rejected: bad UUID".into(),
            },
        );
        assert_eq!(
            effects,
            vec![
                app_log_error("Edit rejected: bad UUID"),
                Effect::BroadcastState,
            ]
        );
        assert!(model.status.is_error(), "status: {:?}", model.status);
        assert_eq!(model.status.text(), "Edit rejected: bad UUID");
        // set_status also pushes into the in-memory log panel so the message
        // survives a later status overwrite.
        assert!(model.logs.iter().any(|l| l.contains("Edit rejected")));
    }

    #[test]
    fn ipc_command_reload_config_returns_effect() {
        let mut model = model_with_profiles(vec![]);
        let effects = handle_ipc_command(&mut model, crate::app::msg::IpcCommand::ReloadConfig);
        assert_eq!(effects, vec![Effect::ReloadConfig, Effect::BroadcastState]);
    }

    // ---- semantic IPC commands (bar widget / CLI clients) ----

    fn connected_model() -> (Model, Uuid) {
        let a = Profile::new_vless("A".into(), "1.1.1.1".into(), 443, "u1".into());
        let a_id = a.id;
        let mut model = model_with_profiles(vec![a]);
        model.connection = ConnectionState::Connected;
        model.active_profile_id = Some(a_id);
        (model, a_id)
    }

    #[test]
    fn ipc_command_disconnect_when_connected() {
        let (mut model, _) = connected_model();
        let effects = handle_ipc_command(&mut model, crate::app::msg::IpcCommand::Disconnect);
        assert_eq!(effects, vec![Effect::Disconnect, Effect::BroadcastState]);
    }

    #[test]
    fn ipc_command_disconnect_when_idle_is_noop() {
        let mut model = model_with_profiles(vec![]);
        let effects = handle_ipc_command(&mut model, crate::app::msg::IpcCommand::Disconnect);
        assert_eq!(effects, vec![Effect::BroadcastState]);
    }

    #[test]
    fn ipc_command_reconnect_when_connected() {
        let (mut model, a_id) = connected_model();
        let effects = handle_ipc_command(&mut model, crate::app::msg::IpcCommand::Reconnect);
        assert_eq!(model.connection, ConnectionState::Connecting);
        assert_eq!(model.connecting_profile_id, Some(a_id));
        assert_eq!(
            effects,
            vec![app_log_info("Reconnecting to A…"), Effect::BroadcastState]
        );
    }

    #[test]
    fn ipc_command_reconnect_when_idle_is_noop() {
        let mut model = model_with_profiles(vec![]);
        let effects = handle_ipc_command(&mut model, crate::app::msg::IpcCommand::Reconnect);
        assert_eq!(effects, vec![Effect::BroadcastState]);
        assert_eq!(model.connection, ConnectionState::Idle);
    }

    #[test]
    fn ipc_command_set_routing_mode_commits_and_saves() {
        let mut model = model_with_profiles(vec![]);
        model.config.settings.geo_routing.set_region(GeoRegion::Ru);
        let effects = handle_ipc_command(
            &mut model,
            crate::app::msg::IpcCommand::SetRoutingMode {
                mode: RoutingMode::Bypass(GeoRegion::Ru),
            },
        );
        assert_eq!(
            model.config.settings.geo_routing.mode(),
            RoutingMode::Bypass(GeoRegion::Ru)
        );
        assert_eq!(
            effects,
            vec![
                Effect::SaveConfig,
                app_log_info("Routing mode: Bypass RU"),
                Effect::BroadcastState,
            ]
        );
    }

    #[test]
    fn ipc_command_set_routing_mode_reconnects_when_connected() {
        let (mut model, _) = connected_model();
        model.config.settings.geo_routing.set_region(GeoRegion::Ru);
        let effects = handle_ipc_command(
            &mut model,
            crate::app::msg::IpcCommand::SetRoutingMode {
                mode: RoutingMode::Bypass(GeoRegion::Ru),
            },
        );
        assert_eq!(model.connection, ConnectionState::Connecting);
        assert_eq!(
            effects,
            vec![
                Effect::SaveConfig,
                app_log_info("Routing mode: Bypass RU"),
                app_log_info("Mode changed to Bypass RU — reconnecting"),
                Effect::BroadcastState,
            ]
        );
    }

    #[test]
    fn ipc_command_set_routing_mode_rejects_unavailable_mode() {
        let mut model = model_with_profiles(vec![]);
        // No region selected (or Global) → only RoutingMode::Global is valid.
        let effects = handle_ipc_command(
            &mut model,
            crate::app::msg::IpcCommand::SetRoutingMode {
                mode: RoutingMode::Only(GeoRegion::Ru),
            },
        );
        assert_eq!(
            model.config.settings.geo_routing.mode(),
            RoutingMode::Global
        );
        assert!(!effects.contains(&Effect::SaveConfig));
        assert_eq!(
            effects,
            vec![
                app_log_error("Routing mode Only RU is unavailable for region GLOBAL"),
                Effect::BroadcastState,
            ]
        );
    }

    #[test]
    fn ipc_command_set_geo_region_switches_and_restores_mode() {
        let mut model = model_with_profiles(vec![]);
        model.config.settings.geo_routing.set_region(GeoRegion::Ru);
        model
            .config
            .settings
            .geo_routing
            .set_mode(RoutingMode::Bypass(GeoRegion::Ru));
        model
            .config
            .settings
            .geo_routing
            .selected_region_modes
            .insert(GeoRegion::Cn, RoutingMode::Only(GeoRegion::Cn));

        let effects = handle_ipc_command(
            &mut model,
            crate::app::msg::IpcCommand::SetGeoRegion {
                region: GeoRegion::Cn,
            },
        );

        assert_eq!(
            model.config.settings.geo_routing.current_region,
            Some(GeoRegion::Cn)
        );
        // Old region's mode was persisted, new region's restored.
        assert_eq!(
            model
                .config
                .settings
                .geo_routing
                .selected_region_modes
                .get(&GeoRegion::Ru),
            Some(&RoutingMode::Bypass(GeoRegion::Ru))
        );
        assert_eq!(
            model.config.settings.geo_routing.mode(),
            RoutingMode::Only(GeoRegion::Cn)
        );
        assert!(
            model.geo_updating,
            "missing geo databases should be fetched"
        );
        assert!(effects.contains(&Effect::SaveConfig));
        assert!(effects.contains(&Effect::DownloadGeoIfMissing));
        assert!(effects.contains(&Effect::RefreshGeoLastUpdated));
    }

    #[test]
    fn ipc_command_set_geo_region_same_region_only_saves() {
        let mut model = model_with_profiles(vec![]);
        model.config.settings.geo_routing.set_region(GeoRegion::Ru);
        let effects = handle_ipc_command(
            &mut model,
            crate::app::msg::IpcCommand::SetGeoRegion {
                region: GeoRegion::Ru,
            },
        );
        assert!(!model.geo_updating);
        assert!(!effects.contains(&Effect::DownloadGeoIfMissing));
        assert!(!effects.contains(&Effect::RefreshGeoLastUpdated));
        assert!(effects.contains(&Effect::SaveConfig));
    }

    #[test]
    fn ipc_command_set_kill_switch_applies_and_saves_via_result() {
        let mut model = model_with_profiles(vec![]);
        assert!(!model.config.settings.kill_switch);
        let effects = handle_ipc_command(
            &mut model,
            crate::app::msg::IpcCommand::SetKillSwitch { enabled: true },
        );
        assert_eq!(model.kill_switch_pending, Some(true));
        assert_eq!(
            effects,
            vec![
                app_log_info("Kill switch enabling…"),
                Effect::ApplyKillSwitch { enabled: true },
                Effect::BroadcastState,
            ]
        );

        // While the first toggle is still in flight, a second is ignored.
        let effects = handle_ipc_command(
            &mut model,
            crate::app::msg::IpcCommand::SetKillSwitch { enabled: false },
        );
        assert_eq!(effects, vec![Effect::BroadcastState]);
        assert_eq!(model.kill_switch_pending, Some(true));
    }

    #[test]
    fn ipc_command_set_kill_switch_noop_when_already_set() {
        let mut model = model_with_profiles(vec![]);
        model.config.settings.kill_switch = true;
        let effects = handle_ipc_command(
            &mut model,
            crate::app::msg::IpcCommand::SetKillSwitch { enabled: true },
        );
        assert_eq!(effects, vec![Effect::BroadcastState]);
        assert_eq!(model.kill_switch_pending, None);
    }

    #[test]
    fn ipc_command_set_auto_connect_flips_and_saves() {
        let mut model = model_with_profiles(vec![]);
        assert!(!model.config.settings.auto_connect);
        let effects = handle_ipc_command(
            &mut model,
            crate::app::msg::IpcCommand::SetAutoConnect { enabled: true },
        );
        assert!(model.config.settings.auto_connect);
        assert_eq!(
            effects,
            vec![
                Effect::SaveConfig,
                app_log_info("Auto-connect enabled"),
                Effect::BroadcastState,
            ]
        );

        // Setting the same value again is a no-op (no redundant save).
        let effects = handle_ipc_command(
            &mut model,
            crate::app::msg::IpcCommand::SetAutoConnect { enabled: true },
        );
        assert_eq!(effects, vec![Effect::BroadcastState]);
    }

    #[test]
    fn config_reloaded_updates_model() {
        let mut model = model_with_profiles(vec![Profile::new_vless(
            "A".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        model.config.settings.geo_routing.set_region(GeoRegion::Ru);
        let config = model.config.clone();
        let effects = update(&mut model, Msg::ConfigReloaded(Box::new(Ok(config))));
        assert_eq!(
            effects,
            vec![Effect::BroadcastState, app_log_info("Profiles reloaded")]
        );
    }

    #[test]
    fn config_reloaded_preserves_selected_profile() {
        let mut model = model_with_profiles(vec![
            Profile::new_vless(
                "A".to_string(),
                "1.1.1.1".to_string(),
                443,
                "u1".to_string(),
            ),
            Profile::new_vless(
                "B".to_string(),
                "2.2.2.2".to_string(),
                443,
                "u2".to_string(),
            ),
        ]);
        model.selected = 1;
        let selected_id = model.selected_profile().unwrap().id;
        let mut config = model.config.clone();
        config.profiles[1].name = "B edited".to_string();

        update(&mut model, Msg::ConfigReloaded(Box::new(Ok(config))));

        assert_eq!(model.selected, 1);
        assert_eq!(model.selected_profile().unwrap().id, selected_id);
        assert_eq!(model.selected_profile().unwrap().name, "B edited");
    }

    #[test]
    fn config_reloaded_error_updates_status() {
        let mut model = model_with_profiles(vec![]);
        let effects = update(
            &mut model,
            Msg::ConfigReloaded(Box::new(Err(crate::app::msg::IpcError::new("parse error")))),
        );
        assert_eq!(
            effects,
            vec![
                Effect::BroadcastState,
                app_log_error("Failed to reload: parse error")
            ]
        );
    }

    #[test]
    fn config_reload_disconnects_when_active_profile_was_removed() {
        let profile = Profile::new_vless("A".into(), "1.1.1.1".into(), 443, "u1".into());
        let profile_id = profile.id;
        let mut model = model_with_profiles(vec![profile]);
        model.connection = ConnectionState::Connected;
        model.active_profile_id = Some(profile_id);
        let mut config = model.config.clone();
        config.profiles.clear();

        let effects = update(&mut model, Msg::ConfigReloaded(Box::new(Ok(config))));

        assert!(effects.contains(&Effect::Disconnect));
    }

    #[test]
    fn config_reload_cancels_missing_queued_connection() {
        let profile = Profile::new_vless("A".into(), "1.1.1.1".into(), 443, "u1".into());
        let profile_id = profile.id;
        let mut model = model_with_profiles(vec![profile]);
        assert!(queue_connect(&mut model, profile_id));
        let mut config = model.config.clone();
        config.profiles.clear();

        let effects = update(&mut model, Msg::ConfigReloaded(Box::new(Ok(config))));

        assert_eq!(model.connection, ConnectionState::Connecting);
        assert_eq!(model.connecting_profile_id, Some(profile_id));
        assert!(effects.contains(&Effect::Disconnect));
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, Effect::Connect { .. }))
        );
    }

    #[test]
    fn config_reload_disconnects_missing_connect_pending_profile() {
        let profile = Profile::new_vless("A".into(), "1.1.1.1".into(), 443, "u1".into());
        let profile_id = profile.id;
        let mut model = model_with_profiles(vec![profile]);
        assert!(queue_connect(&mut model, profile_id));
        let connect_effects = handle_tick(&mut model);
        assert!(
            connect_effects
                .iter()
                .any(|effect| matches!(effect, Effect::Connect { .. }))
        );
        model.connection = ConnectionState::ConnectPending;
        let mut config = model.config.clone();
        config.profiles.clear();

        let effects = update(&mut model, Msg::ConfigReloaded(Box::new(Ok(config))));

        assert_eq!(model.connecting_profile_id, Some(profile_id));
        assert!(effects.contains(&Effect::Disconnect));
    }

    #[test]
    fn config_reload_keeps_connect_pending_profile_with_same_id() {
        let profile = Profile::new_vless("A".into(), "1.1.1.1".into(), 443, "u1".into());
        let profile_id = profile.id;
        let mut model = model_with_profiles(vec![profile]);
        assert!(queue_connect(&mut model, profile_id));
        model.connection = ConnectionState::ConnectPending;
        let mut config = model.config.clone();
        config.profiles[0].name = "A renamed".into();

        let effects = update(&mut model, Msg::ConfigReloaded(Box::new(Ok(config))));

        assert!(!effects.contains(&Effect::Disconnect));
        assert_eq!(model.connecting_profile_id, Some(profile_id));
    }

    #[test]
    fn config_reload_reconnects_changed_active_profile() {
        let profile = Profile::new_vless("A".into(), "1.1.1.1".into(), 443, "u1".into());
        let profile_id = profile.id;
        let mut model = model_with_profiles(vec![profile]);
        model.connection = ConnectionState::Connected;
        model.active_profile_id = Some(profile_id);
        let mut config = model.config.clone();
        config.profiles[0].address = "2.2.2.2".into();

        let effects = update(&mut model, Msg::ConfigReloaded(Box::new(Ok(config))));

        assert_eq!(model.connection, ConnectionState::Connecting);
        assert_eq!(model.connecting_profile_id, Some(profile_id));
        assert_eq!(model.status.text(), "Configuration changed — reconnecting");
        assert!(!effects.contains(&Effect::Disconnect));
    }

    #[test]
    fn config_reload_does_not_reconnect_for_profile_metadata() {
        let profile = Profile::new_vless("A".into(), "1.1.1.1".into(), 443, "u1".into());
        let profile_id = profile.id;
        let mut model = model_with_profiles(vec![profile]);
        model.connection = ConnectionState::Connected;
        model.active_profile_id = Some(profile_id);
        let mut config = model.config.clone();
        config.profiles[0].name = "Renamed".into();
        config.profiles[0].tags.push("metadata".into());

        let effects = update(&mut model, Msg::ConfigReloaded(Box::new(Ok(config))));

        assert_eq!(model.connection, ConnectionState::Connected);
        assert_eq!(model.connecting_profile_id, None);
        assert_eq!(model.status.text(), "Profiles reloaded");
        assert!(!effects.contains(&Effect::Disconnect));
    }

    #[test]
    fn config_reload_reconnects_for_runtime_settings_only() {
        let profile = Profile::new_vless("A".into(), "1.1.1.1".into(), 443, "u1".into());
        let profile_id = profile.id;
        let mut model = model_with_profiles(vec![profile]);
        model.connection = ConnectionState::Connected;
        model.active_profile_id = Some(profile_id);
        let mut config = model.config.clone();
        config.settings.tun_interface = "tun9".into();

        update(&mut model, Msg::ConfigReloaded(Box::new(Ok(config))));

        assert_eq!(model.connection, ConnectionState::Connecting);
        assert_eq!(model.connecting_profile_id, Some(profile_id));
    }

    #[test]
    fn connection_settings_comparison_covers_singbox_inputs() {
        use crate::config::profile::{DnsStrategy, GeoRegion, RoutedService, ServiceRoute};

        let original = crate::config::profile::Settings::default();
        let mut changed = original.clone();
        changed.dns.strategy = DnsStrategy::OnlyIpv6;
        assert!(connection_settings_changed(&original, &changed));

        let mut changed = original.clone();
        changed.logs.level = "debug".into();
        assert!(connection_settings_changed(&original, &changed));

        let mut changed = original.clone();
        changed.geo_routing.set_region(GeoRegion::Ru);
        changed
            .geo_routing
            .set_mode(crate::config::profile::RoutingMode::Only(GeoRegion::Ru));
        assert!(connection_settings_changed(&original, &changed));

        let mut changed = original.clone();
        changed
            .geo_routing
            .service_routes
            .insert(RoutedService::Steam, ServiceRoute::Proxy);
        assert!(connection_settings_changed(&original, &changed));
    }

    #[test]
    fn config_reload_ignores_ui_settings_and_external_kill_switch_value() {
        let profile = Profile::new_vless("A".into(), "1.1.1.1".into(), 443, "u1".into());
        let profile_id = profile.id;
        let mut model = model_with_profiles(vec![profile]);
        model.connection = ConnectionState::Connected;
        model.active_profile_id = Some(profile_id);
        let mut config = model.config.clone();
        config.settings.theme = "gruvbox".into();
        config.settings.auto_connect = !config.settings.auto_connect;
        config.settings.kill_switch = !config.settings.kill_switch;

        update(&mut model, Msg::ConfigReloaded(Box::new(Ok(config))));

        assert_eq!(model.connection, ConnectionState::Connected);
        assert!(!model.config.settings.kill_switch);
        assert_eq!(
            model.status.text(),
            "Kill switch edit ignored — use K to change it"
        );
    }

    #[test]
    fn config_reload_reports_ignored_kill_switch_alongside_reconnect() {
        let profile = Profile::new_vless("A".into(), "1.1.1.1".into(), 443, "u1".into());
        let profile_id = profile.id;
        let mut model = model_with_profiles(vec![profile]);
        model.connection = ConnectionState::Connected;
        model.active_profile_id = Some(profile_id);
        let mut config = model.config.clone();
        config.settings.kill_switch = true;
        config.settings.tun_interface = "tun9".into();

        let effects = update(&mut model, Msg::ConfigReloaded(Box::new(Ok(config))));

        assert_eq!(model.connection, ConnectionState::Connecting);
        assert!(!model.config.settings.kill_switch);
        assert_eq!(
            model.status.text(),
            "Kill switch edit ignored (use K); configuration changed — reconnecting"
        );
        assert!(effects.contains(&app_log_info(
            "Kill switch edit ignored (use K); configuration changed — reconnecting"
        )));
    }

    #[test]
    fn config_reload_requeues_changed_connect_pending_attempt() {
        let profile = Profile::new_vless("A".into(), "1.1.1.1".into(), 443, "u1".into());
        let profile_id = profile.id;
        let mut model = model_with_profiles(vec![profile]);
        assert!(queue_connect(&mut model, profile_id));
        model.connection = ConnectionState::ConnectPending;
        let old_attempt = model.connect_attempt_id;
        let mut config = model.config.clone();
        config.profiles[0].port = 8443;

        update(&mut model, Msg::ConfigReloaded(Box::new(Ok(config))));

        assert_eq!(model.connection, ConnectionState::Connecting);
        assert_eq!(model.connect_attempt_id, old_attempt + 1);
        assert_eq!(model.connecting_profile_id, Some(profile_id));
    }

    #[test]
    fn config_reload_defers_service_route_reconnect_until_assets_ready() {
        use crate::config::profile::{RoutedService, ServiceRoute};

        let profile = Profile::new_vless("A".into(), "1.1.1.1".into(), 443, "u1".into());
        let profile_id = profile.id;
        let mut model = model_with_profiles(vec![profile]);
        model.connection = ConnectionState::Connected;
        model.active_profile_id = Some(profile_id);
        let mut config = model.config.clone();
        config
            .settings
            .geo_routing
            .service_routes
            .insert(RoutedService::Steam, ServiceRoute::Proxy);

        let effects = update(&mut model, Msg::ConfigReloaded(Box::new(Ok(config))));

        assert_eq!(model.connection, ConnectionState::Connected);
        assert!(model.pending_service_reconnect);
        assert!(effects.contains(&Effect::DownloadServiceRuleSetsIfMissing));
    }

    #[test]
    fn ipc_command_key_navigates() {
        let mut model = model_with_profiles(vec![
            Profile::new_vless(
                "A".to_string(),
                "1.1.1.1".to_string(),
                443,
                "u1".to_string(),
            ),
            Profile::new_vless(
                "B".to_string(),
                "2.2.2.2".to_string(),
                443,
                "u2".to_string(),
            ),
        ]);
        let effects = handle_ipc_command(
            &mut model,
            crate::app::msg::IpcCommand::Key {
                code: "Char".into(),
                char: Some('j'),
                ctrl: false,
            },
        );
        assert_eq!(effects, vec![Effect::BroadcastState]);
        assert_eq!(model.selected, 1);
    }

    #[test]
    fn rebuild_key_event_handles_tab_and_backtab() {
        use crossterm::event::{KeyCode, KeyModifiers};

        let tab = rebuild_key_event("Tab", None, false).unwrap();
        assert_eq!(tab.code, KeyCode::Tab);
        assert_eq!(tab.modifiers, KeyModifiers::empty());

        let backtab = rebuild_key_event("BackTab", None, false).unwrap();
        assert_eq!(backtab.code, KeyCode::BackTab);
        assert_eq!(backtab.modifiers, KeyModifiers::empty());
    }

    #[test]
    fn help_mode_any_key_returns_to_normal() {
        let mut model = model_with_profiles(vec![]);
        model.overlay = Overlay::Help;
        let effects = handle_key(&mut model, key('x'));
        assert_eq!(model.overlay, Overlay::None);
        assert!(effects.is_empty());
    }

    #[test]
    fn confirm_delete_yes() {
        let mut model = model_with_profiles(vec![Profile::new_vless(
            "A".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        model.overlay = Overlay::ConfirmDelete;
        let effects = handle_confirm_delete(&mut model, key('y'));
        assert!(model.config.profiles.is_empty());
        assert_eq!(model.overlay, Overlay::None);
        assert_eq!(
            effects,
            vec![Effect::SaveConfig, app_log_info("Profile 'A' deleted")]
        );
    }

    #[test]
    fn confirm_delete_no() {
        let mut model = model_with_profiles(vec![Profile::new_vless(
            "A".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        model.overlay = Overlay::ConfirmDelete;
        let effects = handle_confirm_delete(&mut model, key('n'));
        assert_eq!(model.config.profiles.len(), 1);
        assert_eq!(model.overlay, Overlay::None);
        assert!(effects.is_empty());
    }

    #[test]
    fn routing_mode_navigates() {
        let mut model = model_with_profiles(vec![]);
        model.config.settings.geo_routing.set_region(GeoRegion::Ru);
        model.overlay = Overlay::RoutingMode;
        model.routing_selected = 0;

        let _ = handle_routing_mode(&mut model, key('j'));
        assert_eq!(model.routing_selected, 1);
        let _ = handle_routing_mode(&mut model, key('j'));
        assert_eq!(model.routing_selected, 2);
        let _ = handle_routing_mode(&mut model, key('j'));
        assert_eq!(model.routing_selected, 2); // clamp

        let _ = handle_routing_mode(&mut model, key('k'));
        assert_eq!(model.routing_selected, 1);
        let _ = handle_routing_mode(&mut model, key('g'));
        assert_eq!(model.routing_selected, 1);
        let _ = handle_ipc_command(&mut model, crate::app::msg::IpcCommand::GoFirst);
        assert_eq!(model.routing_selected, 0);
        let _ = handle_routing_mode(&mut model, key('G'));
        assert_eq!(model.routing_selected, 2);
    }

    #[test]
    fn routing_mode_enter_changes_mode() {
        let mut model = model_with_profiles(vec![]);
        model.config.settings.geo_routing.set_region(GeoRegion::Ru);
        model.overlay = Overlay::RoutingMode;
        model.routing_selected = 2; // OnlyRu

        let effects = handle_routing_mode(&mut model, KeyEvent::from(KeyCode::Enter));
        assert_eq!(
            model.config.settings.geo_routing.mode(),
            RoutingMode::Only(GeoRegion::Ru)
        );
        assert_eq!(model.overlay, Overlay::None);
        assert!(model.status.text().contains("Only RU"));
        assert_eq!(
            effects,
            vec![Effect::SaveConfig, app_log_info("Routing mode: Only RU")]
        );
    }

    #[test]
    fn routing_mode_change_queues_active_profile_not_cursor() {
        let a = Profile::new_vless("A".into(), "1.1.1.1".into(), 443, "u1".into());
        let b = Profile::new_vless("B".into(), "2.2.2.2".into(), 443, "u2".into());
        let active_id = a.id;
        let mut model = model_with_profiles(vec![a, b]);
        model.config.settings.geo_routing.set_region(GeoRegion::Ru);
        model.connection = ConnectionState::Connected;
        model.active_profile_id = Some(active_id);
        model.select_next();
        model.overlay = Overlay::RoutingMode;
        model.routing_selected = 1;

        handle_routing_mode(&mut model, KeyEvent::from(KeyCode::Enter));

        assert_eq!(model.connecting_profile_id, Some(active_id));
    }

    #[test]
    fn routing_mode_esc_cancels() {
        let mut model = model_with_profiles(vec![]);
        model.config.settings.geo_routing.set_region(GeoRegion::Ru);
        model.overlay = Overlay::RoutingMode;
        model.routing_selected = 2;
        let effects = handle_routing_mode(&mut model, KeyEvent::from(KeyCode::Esc));
        assert_eq!(model.overlay, Overlay::None);
        assert!(effects.is_empty());
    }

    #[test]
    fn geo_region_navigates() {
        let mut model = model_with_profiles(vec![]);
        model.overlay = Overlay::GeoRegions;
        model.geo_region_selected = 0;

        let _ = handle_geo_region(&mut model, key('j'));
        assert_eq!(model.geo_region_selected, 1);
        let _ = handle_geo_region(&mut model, key('j'));
        assert_eq!(model.geo_region_selected, 2);
        let _ = handle_geo_region(&mut model, key('j'));
        assert_eq!(model.geo_region_selected, 3);
        let _ = handle_geo_region(&mut model, key('j'));
        assert_eq!(model.geo_region_selected, 3); // clamp

        let _ = handle_geo_region(&mut model, key('k'));
        assert_eq!(model.geo_region_selected, 2);
        let _ = handle_geo_region(&mut model, key('g'));
        assert_eq!(model.geo_region_selected, 2);
        let _ = handle_ipc_command(&mut model, crate::app::msg::IpcCommand::GoFirst);
        assert_eq!(model.geo_region_selected, 0);
        let _ = handle_geo_region(&mut model, key('G'));
        assert_eq!(model.geo_region_selected, 3);
    }

    #[test]
    fn geo_region_enter_changes_region() {
        let mut model = model_with_profiles(vec![]);
        model.overlay = Overlay::GeoRegions;
        model.geo_region_selected = 1; // Cn

        let effects = handle_geo_region(&mut model, KeyEvent::from(KeyCode::Enter));
        assert_eq!(
            model.config.settings.geo_routing.current_region,
            Some(GeoRegion::Cn)
        );
        assert_eq!(model.overlay, Overlay::None);
        assert!(model.logs.iter().any(|l| l.contains("Geo region: cn")));
        assert_eq!(
            effects,
            vec![
                Effect::SaveConfig,
                Effect::RefreshGeoLastUpdated,
                app_log_info("Geo region: cn"),
                app_log_info("Checking geo databases..."),
                Effect::DownloadGeoIfMissing,
            ]
        );
    }

    #[test]
    fn geo_region_change_queues_active_profile_not_cursor() {
        let a = Profile::new_vless("A".into(), "1.1.1.1".into(), 443, "u1".into());
        let b = Profile::new_vless("B".into(), "2.2.2.2".into(), 443, "u2".into());
        let active_id = a.id;
        let mut model = model_with_profiles(vec![a, b]);
        model.config.settings.geo_routing.set_region(GeoRegion::Ru);
        model.config.settings.auto_connect = true;
        model.config.settings.last_connected_profile = Some(model.config.profiles[1].id);
        model.connection = ConnectionState::Connected;
        model.active_profile_id = Some(active_id);
        model.select_next();
        model.overlay = Overlay::GeoRegions;
        model.geo_region_selected = 1;

        handle_geo_region(&mut model, KeyEvent::from(KeyCode::Enter));

        assert_eq!(model.connecting_profile_id, Some(active_id));
    }

    #[test]
    fn geo_region_esc_blocked_when_none() {
        let mut model = model_with_profiles(vec![]);
        model.overlay = Overlay::GeoRegions;

        let effects = handle_geo_region(&mut model, KeyEvent::from(KeyCode::Esc));
        assert_eq!(model.overlay, Overlay::GeoRegions);
        assert!(effects.is_empty());
    }

    #[test]
    fn geo_region_esc_allowed_when_some() {
        let mut model = model_with_profiles(vec![]);
        model.overlay = Overlay::GeoRegions;
        model.config.settings.geo_routing.set_region(GeoRegion::Ru);

        let effects = handle_geo_region(&mut model, KeyEvent::from(KeyCode::Esc));
        assert_eq!(model.overlay, Overlay::None);
        assert!(effects.is_empty());
    }

    #[test]
    fn geo_region_change_resets_incompatible_routing_mode() {
        let mut model = model_with_profiles(vec![]);
        model.config.settings.geo_routing.set_region(GeoRegion::Ru);
        model
            .config
            .settings
            .geo_routing
            .set_mode(RoutingMode::Only(GeoRegion::Ru));
        model.overlay = Overlay::GeoRegions;
        model.geo_region_selected = 3; // Global

        let effects = handle_geo_region(&mut model, KeyEvent::from(KeyCode::Enter));
        assert_eq!(
            model.config.settings.geo_routing.current_region,
            Some(GeoRegion::Global)
        );
        assert_eq!(
            model.config.settings.geo_routing.mode(),
            RoutingMode::Global
        );
        assert_eq!(
            model
                .config
                .settings
                .geo_routing
                .selected_region_modes
                .get(&GeoRegion::Ru)
                .copied()
                .unwrap_or(RoutingMode::Global),
            RoutingMode::Only(GeoRegion::Ru),
            "previous region's routing mode should be preserved"
        );
        assert_eq!(
            effects,
            vec![
                Effect::SaveConfig,
                Effect::RefreshGeoLastUpdated,
                app_log_info("Geo region: global"),
                app_log_info("Routing mode: Global")
            ]
        );
    }

    #[test]
    fn routing_mode_persists_per_region() {
        let mut model = model_with_profiles(vec![]);
        model.config.settings.geo_routing.set_region(GeoRegion::Ru);
        model
            .config
            .settings
            .geo_routing
            .set_mode(RoutingMode::Bypass(GeoRegion::Ru));
        model.overlay = Overlay::GeoRegions;
        model.geo_region_selected = 1; // Cn

        // Switch to Cn: routing mode falls back to Global.
        let effects = handle_geo_region(&mut model, KeyEvent::from(KeyCode::Enter));
        assert_eq!(
            model.config.settings.geo_routing.current_region,
            Some(GeoRegion::Cn)
        );
        assert_eq!(
            model.config.settings.geo_routing.mode(),
            RoutingMode::Global
        );
        assert!(effects.contains(&Effect::DownloadGeoIfMissing));

        // Switch back to Ru: routing mode is restored to BypassRu.
        model.overlay = Overlay::GeoRegions;
        model.geo_region_selected = 0; // Ru
        let effects = handle_geo_region(&mut model, KeyEvent::from(KeyCode::Enter));
        assert_eq!(
            model.config.settings.geo_routing.current_region,
            Some(GeoRegion::Ru)
        );
        assert_eq!(
            model.config.settings.geo_routing.mode(),
            RoutingMode::Bypass(GeoRegion::Ru)
        );
        assert!(effects.iter().any(|e| matches!(e, Effect::AppendAppLog { message, .. } if message.contains("Routing mode: Bypass RU"))));
    }

    #[test]
    fn routing_mode_change_is_stored_per_region() {
        let mut model = model_with_profiles(vec![]);
        model.config.settings.geo_routing.set_region(GeoRegion::Ru);
        model.overlay = Overlay::RoutingMode;
        model.routing_selected = 1; // BypassRu

        handle_routing_mode(&mut model, KeyEvent::from(KeyCode::Enter));
        assert_eq!(
            model.config.settings.geo_routing.mode(),
            RoutingMode::Bypass(GeoRegion::Ru)
        );
        assert_eq!(
            model
                .config
                .settings
                .geo_routing
                .selected_region_modes
                .get(&GeoRegion::Ru)
                .copied()
                .unwrap_or(RoutingMode::Global),
            RoutingMode::Bypass(GeoRegion::Ru)
        );
    }

    #[test]
    fn geo_region_triggers_auto_connect_after_selection() {
        let mut model = model_with_profiles(vec![Profile::new_vless(
            "Auto".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        let id = model.config.profiles[0].id;
        model.config.settings.auto_connect = true;
        model.config.settings.last_connected_profile = Some(id);
        model.overlay = Overlay::GeoRegions;
        model.geo_region_selected = 0; // Ru

        let effects = handle_geo_region(&mut model, KeyEvent::from(KeyCode::Enter));
        assert_eq!(
            model.config.settings.geo_routing.current_region,
            Some(GeoRegion::Ru)
        );
        assert_eq!(model.connection, ConnectionState::Connecting);
        assert_eq!(model.connecting_profile_id, Some(id));
        assert_eq!(model.selected, 0);
        assert!(model.status.text().contains("Auto-connecting"));
        assert_eq!(
            effects,
            vec![
                Effect::SaveConfig,
                Effect::RefreshGeoLastUpdated,
                app_log_info("Geo region: ru"),
                app_log_info("Checking geo databases..."),
                Effect::DownloadGeoIfMissing,
                app_log_info("Auto-connecting to Auto…")
            ]
        );
    }

    #[test]
    fn geo_region_same_region_does_not_refresh_last_updated() {
        let mut model = model_with_profiles(vec![]);
        model.config.settings.geo_routing.set_region(GeoRegion::Cn);
        model.overlay = Overlay::GeoRegions;
        model.geo_region_selected = 1; // Cn

        let effects = handle_geo_region(&mut model, KeyEvent::from(KeyCode::Enter));
        assert_eq!(
            model.config.settings.geo_routing.current_region,
            Some(GeoRegion::Cn)
        );
        assert!(!effects.contains(&Effect::RefreshGeoLastUpdated));
    }

    #[test]
    fn geo_region_global_does_not_trigger_geo_download() {
        let mut model = model_with_profiles(vec![]);
        model.config.settings.geo_routing.set_region(GeoRegion::Ru);
        model.overlay = Overlay::GeoRegions;
        model.geo_region_selected = 3; // Global

        let effects = handle_geo_region(&mut model, KeyEvent::from(KeyCode::Enter));
        assert_eq!(
            model.config.settings.geo_routing.current_region,
            Some(GeoRegion::Global)
        );
        assert!(!effects.contains(&Effect::DownloadGeoIfMissing));
        assert!(!model.status.text().contains("Checking geo databases"));
    }

    #[test]
    fn geo_last_updated_message_updates_model() {
        let mut model = model_with_profiles(vec![]);
        model.geo_last_updated = None;
        let effects = update(
            &mut model,
            Msg::GeoMetadataRefreshed {
                last_updated: Some("2026-06-15 08:00".to_string()),
                last_checked_at: None,
                retry_state: None,
                service_retry_states: Default::default(),
                service_checked_at: Default::default(),
                next_update: None,
                service_next_updates: Default::default(),
            },
        );
        assert_eq!(model.geo_last_updated, Some("2026-06-15 08:00".to_string()));
        assert_eq!(effects, vec![Effect::BroadcastState]);
    }

    #[test]
    fn normal_mode_u_in_global_updates_enabled_services() {
        let mut model = model_with_profiles(vec![]);
        model
            .config
            .settings
            .geo_routing
            .set_region(GeoRegion::Global);
        model.config.settings.geo_routing.service_routes.insert(
            RoutedService::Telegram,
            crate::config::profile::ServiceRoute::Proxy,
        );
        model.connection = ConnectionState::Connected;

        let effects = handle_sources(&mut model, key('u'));
        assert!(model.geo_updating);
        assert!(!effects.contains(&Effect::DownloadGeo));
        assert!(effects.contains(&Effect::RetryServiceRuleSets {
            services: vec![RoutedService::Telegram],
        }));
    }

    #[test]
    fn normal_mode_u_in_global_without_services_is_noop() {
        let mut model = model_with_profiles(vec![]);
        model
            .config
            .settings
            .geo_routing
            .set_region(GeoRegion::Global);

        let effects = handle_sources(&mut model, key('u'));
        assert!(!model.geo_updating);
        assert!(!effects.contains(&Effect::DownloadGeo));
        assert!(model.status.text().contains("No enabled service"));
    }

    #[test]
    fn geo_result_updated_broadcasts_state() {
        let mut model = model_with_profiles(vec![]);
        model.geo_updating = true;
        let effects = update(
            &mut model,
            Msg::GeoUpdated(GeoResult::Updated {
                parts: vec!["geoip".into()],
                last_updated: Some("2026-05-31 13:41".to_string()),
                checked_at: Local::now(),
                retry_state: None,
                service_retry_states: Default::default(),
                service_checked_at: Default::default(),
                next_update: None,
                service_next_updates: Default::default(),
                warnings: Vec::new(),
            }),
        );
        assert!(!model.geo_updating);
        assert_eq!(
            effects,
            vec![
                app_log_info("Updated: geoip"),
                app_log_info("Geo databases updated"),
                Effect::BroadcastState
            ]
        );
    }

    #[test]
    fn geo_update_queues_active_profile_not_cursor() {
        let a = Profile::new_vless("A".into(), "1.1.1.1".into(), 443, "u1".into());
        let b = Profile::new_vless("B".into(), "2.2.2.2".into(), 443, "u2".into());
        let active_id = a.id;
        let mut model = model_with_profiles(vec![a, b]);
        model.connection = ConnectionState::Connected;
        model.active_profile_id = Some(active_id);
        model.select_next();

        update(
            &mut model,
            Msg::GeoUpdated(GeoResult::Updated {
                parts: vec!["geoip".into()],
                last_updated: None,
                checked_at: Local::now(),
                retry_state: None,
                service_retry_states: Default::default(),
                service_checked_at: Default::default(),
                next_update: None,
                service_next_updates: Default::default(),
                warnings: Vec::new(),
            }),
        );

        assert_eq!(model.connecting_profile_id, Some(active_id));
    }

    #[test]
    fn geo_result_up_to_date_broadcasts_state() {
        let mut model = model_with_profiles(vec![]);
        model.geo_updating = true;
        let effects = update(
            &mut model,
            Msg::GeoUpdated(GeoResult::UpToDate {
                checked_at: Some(Local::now()),
                retry_state: None,
                service_retry_states: Default::default(),
                service_checked_at: Default::default(),
                next_update: None,
                service_next_updates: Default::default(),
                warnings: Vec::new(),
            }),
        );
        assert!(!model.geo_updating);
        assert_eq!(
            effects,
            vec![
                app_log_info("Geo databases are up to date"),
                Effect::BroadcastState
            ]
        );
    }

    #[test]
    fn geo_result_error_broadcasts_state() {
        let mut model = model_with_profiles(vec![]);
        model.geo_updating = true;
        let retry_state = crate::geo::GeoRetryState {
            consecutive_failures: 1,
            retry_at: Local::now() + chrono::Duration::minutes(1),
            attempt_date: None,
        };
        let effects = update(
            &mut model,
            Msg::GeoUpdated(GeoResult::Error {
                message: "net fail".into(),
                retry_state: Some(retry_state),
                service_retry_states: Default::default(),
                service_checked_at: Default::default(),
                next_update: None,
                service_next_updates: Default::default(),
                updated_parts: Vec::new(),
            }),
        );
        assert!(!model.geo_updating);
        assert_eq!(model.geo_retry_state, Some(retry_state));
        assert_eq!(
            effects,
            vec![
                app_log_error("net fail"),
                Effect::AppendAppLog {
                    level: "WARN".into(),
                    message:
                        "VPN is disconnected. Try connecting to VPN and retrying the geo download."
                            .into(),
                },
                Effect::BroadcastState,
            ]
        );
    }

    #[test]
    fn geo_auto_update_due_without_previous_check() {
        let mut model = model_with_profiles(vec![]);
        model.config.settings.geo_routing.set_region(GeoRegion::Ru);
        model.config.settings.geo_routing.auto_update = GeoAutoUpdate::Every1d;

        let effects = handle_tick_at(&mut model, after_update_window());

        assert!(effects.contains(&Effect::DownloadGeo));
        assert!(model.geo_updating);
        assert!(model.geo_last_attempt_at.is_some());
    }

    #[test]
    fn geo_auto_update_respects_interval_and_retries_after_interval() {
        let mut model = model_with_profiles(vec![]);
        model.config.settings.geo_routing.set_region(GeoRegion::Ru);
        model.config.settings.geo_routing.auto_update = GeoAutoUpdate::Every1d;
        let now = after_update_window();
        model.geo_last_checked_at = Some(now - chrono::Duration::try_hours(23).unwrap());
        assert!(!geo_update_due(&model, now));

        model.geo_last_checked_at = Some(now - chrono::Duration::try_hours(25).unwrap());
        assert!(geo_update_due(&model, now));

        model.geo_last_attempt_at = Some(now - chrono::Duration::try_hours(1).unwrap());
        assert!(!geo_update_due(&model, now));
        model.geo_last_attempt_at = Some(now - chrono::Duration::try_hours(25).unwrap());
        assert!(geo_update_due(&model, now));
    }

    #[test]
    fn geo_auto_update_honors_retry_deadline_before_normal_interval() {
        let mut model = model_with_profiles(vec![]);
        model.config.settings.geo_routing.set_region(GeoRegion::Ru);
        model.config.settings.geo_routing.auto_update = GeoAutoUpdate::Every1d;
        let now = after_update_window();
        model.geo_last_checked_at = Some(now - chrono::Duration::days(2));
        model.geo_retry_state = Some(crate::geo::GeoRetryState {
            consecutive_failures: 2,
            retry_at: now + chrono::Duration::minutes(5),
            attempt_date: None,
        });

        assert!(!geo_update_due(&model, now));
        assert!(!geo_update_due(&model, now + chrono::Duration::minutes(4)));
        assert!(geo_update_due(&model, now + chrono::Duration::minutes(5)));
    }

    #[test]
    fn service_retry_is_independent_from_region_schedule() {
        let mut model = model_with_profiles(vec![]);
        model.connection = ConnectionState::Connected;
        model.config.settings.geo_routing.set_region(GeoRegion::Ru);
        model.config.settings.geo_routing.auto_update = GeoAutoUpdate::Off;
        model.config.settings.geo_routing.service_routes.insert(
            crate::config::profile::RoutedService::Telegram,
            crate::config::profile::ServiceRoute::Proxy,
        );
        model.service_retry_states.insert(
            crate::config::profile::RoutedService::Telegram,
            crate::geo::GeoRetryState {
                consecutive_failures: 2,
                retry_at: Local::now() - chrono::Duration::seconds(1),
                attempt_date: None,
            },
        );

        let effects = handle_tick(&mut model);

        assert!(effects.contains(&Effect::RetryServiceRuleSets {
            services: vec![crate::config::profile::RoutedService::Telegram],
        }));
        assert!(!effects.contains(&Effect::DownloadGeo));
    }

    #[test]
    fn global_auto_update_checks_enabled_services_only() {
        let mut model = model_with_profiles(vec![]);
        model.connection = ConnectionState::Connected;
        model
            .config
            .settings
            .geo_routing
            .set_region(GeoRegion::Global);
        model.config.settings.geo_routing.auto_update = GeoAutoUpdate::Every1d;
        model.config.settings.geo_routing.service_routes.insert(
            RoutedService::Telegram,
            crate::config::profile::ServiceRoute::Proxy,
        );

        let effects = handle_tick_at(&mut model, after_update_window());
        assert!(!effects.contains(&Effect::DownloadGeo));
        assert!(effects.contains(&Effect::RetryServiceRuleSets {
            services: vec![RoutedService::Telegram],
        }));
    }

    #[test]
    fn global_auto_update_skips_fresh_service_check() {
        let mut model = model_with_profiles(vec![]);
        model.connection = ConnectionState::Connected;
        model
            .config
            .settings
            .geo_routing
            .set_region(GeoRegion::Global);
        model.config.settings.geo_routing.auto_update = GeoAutoUpdate::Every1d;
        model.config.settings.geo_routing.service_routes.insert(
            RoutedService::Telegram,
            crate::config::profile::ServiceRoute::Proxy,
        );
        model
            .service_checked_at
            .insert(RoutedService::Telegram, Local::now());

        let effects = handle_tick(&mut model);
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, Effect::RetryServiceRuleSets { .. }))
        );
    }

    #[test]
    fn geo_auto_update_waits_for_vpn_when_kill_switch_is_enabled() {
        let mut model = model_with_profiles(vec![]);
        model.config.settings.geo_routing.set_region(GeoRegion::Ru);
        model.config.settings.geo_routing.auto_update = GeoAutoUpdate::Every1d;
        model.config.settings.kill_switch = true;
        let now = after_update_window();

        model.connection = ConnectionState::Connecting;
        assert!(!geo_update_due(&model, now));

        model.connection = ConnectionState::ConnectPending;
        assert!(!geo_update_due(&model, now));

        model.connection = ConnectionState::Connected;
        assert!(geo_update_due(&model, now));
    }

    #[test]
    fn geo_auto_update_skips_off_global_and_in_flight() {
        let mut model = model_with_profiles(vec![]);
        let now = Local::now();
        model.config.settings.geo_routing.set_region(GeoRegion::Ru);
        assert!(!geo_update_due(&model, now));

        model.config.settings.geo_routing.auto_update = GeoAutoUpdate::Every1d;
        model
            .config
            .settings
            .geo_routing
            .set_region(GeoRegion::Global);
        assert!(!geo_update_due(&model, now));

        model.config.settings.geo_routing.set_region(GeoRegion::Ru);
        model.geo_updating = true;
        assert!(!geo_update_due(&model, now));
    }

    #[test]
    fn shift_i_cycles_geo_auto_update_and_saves() {
        let mut model = model_with_profiles(vec![]);
        for expected in [
            GeoAutoUpdate::Every1d,
            GeoAutoUpdate::Every3d,
            GeoAutoUpdate::Every7d,
            GeoAutoUpdate::Off,
        ] {
            let effects = handle_sources(&mut model, key('I'));
            assert_eq!(model.config.settings.geo_routing.auto_update, expected);
            assert!(effects.contains(&Effect::SaveConfig));
            assert!(model.status.text().contains(expected.label()));
        }
    }

    #[test]
    fn tick_idle_fallback_broadcasts_state() {
        let mut model = Model::test_new(crate::config::profile::Config::default());
        model.connection = ConnectionState::Connecting;
        let effects = handle_tick(&mut model);
        assert_eq!(model.connection, ConnectionState::Idle);
        assert_eq!(effects, vec![Effect::BroadcastState]);
    }

    #[test]
    fn connected_mode_s_disconnects() {
        let mut model = model_with_profiles(vec![]);
        model.connection = ConnectionState::Connected;
        model.overlay = Overlay::None;
        let effects = handle_key(&mut model, key('s'));
        assert_eq!(effects, vec![Effect::Disconnect]);
    }

    #[test]
    fn connected_mode_navigates() {
        let mut model = model_with_profiles(vec![
            Profile::new_vless(
                "A".to_string(),
                "1.1.1.1".to_string(),
                443,
                "u1".to_string(),
            ),
            Profile::new_vless(
                "B".to_string(),
                "2.2.2.2".to_string(),
                443,
                "u2".to_string(),
            ),
        ]);
        model.connection = ConnectionState::Connected;
        model.overlay = Overlay::None;
        assert_eq!(model.selected, 0);
        let _ = handle_key(&mut model, key('j'));
        assert_eq!(model.selected, 1);
        let _ = handle_key(&mut model, key('k'));
        assert_eq!(model.selected, 0);
        let _ = handle_key(&mut model, key('G'));
        assert_eq!(model.selected, 1);
        let _ = handle_key(&mut model, key('g'));
        assert_eq!(model.selected, 1);
        let _ = handle_ipc_command(&mut model, crate::app::msg::IpcCommand::GoFirst);
        assert_eq!(model.selected, 0);
    }

    #[test]
    fn connected_mode_enter_connects() {
        let mut model = model_with_profiles(vec![Profile::new_vless(
            "A".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        model.connection = ConnectionState::Connected;
        model.overlay = Overlay::None;
        let effects = handle_key(&mut model, KeyEvent::from(KeyCode::Enter));
        assert_eq!(effects, vec![app_log_info("Connecting to A…")]);
        assert_eq!(model.connection, ConnectionState::Connecting);
    }

    #[test]
    fn enter_on_active_profile_is_noop() {
        let profile = Profile::new_vless(
            "A".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        );
        let profile_id = profile.id;
        let mut model = model_with_profiles(vec![profile]);
        model.connection = ConnectionState::Connected;
        model.active_profile_id = Some(profile_id);
        model.overlay = Overlay::None;
        let effects = handle_key(&mut model, KeyEvent::from(KeyCode::Enter));
        assert!(effects.is_empty());
        assert_eq!(model.connection, ConnectionState::Connected);
    }

    #[test]
    fn connected_mode_r_reconnects() {
        let a = Profile::new_vless("A".into(), "1.1.1.1".into(), 443, "u1".into());
        let b = Profile::new_vless("B".into(), "2.2.2.2".into(), 443, "u2".into());
        let a_id = a.id;
        let mut model = model_with_profiles(vec![a, b]);
        model.connection = ConnectionState::Connected;
        model.active_profile_id = Some(a_id);
        model.overlay = Overlay::None;
        model.select_next();
        let effects = handle_key(&mut model, key('r'));
        assert_eq!(effects, vec![app_log_info("Reconnecting to A…")]);
        assert_eq!(model.connection, ConnectionState::Connecting);
        assert_eq!(model.connecting_profile_id, Some(a_id));
    }

    #[test]
    fn connected_mode_help() {
        let mut model = model_with_profiles(vec![]);
        model.connection = ConnectionState::Connected;
        model.overlay = Overlay::None;
        let effects = handle_key(&mut model, key('?'));
        assert!(effects.is_empty());
        assert_eq!(model.overlay, Overlay::Help);
    }

    #[test]
    fn connect_failed_sets_status_error() {
        let mut model = Model::test_new(crate::config::profile::Config::default());
        model.connection = ConnectionState::ConnectPending;
        model.connect_attempt_id = 7;
        model.singbox_pid = Some(99);
        model.active_profile_id = Some(Uuid::new_v4());
        model.traffic_request_id = 4;
        model.last_traffic_response_id = 3;
        let effects = update(
            &mut model,
            Msg::ConnectFailed {
                attempt_id: 7,
                error: crate::app::msg::IpcError::new("timeout"),
            },
        );
        assert_eq!(model.connection, ConnectionState::Idle);
        assert_eq!(model.singbox_pid, None);
        assert_eq!(model.active_profile_id, None);
        assert_eq!(model.traffic_request_id, 0);
        assert_eq!(model.last_traffic_response_id, 0);
        assert!(model.status.is_error());
        assert!(model.status.text().contains("Connection failed: timeout"));
        assert_eq!(
            effects,
            vec![
                Effect::BroadcastState,
                app_log_error("Connection failed: timeout")
            ]
        );
    }

    #[test]
    fn stale_connect_failed_does_not_override_newer_connection() {
        let profile_id = Uuid::new_v4();
        let mut model = Model::test_new(crate::config::profile::Config::default());
        model.connection = ConnectionState::Connected;
        model.connect_attempt_id = 2;
        model.active_profile_id = Some(profile_id);
        model.singbox_pid = Some(42);

        let effects = update(
            &mut model,
            Msg::ConnectFailed {
                attempt_id: 1,
                error: crate::app::msg::IpcError::new("old timeout"),
            },
        );

        assert!(effects.is_empty());
        assert_eq!(model.connection, ConnectionState::Connected);
        assert_eq!(model.active_profile_id, Some(profile_id));
        assert_eq!(model.singbox_pid, Some(42));
    }

    #[test]
    fn singbox_exit_clears_connection_and_invalidates_attempt() {
        let profile_id = Uuid::new_v4();
        let mut model = Model::test_new(crate::config::profile::Config::default());
        model.connection = ConnectionState::Connected;
        model.connect_attempt_id = 7;
        model.connecting_profile_id = Some(profile_id);
        model.active_profile_id = Some(profile_id);
        model.singbox_pid = Some(99);
        model.traffic.up_total = 123;
        model.traffic_request_id = 4;
        model.last_traffic_response_id = 3;

        let effects = update(
            &mut model,
            Msg::SingBoxExited {
                attempt_id: 7,
                code: Some(17),
                signal: None,
            },
        );

        assert_eq!(model.connection, ConnectionState::Idle);
        assert_eq!(model.connect_attempt_id, 8);
        assert_eq!(model.connecting_profile_id, None);
        assert_eq!(model.active_profile_id, None);
        assert_eq!(model.singbox_pid, None);
        assert_eq!(model.traffic, TrafficStats::default());
        assert_eq!(model.traffic_request_id, 0);
        assert_eq!(model.last_traffic_response_id, 0);
        assert_eq!(
            model.status.text(),
            "sing-box exited unexpectedly (code 17)"
        );
        assert_eq!(
            effects,
            vec![
                Effect::WriteState,
                Effect::RevokeKillSwitchExceptions,
                Effect::BroadcastState,
                app_log_error("sing-box exited unexpectedly (code 17)"),
            ]
        );
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, Effect::Connect { .. }))
        );
    }

    #[test]
    fn singbox_exit_reports_signal() {
        let mut model = Model::test_new(crate::config::profile::Config::default());
        model.connection = ConnectionState::ConnectPending;
        model.connect_attempt_id = 3;

        update(
            &mut model,
            Msg::SingBoxExited {
                attempt_id: 3,
                code: None,
                signal: Some(9),
            },
        );

        assert_eq!(
            model.status.text(),
            "sing-box terminated unexpectedly (signal 9)"
        );
    }

    #[test]
    fn stale_singbox_exit_does_not_override_newer_connection() {
        let profile_id = Uuid::new_v4();
        let mut model = Model::test_new(crate::config::profile::Config::default());
        model.connection = ConnectionState::Connected;
        model.connect_attempt_id = 8;
        model.active_profile_id = Some(profile_id);
        model.singbox_pid = Some(100);

        let effects = update(
            &mut model,
            Msg::SingBoxExited {
                attempt_id: 7,
                code: Some(1),
                signal: None,
            },
        );

        assert!(effects.is_empty());
        assert_eq!(model.connection, ConnectionState::Connected);
        assert_eq!(model.connect_attempt_id, 8);
        assert_eq!(model.active_profile_id, Some(profile_id));
        assert_eq!(model.singbox_pid, Some(100));
    }

    #[test]
    fn stale_connected_does_not_override_newer_attempt() {
        let mut model = Model::test_new(crate::config::profile::Config::default());
        model.connection = ConnectionState::ConnectPending;
        model.connect_attempt_id = 2;

        let effects = update(
            &mut model,
            Msg::Connected {
                pid: 42,
                profile_id: Uuid::new_v4(),
                attempt_id: 1,
            },
        );

        assert!(effects.is_empty());
        assert_eq!(model.connection, ConnectionState::ConnectPending);
        assert_eq!(model.singbox_pid, None);
        assert_eq!(model.active_profile_id, None);
    }

    #[test]
    fn queued_connect_carries_new_attempt_id() {
        let mut model = model_with_profiles(vec![Profile::new_vless(
            "A".into(),
            "1.1.1.1".into(),
            443,
            "u1".into(),
        )]);
        let profile_id = model.config.profiles[0].id;

        assert!(queue_connect(&mut model, profile_id));
        assert_eq!(model.connect_attempt_id, 1);
        let effects = handle_tick(&mut model);

        assert_eq!(model.connecting_profile_id, Some(profile_id));
        assert!(matches!(
            effects.as_slice(),
            [Effect::Connect { attempt_id: 1, .. }]
        ));
    }

    #[test]
    fn handle_tick_skips_connect_when_pending() {
        let mut model = model_with_profiles(vec![Profile::new_vless(
            "A".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        model.connection = ConnectionState::ConnectPending;
        let effects = handle_tick(&mut model);
        assert!(effects.iter().all(|e| !matches!(e, Effect::Connect { .. })));
    }

    #[test]
    fn connected_clears_pending() {
        let mut model = Model::test_new(crate::config::profile::Config::default());
        model.connection = ConnectionState::ConnectPending;
        let id = uuid::Uuid::new_v4();
        let effects = update(
            &mut model,
            Msg::Connected {
                pid: 12345,
                profile_id: id,
                attempt_id: 0,
            },
        );
        assert_eq!(model.connection, ConnectionState::Connected);
        assert_eq!(model.overlay, Overlay::None);
        // The connection is attributed to the carried id even when the
        // profile is no longer in the list (deleted mid-connect).
        assert_eq!(model.active_profile_id, Some(id));
        assert_eq!(
            effects,
            vec![
                Effect::WriteState,
                app_log_info("Connected"),
                Effect::SaveConfig
            ]
        );
    }

    #[test]
    fn connected_attributes_to_carried_profile_not_cursor() {
        let a = Profile::new_vless(
            "A".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        );
        let b = Profile::new_vless(
            "B".to_string(),
            "2.2.2.2".to_string(),
            443,
            "u2".to_string(),
        );
        let a_id = a.id;
        let mut model = model_with_profiles(vec![a, b]);
        model.connection = ConnectionState::ConnectPending;
        // Cursor rests on B while A's connect completes.
        model.select_next();
        let effects = update(
            &mut model,
            Msg::Connected {
                pid: 1,
                profile_id: a_id,
                attempt_id: 0,
            },
        );
        assert_eq!(model.active_profile_id, Some(a_id));
        assert_eq!(model.config.settings.last_connected_profile, Some(a_id));
        assert!(effects.contains(&app_log_info("Connected to A")));
    }

    #[test]
    fn system_resumed_reconnects_active_profile_not_cursor() {
        let a = Profile::new_vless(
            "A".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        );
        let b = Profile::new_vless(
            "B".to_string(),
            "2.2.2.2".to_string(),
            443,
            "u2".to_string(),
        );
        let a_id = a.id;
        let mut model = model_with_profiles(vec![a, b]);
        model.connection = ConnectionState::Connected;
        model.active_profile_id = Some(a_id);
        model.select_next();

        let effects = update(&mut model, Msg::SystemResumed);

        assert_eq!(model.connecting_profile_id, Some(a_id));
        assert!(effects.contains(&app_log_info("Resumed — reconnecting…")));
        let tick_effects = handle_tick(&mut model);
        assert!(
            tick_effects.iter().any(
                |effect| matches!(effect, Effect::Connect { profile, .. } if profile.id == a_id)
            )
        );
    }

    #[test]
    fn system_resumed_without_resolvable_active_profile_is_noop() {
        let mut model = model_with_profiles(vec![Profile::new_vless(
            "A".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        model.connection = ConnectionState::Connected;
        model.active_profile_id = Some(Uuid::new_v4());

        let effects = update(&mut model, Msg::SystemResumed);

        assert!(effects.is_empty());
    }

    #[test]
    fn connected_fetches_service_rule_sets_only_when_enabled() {
        use crate::config::profile::{RoutedService, ServiceRoute};

        let mut model = Model::test_new(crate::config::profile::Config::default());
        model.connection = ConnectionState::ConnectPending;
        let effects = update(
            &mut model,
            Msg::Connected {
                pid: 1,
                profile_id: uuid::Uuid::new_v4(),
                attempt_id: 0,
            },
        );
        assert!(
            !effects.contains(&Effect::DownloadServiceRuleSetsIfMissing),
            "all service routes are disabled by default — no fetch"
        );

        let mut model = Model::test_new(crate::config::profile::Config::default());
        model
            .config
            .settings
            .geo_routing
            .service_routes
            .insert(RoutedService::Telegram, ServiceRoute::Proxy);
        model.connection = ConnectionState::ConnectPending;
        let effects = update(
            &mut model,
            Msg::Connected {
                pid: 1,
                profile_id: uuid::Uuid::new_v4(),
                attempt_id: 0,
            },
        );
        assert!(effects.contains(&Effect::DownloadServiceRuleSetsIfMissing));

        // Explicitly-disabled entries don't count as enabled.
        let mut model = Model::test_new(crate::config::profile::Config::default());
        model
            .config
            .settings
            .geo_routing
            .service_routes
            .insert(RoutedService::Steam, ServiceRoute::Disabled);
        model.connection = ConnectionState::ConnectPending;
        let effects = update(
            &mut model,
            Msg::Connected {
                pid: 1,
                profile_id: uuid::Uuid::new_v4(),
                attempt_id: 0,
            },
        );
        assert!(!effects.contains(&Effect::DownloadServiceRuleSetsIfMissing));
    }

    #[test]
    fn service_rule_sets_ready_reconnects_active_profile_not_cursor() {
        let a = Profile::new_vless("A".into(), "e".into(), 1, "u".into());
        let b = Profile::new_vless("B".into(), "e".into(), 2, "u".into());
        let active_id = a.id;
        let mut model = model_with_profiles(vec![a, b]);
        model.connection = ConnectionState::Connected;
        model.active_profile_id = Some(active_id);
        model.pending_service_reconnect = true;
        // Cursor rests on profile B — the reconnect must still target A.
        model.select_next();
        update(
            &mut model,
            Msg::ServiceRuleSetsReady {
                retry_states: Default::default(),
                checked_at: Default::default(),
                next_updates: Default::default(),
                updated_parts: Vec::new(),
                errors: Vec::new(),
            },
        );
        assert!(!model.pending_service_reconnect, "flag is consumed");
        assert_eq!(model.connection, ConnectionState::Connecting);
        assert_eq!(model.connecting_profile_id, Some(active_id));
        let tick_effects = handle_tick(&mut model);
        let connect_id = tick_effects.iter().find_map(|e| match e {
            Effect::Connect { profile, .. } => Some(profile.id),
            _ => None,
        });
        assert_eq!(
            connect_id,
            Some(active_id),
            "must reconnect the active profile, not the cursor's"
        );
    }

    #[test]
    fn service_rule_sets_ready_without_pending_commit_is_noop() {
        // The post-connect backstop download also reports readiness; it must
        // not trigger a reconnect loop.
        let profile = Profile::new_vless("A".into(), "e".into(), 1, "u".into());
        let mut model = model_with_profiles(vec![profile.clone()]);
        model.connection = ConnectionState::Connected;
        model.active_profile_id = Some(profile.id);
        let effects = update(
            &mut model,
            Msg::ServiceRuleSetsReady {
                retry_states: Default::default(),
                checked_at: Default::default(),
                next_updates: Default::default(),
                updated_parts: Vec::new(),
                errors: Vec::new(),
            },
        );
        assert!(effects.is_empty());
        assert_eq!(model.connection, ConnectionState::Connected);
    }

    #[test]
    fn service_rule_sets_ready_after_disconnect_clears_flag_without_connect() {
        let profile = Profile::new_vless("A".into(), "e".into(), 1, "u".into());
        let mut model = model_with_profiles(vec![profile.clone()]);
        model.connection = ConnectionState::Idle;
        model.active_profile_id = Some(profile.id);
        model.pending_service_reconnect = true;
        let effects = update(
            &mut model,
            Msg::ServiceRuleSetsReady {
                retry_states: Default::default(),
                checked_at: Default::default(),
                next_updates: Default::default(),
                updated_parts: Vec::new(),
                errors: Vec::new(),
            },
        );
        assert!(!model.pending_service_reconnect);
        assert!(!effects.iter().any(|e| matches!(e, Effect::Connect { .. })));
        assert_eq!(model.connection, ConnectionState::Idle);
    }

    #[test]
    fn service_rule_sets_ready_with_missing_active_profile_does_not_flip_state() {
        let profile = Profile::new_vless("A".into(), "e".into(), 1, "u".into());
        let mut model = model_with_profiles(vec![profile]);
        model.connection = ConnectionState::Connected;
        // Active profile id points at a profile that no longer exists.
        model.active_profile_id = Some(uuid::Uuid::new_v4());
        model.pending_service_reconnect = true;
        let effects = update(
            &mut model,
            Msg::ServiceRuleSetsReady {
                retry_states: Default::default(),
                checked_at: Default::default(),
                next_updates: Default::default(),
                updated_parts: Vec::new(),
                errors: Vec::new(),
            },
        );
        assert!(!model.pending_service_reconnect);
        assert!(!effects.iter().any(|e| matches!(e, Effect::Connect { .. })));
        // Never drop to Connecting without a resolvable target — that path
        // ends in Idle-with-live-tunnel on the next Tick.
        assert_eq!(model.connection, ConnectionState::Connected);
    }

    #[test]
    fn connected_saves_last_profile() {
        let mut model = model_with_profiles(vec![Profile::new_vless(
            "A".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        model.connection = ConnectionState::ConnectPending;
        let id = model.config.profiles[0].id;
        let effects = update(
            &mut model,
            Msg::Connected {
                pid: 12345,
                profile_id: id,
                attempt_id: 0,
            },
        );
        assert_eq!(model.connection, ConnectionState::Connected);
        assert_eq!(
            model.config.settings.last_connected_profile,
            Some(model.config.profiles[0].id)
        );
        assert_eq!(
            effects,
            vec![
                Effect::WriteState,
                app_log_info("Connected to A"),
                Effect::SaveConfig
            ]
        );
    }

    #[test]
    fn toggle_auto_connect() {
        let mut model = model_with_profiles(vec![]);
        assert!(!model.config.settings.auto_connect);
        let effects = handle_sources(&mut model, key('a'));
        assert!(model.config.settings.auto_connect);
        assert!(model.status.text().contains("enabled"));
        assert_eq!(
            effects,
            vec![app_log_info("Auto-connect enabled"), Effect::SaveConfig]
        );

        let effects = handle_sources(&mut model, key('a'));
        assert!(!model.config.settings.auto_connect);
        assert!(model.status.text().contains("disabled"));
        assert_eq!(
            effects,
            vec![app_log_info("Auto-connect disabled"), Effect::SaveConfig]
        );
    }

    #[test]
    fn toggle_kill_switch_emits_apply_effect_and_does_not_flip_bool() {
        let mut model = model_with_profiles(vec![]);
        assert!(!model.config.settings.kill_switch);

        let effects = handle_sources(&mut model, key('K'));
        // Bool is NOT flipped synchronously — it waits for KillSwitchApplied.
        assert!(!model.config.settings.kill_switch);
        assert_eq!(model.kill_switch_pending, Some(true));
        assert!(model.status.text().contains("enabling"));
        assert_eq!(
            effects,
            vec![
                app_log_info("Kill switch enabling…"),
                Effect::ApplyKillSwitch { enabled: true },
            ]
        );
    }

    #[test]
    fn repeated_kill_switch_toggle_is_ignored_while_pending() {
        let mut model = model_with_profiles(vec![]);
        let first = handle_sources(&mut model, key('K'));
        let status = model.status.clone();

        let second = handle_sources(&mut model, key('K'));

        assert!(first.contains(&Effect::ApplyKillSwitch { enabled: true }));
        assert!(second.is_empty());
        assert_eq!(model.kill_switch_pending, Some(true));
        assert_eq!(model.status, status);
    }

    #[test]
    fn kill_switch_applied_success_flips_and_saves() {
        let mut model = model_with_profiles(vec![]);
        model.kill_switch_pending = Some(true);
        let effects = update(
            &mut model,
            Msg::KillSwitchApplied {
                enabled: true,
                error: None,
            },
        );
        assert!(model.config.settings.kill_switch);
        assert_eq!(model.kill_switch_pending, None);
        assert!(model.status.text().contains("enabled"));
        assert_eq!(
            effects,
            vec![
                app_log_info("Kill switch enabled"),
                Effect::SaveConfig,
                Effect::BroadcastState,
            ]
        );
    }

    #[test]
    fn kill_switch_applied_error_keeps_bool_unchanged() {
        let mut model = model_with_profiles(vec![]);
        model.kill_switch_pending = Some(true);
        let effects = update(
            &mut model,
            Msg::KillSwitchApplied {
                enabled: true,
                error: Some(crate::app::msg::IpcError::new("helper missing")),
            },
        );
        assert!(!model.config.settings.kill_switch);
        assert_eq!(model.kill_switch_pending, None);
        assert!(model.status.text().contains("helper missing"));
        assert!(!effects.iter().any(|e| matches!(e, Effect::SaveConfig)));
    }

    #[test]
    fn stale_kill_switch_result_is_ignored() {
        let mut model = model_with_profiles(vec![]);
        model.kill_switch_pending = Some(true);
        let status = model.status.clone();

        let effects = update(
            &mut model,
            Msg::KillSwitchApplied {
                enabled: false,
                error: None,
            },
        );

        assert!(effects.is_empty());
        assert!(!model.config.settings.kill_switch);
        assert_eq!(model.kill_switch_pending, Some(true));
        assert_eq!(model.status, status);
    }

    #[test]
    fn kill_switch_disable_uses_pending_false() {
        let mut model = model_with_profiles(vec![]);
        model.config.settings.kill_switch = true;

        let effects = handle_sources(&mut model, key('K'));

        assert_eq!(model.kill_switch_pending, Some(false));
        assert!(effects.contains(&Effect::ApplyKillSwitch { enabled: false }));
    }

    #[test]
    fn paste_duplicate_profile_shows_error() {
        let mut model = model_with_profiles(vec![]);
        let uri = "vless://671c62c7-6768-4b98-ac6b-572c9c707be0@203.0.113.42:443#Test";

        // First paste succeeds
        let effects = handle_clipboard_text(&mut model, uri);
        assert_eq!(model.config.profiles.len(), 1);
        assert_eq!(
            effects,
            vec![Effect::SaveConfig, app_log_info("Pasted profile: Test")]
        );
        assert!(model.status.text().contains("Pasted profile"));

        // Second paste with same UUID fails
        let effects = handle_clipboard_text(&mut model, uri);
        assert_eq!(model.config.profiles.len(), 1);
        assert_eq!(effects, vec![app_log_error("Profile already exists")]);
        assert!(model.status.is_error());
        assert!(model.status.text().contains("already exists"));
    }

    #[test]
    fn paste_subscription_url_creates_subscription_and_fetches() {
        let mut model = model_with_profiles(vec![]);
        let url = "http://31.58.134.29:2096/sub/xrkjeq2mhwes0i8f";

        let effects = handle_clipboard_text(&mut model, url);

        assert_eq!(model.config.subscriptions.len(), 1);
        assert_eq!(model.config.subscriptions[0].url, url);
        assert!(matches!(
            model.selected_row(),
            Some(crate::app::model::SourceRow::SubscriptionHeader(0))
        ));
        assert!(model.subscription_fetching);
        assert!(
            model
                .subscription_updates
                .contains(&model.config.subscriptions[0].id)
        );
        assert_eq!(
            effects,
            vec![
                Effect::SaveConfig,
                Effect::UpdateSubscription {
                    id: model.config.subscriptions[0].id
                },
                app_log_info("Added subscription '31.58.134.29' and fetching profiles…")
            ]
        );
    }

    #[test]
    fn paste_vless_adds_standalone_profile() {
        let mut model = model_with_profiles(vec![]);
        let uri = "vless://671c62c7-6768-4b98-ac6b-572c9c707be0@203.0.113.42:443#Test";

        let effects = handle_clipboard_text(&mut model, uri);

        assert_eq!(model.config.profiles.len(), 1);
        assert!(matches!(
            model.selected_row(),
            Some(crate::app::model::SourceRow::StandaloneProfile(0))
        ));
        assert_eq!(
            effects,
            vec![Effect::SaveConfig, app_log_info("Pasted profile: Test")]
        );
    }

    #[test]
    fn subscription_fetched_adds_profiles_and_saves() {
        let mut model = model_with_profiles(vec![]);
        let profiles = vec![
            Profile::new_vless(
                "Sub1".to_string(),
                "1.1.1.1".to_string(),
                443,
                "u1".to_string(),
            ),
            Profile::new_vless(
                "Sub2".to_string(),
                "2.2.2.2".to_string(),
                443,
                "u2".to_string(),
            ),
        ];

        let effects = handle_subscription_result(&mut model, Uuid::nil(), Ok(profiles));

        assert!(!model.subscription_fetching);
        assert_eq!(model.config.profiles.len(), 2);
        assert_eq!(
            effects,
            vec![
                Effect::SaveConfig,
                app_log_info("Imported 2 profile(s) from subscription")
            ]
        );
    }

    #[test]
    fn subscription_fetched_updates_standalone_duplicate() {
        let mut model = model_with_profiles(vec![Profile::new_vless(
            "Existing".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        let profiles = vec![
            Profile::new_vless(
                "Existing".to_string(),
                "1.1.1.1".to_string(),
                443,
                "u1".to_string(),
            ),
            Profile::new_vless(
                "New".to_string(),
                "2.2.2.2".to_string(),
                443,
                "u2".to_string(),
            ),
        ];

        let effects = handle_subscription_result(&mut model, Uuid::nil(), Ok(profiles));

        assert_eq!(model.config.profiles.len(), 2);
        assert_eq!(
            effects,
            vec![
                Effect::SaveConfig,
                app_log_info("Imported 2 profile(s) from subscription")
            ]
        );
    }

    #[test]
    fn subscription_fetched_skips_duplicate_from_other_subscription() {
        let other_sub_id = Uuid::new_v4();
        let mut existing = Profile::new_vless(
            "Existing".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        );
        existing.subscription_id = Some(other_sub_id);
        let mut model = model_with_profiles(vec![existing]);
        model.config.subscriptions.push(Subscription {
            id: other_sub_id,
            name: "Other".to_string(),
            url: "http://example.com/other".to_string(),
            auto_update: SubscriptionAutoUpdate::Every1d,
            last_updated: None,
            next_auto_update: None,
            retry_state: None,
        });

        let new_sub_id = Uuid::new_v4();
        model.config.subscriptions.push(Subscription {
            id: new_sub_id,
            name: "New".to_string(),
            url: "http://example.com/new".to_string(),
            auto_update: SubscriptionAutoUpdate::Every1d,
            last_updated: None,
            next_auto_update: None,
            retry_state: None,
        });

        let fetched = Profile::new_vless(
            "Existing".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        );

        let effects = handle_subscription_result(&mut model, new_sub_id, Ok(vec![fetched]));

        assert_eq!(model.config.profiles.len(), 1);
        assert_eq!(model.config.profiles[0].subscription_id, Some(other_sub_id));
        assert_eq!(
            effects,
            vec![
                Effect::SaveConfig,
                app_log_info("No new profiles in subscription")
            ]
        );
    }

    #[test]
    fn subscription_fetched_attaches_standalone_duplicate() {
        let sub_id = Uuid::new_v4();
        let standalone = Profile::new_vless(
            "OldName".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        );
        let standalone_id = standalone.id;
        let mut model = model_with_profiles(vec![standalone]);
        model.config.subscriptions.push(Subscription {
            id: sub_id,
            name: "Sub".to_string(),
            url: "http://example.com/sub".to_string(),
            auto_update: SubscriptionAutoUpdate::Every1d,
            last_updated: None,
            next_auto_update: None,
            retry_state: None,
        });

        let mut fetched = Profile::new_vless(
            "NewName".to_string(),
            "2.2.2.2".to_string(),
            443,
            "u1".to_string(),
        );
        // Different id from parse, same uuid as the standalone profile.
        fetched.id = Uuid::new_v4();

        let effects = handle_subscription_result(&mut model, sub_id, Ok(vec![fetched]));

        assert_eq!(model.config.profiles.len(), 1);
        assert_eq!(model.config.profiles[0].id, standalone_id);
        assert_eq!(model.config.profiles[0].name, "NewName");
        assert_eq!(model.config.profiles[0].address, "2.2.2.2");
        assert_eq!(model.config.profiles[0].subscription_id, Some(sub_id));
        assert_eq!(
            effects,
            vec![
                Effect::SaveConfig,
                app_log_info("Imported 1 profile(s) from subscription")
            ]
        );
    }

    #[test]
    fn subscription_fetched_empty_logs_no_new_profiles() {
        let mut model = model_with_profiles(vec![]);

        let effects = handle_subscription_result(&mut model, Uuid::nil(), Ok(vec![]));

        assert_eq!(model.config.profiles.len(), 0);
        assert_eq!(
            effects,
            vec![app_log_info("No new profiles in subscription")]
        );
    }

    #[test]
    fn subscription_fetched_error_logs_failure() {
        let sub_id = Uuid::new_v4();
        let mut existing = Profile::new_vless(
            "Existing".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        );
        existing.subscription_id = Some(sub_id);
        let existing_id = existing.id;
        let last_updated = Local::now() - chrono::Duration::try_hours(2).unwrap();
        let mut model = model_with_profiles(vec![existing]);
        model.config.subscriptions.push(Subscription {
            id: sub_id,
            name: "Sub".to_string(),
            url: "http://example.com/sub".to_string(),
            auto_update: SubscriptionAutoUpdate::Every1d,
            last_updated: Some(last_updated),
            next_auto_update: None,
            retry_state: None,
        });
        model.automatic_subscription_updates.insert(sub_id);

        let failed_at = Local::now();
        let effects = handle_subscription_result_at(
            &mut model,
            sub_id,
            Err(crate::app::msg::IpcError::new("network down")),
            failed_at,
        );

        assert!(!model.subscription_fetching);
        assert_eq!(model.config.profiles.len(), 1);
        assert_eq!(model.config.profiles[0].id, existing_id);
        assert_eq!(
            model.config.subscriptions[0].last_updated,
            Some(last_updated)
        );
        assert_eq!(
            model.config.subscriptions[0].retry_state,
            Some(crate::config::profile::SubscriptionRetryState {
                consecutive_failures: 1,
                retry_at: failed_at + chrono::Duration::minutes(1),
                attempt_date: Some(failed_at.date_naive()),
            })
        );
        assert_eq!(
            effects,
            vec![
                Effect::SaveConfig,
                app_log_error("Subscription failed: network down"),
                Effect::AppendAppLog {
                    level: "WARN".into(),
                    message: "VPN is disconnected. Try connecting to VPN and retrying the subscription update."
                        .into(),
                },
            ]
        );
    }

    #[test]
    fn subscription_fetched_error_broadcasts_state_without_other_effects() {
        let mut model = model_with_profiles(vec![]);
        let effects = update(
            &mut model,
            Msg::SubscriptionFetched {
                id: Uuid::nil(),
                result: Err(crate::app::msg::IpcError::new("network down")),
            },
        );

        assert!(effects.contains(&Effect::BroadcastState));
        assert!(matches!(model.status, AppStatus::Error(_)));
    }

    #[test]
    fn subscription_fetched_managed_replaces_profiles_and_updates_last_updated() {
        let sub_id = Uuid::new_v4();
        let mut existing = Profile::new_vless(
            "Old".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        );
        existing.subscription_id = Some(sub_id);
        let mut model = model_with_profiles(vec![existing]);
        model.config.subscriptions.push(Subscription {
            id: sub_id,
            name: "Sub".to_string(),
            url: "http://example.com/sub".to_string(),
            auto_update: SubscriptionAutoUpdate::Every1d,
            last_updated: None,
            next_auto_update: None,
            retry_state: None,
        });
        model.config.subscriptions[0].record_fetch_failure(Local::now());
        model.automatic_subscription_updates.insert(sub_id);

        let new_profiles = vec![Profile::new_vless(
            "New".to_string(),
            "2.2.2.2".to_string(),
            443,
            "u2".to_string(),
        )];

        let effects = handle_subscription_result(&mut model, sub_id, Ok(new_profiles));

        assert_eq!(model.config.profiles.len(), 1);
        assert_eq!(model.config.profiles[0].name, "New");
        assert_eq!(model.config.profiles[0].subscription_id, Some(sub_id));
        assert!(
            model
                .config
                .subscriptions
                .iter()
                .find(|s| s.id == sub_id)
                .unwrap()
                .last_updated
                .is_some()
        );
        assert!(model.config.subscriptions[0].retry_state.is_none());
        assert_eq!(
            effects,
            vec![
                Effect::SaveConfig,
                app_log_info("Imported 1 profile(s) from subscription")
            ]
        );
    }

    #[test]
    fn subscription_fetched_restores_selection_to_subscription_header() {
        let sub_id = Uuid::new_v4();
        let mut existing = Profile::new_vless(
            "Old".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        );
        existing.subscription_id = Some(sub_id);
        let mut model = model_with_profiles(vec![existing]);
        model.config.subscriptions.push(Subscription {
            id: sub_id,
            name: "Sub".to_string(),
            url: "http://example.com/sub".to_string(),
            auto_update: SubscriptionAutoUpdate::Every1d,
            last_updated: None,
            next_auto_update: None,
            retry_state: None,
        });
        // Start with cursor on the subscription header.
        model.selected = crate::app::model::row_for_subscription_header(&model.config, 0);
        let header_row = model.selected;

        let new_profiles = vec![
            Profile::new_vless(
                "A".to_string(),
                "2.2.2.2".to_string(),
                443,
                "u2".to_string(),
            ),
            Profile::new_vless(
                "B".to_string(),
                "3.3.3.3".to_string(),
                443,
                "u3".to_string(),
            ),
        ];
        handle_subscription_result(&mut model, sub_id, Ok(new_profiles));

        assert_eq!(
            model.selected, header_row,
            "cursor should stay on the subscription header"
        );
    }

    #[test]
    fn subscription_fetched_preserves_active_profile_id_for_same_server() {
        let sub_id = Uuid::new_v4();
        let mut existing = Profile::new_vless(
            "Server".to_string(),
            "1.1.1.1".to_string(),
            443,
            "same-uuid".to_string(),
        );
        existing.subscription_id = Some(sub_id);
        let old_profile_id = existing.id;
        let mut model = model_with_profiles(vec![existing]);
        model.active_profile_id = Some(old_profile_id);

        // Same server (same dedup key) comes back in the updated subscription.
        let updated = Profile::new_vless(
            "Server Renamed".to_string(),
            "1.1.1.1".to_string(),
            443,
            "same-uuid".to_string(),
        );
        handle_subscription_result(&mut model, sub_id, Ok(vec![updated]));

        assert_eq!(model.config.profiles.len(), 1);
        assert_eq!(model.config.profiles[0].id, old_profile_id);
        assert_eq!(model.active_profile_id, Some(old_profile_id));
    }

    #[test]
    fn subscription_fetched_reconnects_changed_active_profile() {
        let sub_id = Uuid::new_v4();
        let mut existing = Profile::new_vless(
            "Server".to_string(),
            "1.1.1.1".to_string(),
            443,
            "same-uuid".to_string(),
        );
        existing.subscription_id = Some(sub_id);
        let old_profile_id = existing.id;
        let mut model = model_with_profiles(vec![existing]);
        model.config.subscriptions.push(Subscription {
            id: sub_id,
            name: "Sub".into(),
            url: "http://example.com/sub".into(),
            auto_update: SubscriptionAutoUpdate::Off,
            last_updated: None,
            next_auto_update: None,
            retry_state: None,
        });
        model.connection = ConnectionState::Connected;
        model.active_profile_id = Some(old_profile_id);
        let mut updated = Profile::new_vless(
            "Server".to_string(),
            "1.1.1.1".to_string(),
            443,
            "same-uuid".to_string(),
        );
        if let crate::config::profile::ProtocolConfig::Vless(config) = &mut updated.config {
            config.flow = Some(crate::config::profile::Flow::XtlsRprxVision);
        }

        let effects = handle_subscription_result(&mut model, sub_id, Ok(vec![updated]));

        assert_eq!(model.connection, ConnectionState::Connecting);
        assert_eq!(model.connecting_profile_id, Some(old_profile_id));
        assert!(effects.contains(&app_log_info("Subscription changed — reconnecting")));
    }

    #[test]
    fn subscription_update_disconnects_when_connecting_profile_is_removed() {
        let sub_id = Uuid::new_v4();
        let mut existing = Profile::new_vless(
            "Server".to_string(),
            "1.1.1.1".to_string(),
            443,
            "old-uuid".to_string(),
        );
        existing.subscription_id = Some(sub_id);
        let profile_id = existing.id;
        let mut model = model_with_profiles(vec![existing]);
        model.config.subscriptions.push(Subscription {
            id: sub_id,
            name: "Sub".into(),
            url: "http://example.com/sub".into(),
            auto_update: SubscriptionAutoUpdate::Off,
            last_updated: None,
            next_auto_update: None,
            retry_state: None,
        });
        assert!(queue_connect(&mut model, profile_id));
        model.connection = ConnectionState::ConnectPending;

        let effects = handle_subscription_result(&mut model, sub_id, Ok(vec![]));

        assert!(effects.contains(&Effect::Disconnect));
    }

    #[test]
    fn subscription_fetched_clears_active_profile_id_when_profile_removed() {
        let sub_id = Uuid::new_v4();
        let mut existing = Profile::new_vless(
            "Old Server".to_string(),
            "1.1.1.1".to_string(),
            443,
            "old-uuid".to_string(),
        );
        existing.subscription_id = Some(sub_id);
        let old_profile_id = existing.id;
        let mut model = model_with_profiles(vec![existing]);
        model.active_profile_id = Some(old_profile_id);

        // Updated subscription has a different server; old one is gone.
        let new_server = Profile::new_vless(
            "New Server".to_string(),
            "2.2.2.2".to_string(),
            443,
            "new-uuid".to_string(),
        );
        let effects = handle_subscription_result(&mut model, sub_id, Ok(vec![new_server]));

        assert_eq!(model.config.profiles.len(), 1);
        // Disconnect is emitted; active_profile_id is cleared when the effect runs.
        assert!(effects.contains(&Effect::Disconnect));
    }

    #[test]
    fn subscriptions_update_triggers_fetch() {
        let mut model = model_with_profiles(vec![]);
        let sub_id = Uuid::new_v4();
        model.config.subscriptions.push(Subscription {
            id: sub_id,
            name: "Sub".to_string(),
            url: "http://example.com/sub".to_string(),
            auto_update: SubscriptionAutoUpdate::Every1d,
            last_updated: None,
            next_auto_update: None,
            retry_state: None,
        });
        model.selected = 0;

        let effects = handle_sources(&mut model, key('u'));

        assert!(model.subscription_fetching);
        assert!(model.subscription_updates.contains(&sub_id));
        assert_eq!(
            effects,
            vec![
                Effect::SaveConfig,
                Effect::UpdateSubscription { id: sub_id },
                app_log_info("Updating subscription 'Sub'…"),
            ]
        );
    }

    #[test]
    fn subscription_interval_shows_status() {
        use crate::config::profile::Subscription;
        use uuid::Uuid;

        let sub_id = Uuid::new_v4();
        let mut model = model_with_profiles(vec![]);
        model.config.subscriptions.push(Subscription {
            id: sub_id,
            name: "Sub".to_string(),
            url: "http://example.com".to_string(),
            auto_update: SubscriptionAutoUpdate::Every1d,
            last_updated: None,
            next_auto_update: None,
            retry_state: None,
        });
        model.selected = 0;
        model.config.subscriptions[0].record_fetch_failure(Local::now());
        let effects = handle_sources(&mut model, KeyEvent::from(KeyCode::Char('i')));
        assert_eq!(
            model.config.subscriptions[0].auto_update,
            SubscriptionAutoUpdate::Every3d
        );
        assert!(effects.contains(&Effect::SaveConfig));
        assert!(effects.contains(&app_log_info("Subscription 'Sub' [🗘 3d]")));
        assert!(model.config.subscriptions[0].retry_state.is_none());
    }

    #[test]
    fn subscriptions_interval_cycles() {
        let mut model = model_with_profiles(vec![]);
        model.config.subscriptions.push(Subscription {
            id: Uuid::new_v4(),
            name: "Sub".to_string(),
            url: "http://example.com/sub".to_string(),
            auto_update: SubscriptionAutoUpdate::Off,
            last_updated: None,
            next_auto_update: None,
            retry_state: None,
        });
        model.selected = 0;

        let _ = handle_sources(&mut model, KeyEvent::from(KeyCode::Char('i')));
        assert_eq!(
            model.config.subscriptions[0].auto_update,
            SubscriptionAutoUpdate::Every1d
        );
        let _ = handle_sources(&mut model, KeyEvent::from(KeyCode::Char('i')));
        assert_eq!(
            model.config.subscriptions[0].auto_update,
            SubscriptionAutoUpdate::Every3d
        );
        let _ = handle_sources(&mut model, KeyEvent::from(KeyCode::Char('i')));
        assert_eq!(
            model.config.subscriptions[0].auto_update,
            SubscriptionAutoUpdate::Every7d
        );
        let _ = handle_sources(&mut model, KeyEvent::from(KeyCode::Char('i')));
        assert_eq!(
            model.config.subscriptions[0].auto_update,
            SubscriptionAutoUpdate::Off
        );
    }

    #[test]
    fn confirm_delete_subscription_removes_subscription_and_profiles() {
        let sub_id = Uuid::new_v4();
        let mut existing = Profile::new_vless(
            "Old".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        );
        existing.subscription_id = Some(sub_id);
        let mut model = model_with_profiles(vec![existing]);
        model.config.subscriptions.push(Subscription {
            id: sub_id,
            name: "Sub".to_string(),
            url: "http://example.com/sub".to_string(),
            auto_update: SubscriptionAutoUpdate::Every1d,
            last_updated: None,
            next_auto_update: None,
            retry_state: None,
        });
        // source_rows: [SubscriptionHeader(0), SubscriptionProfile { sub_idx: 0, profile_idx: 0 }]
        model.selected = 0;
        model.overlay = Overlay::ConfirmDelete;

        let effects = handle_confirm_delete(&mut model, KeyEvent::from(KeyCode::Enter));

        assert!(model.config.subscriptions.is_empty());
        assert!(model.config.profiles.is_empty());
        assert_eq!(model.selected, 0);
        assert_eq!(
            effects,
            vec![
                Effect::SaveConfig,
                app_log_info("Subscription 'Sub' deleted")
            ]
        );
    }

    #[test]
    fn due_subscriptions_are_queued_for_update() {
        let sub_id = Uuid::new_v4();
        let now = after_update_window();
        let mut model = model_with_profiles(vec![]);
        model.config.subscriptions.push(Subscription {
            id: sub_id,
            name: "Sub".to_string(),
            url: "http://example.com/sub".to_string(),
            auto_update: SubscriptionAutoUpdate::Every1d,
            last_updated: Some(now - chrono::Duration::try_hours(25).unwrap()),
            next_auto_update: None,
            retry_state: None,
        });

        let effects = check_due_subscriptions_at(&mut model, now);

        assert_eq!(effects, vec![Effect::UpdateSubscription { id: sub_id }]);
        assert!(model.subscription_updates.contains(&sub_id));
    }

    #[test]
    fn failed_subscription_waits_until_retry_deadline() {
        let sub_id = Uuid::new_v4();
        let now = Local::now();
        let mut model = model_with_profiles(vec![]);
        let mut sub = Subscription {
            id: sub_id,
            name: "Sub".to_string(),
            url: "http://example.com/sub".to_string(),
            auto_update: SubscriptionAutoUpdate::Every1d,
            last_updated: Some(now - chrono::Duration::hours(2)),
            next_auto_update: None,
            retry_state: None,
        };
        sub.record_fetch_failure(now);
        model.config.subscriptions.push(sub);

        assert!(check_due_subscriptions_at(&mut model, now).is_empty());
        assert!(
            check_due_subscriptions_at(&mut model, now + chrono::Duration::seconds(59)).is_empty()
        );
        assert_eq!(
            check_due_subscriptions_at(&mut model, now + chrono::Duration::minutes(1)),
            vec![Effect::UpdateSubscription { id: sub_id }]
        );
    }

    #[test]
    fn subscription_stops_after_five_failures_and_reopens_at_eight_next_day() {
        use chrono::TimeZone;

        let today = Local.with_ymd_and_hms(2026, 8, 22, 12, 0, 0).unwrap();
        let mut model = model_with_profiles(vec![]);
        let id = Uuid::new_v4();
        model.config.subscriptions.push(Subscription {
            id,
            name: "Sub".into(),
            url: "https://example.com/sub".into(),
            auto_update: SubscriptionAutoUpdate::Every1d,
            last_updated: None,
            next_auto_update: Some(today.date_naive()),
            retry_state: Some(crate::config::profile::SubscriptionRetryState {
                consecutive_failures: 5,
                retry_at: today,
                attempt_date: Some(today.date_naive()),
            }),
        });

        assert!(check_due_subscriptions_at(&mut model, today).is_empty());
        let before_window = today + chrono::Duration::hours(19);
        assert!(check_due_subscriptions_at(&mut model, before_window).is_empty());
        let at_window = today + chrono::Duration::hours(20);
        assert_eq!(
            check_due_subscriptions_at(&mut model, at_window),
            vec![Effect::UpdateSubscription { id }]
        );
    }

    #[test]
    fn manual_subscription_result_does_not_change_automatic_schedule() {
        let now = Local::now();
        let id = Uuid::new_v4();
        let mut model = model_with_profiles(vec![]);
        let retry = crate::config::profile::SubscriptionRetryState {
            consecutive_failures: 2,
            retry_at: now + chrono::Duration::minutes(5),
            attempt_date: Some(now.date_naive()),
        };
        model.config.subscriptions.push(Subscription {
            id,
            name: "Sub".into(),
            url: "https://example.com/sub".into(),
            auto_update: SubscriptionAutoUpdate::Every3d,
            last_updated: None,
            next_auto_update: Some(now.date_naive()),
            retry_state: Some(retry.clone()),
        });

        handle_subscription_result_at(&mut model, id, Ok(Vec::new()), now);

        assert_eq!(model.config.subscriptions[0].retry_state, Some(retry));
        assert_eq!(
            model.config.subscriptions[0].next_auto_update,
            Some(now.date_naive())
        );
    }

    #[test]
    fn manual_subscription_update_ignores_retry_deadline() {
        let sub_id = Uuid::new_v4();
        let mut model = model_with_profiles(vec![]);
        let mut sub = Subscription {
            id: sub_id,
            name: "Sub".to_string(),
            url: "http://example.com/sub".to_string(),
            auto_update: SubscriptionAutoUpdate::Every1d,
            last_updated: None,
            next_auto_update: None,
            retry_state: None,
        };
        sub.record_fetch_failure(Local::now());
        model.config.subscriptions.push(sub);
        model.selected = 0;

        let effects = handle_sources(&mut model, KeyEvent::from(KeyCode::Char('u')));

        assert!(effects.contains(&Effect::UpdateSubscription { id: sub_id }));
        assert!(model.subscription_updates.contains(&sub_id));
    }

    #[test]
    fn subscription_auto_update_waits_for_vpn_when_kill_switch_is_enabled() {
        let sub_id = Uuid::new_v4();
        let mut model = model_with_profiles(vec![]);
        model.config.subscriptions.push(Subscription {
            id: sub_id,
            name: "Sub".to_string(),
            url: "http://example.com/sub".to_string(),
            auto_update: SubscriptionAutoUpdate::Every1d,
            last_updated: None,
            next_auto_update: None,
            retry_state: None,
        });
        model.config.settings.kill_switch = true;
        let now = after_update_window();

        model.connection = ConnectionState::Connecting;
        assert!(check_due_subscriptions_at(&mut model, now).is_empty());
        assert!(!model.subscription_updates.contains(&sub_id));

        model.connection = ConnectionState::ConnectPending;
        assert!(check_due_subscriptions_at(&mut model, now).is_empty());
        assert!(!model.subscription_updates.contains(&sub_id));

        model.connection = ConnectionState::Connected;
        assert_eq!(
            check_due_subscriptions_at(&mut model, now),
            vec![Effect::UpdateSubscription { id: sub_id }]
        );
        assert!(model.subscription_updates.contains(&sub_id));
    }

    #[test]
    fn non_due_subscriptions_are_skipped() {
        let sub_id = Uuid::new_v4();
        let now = after_update_window();
        let mut model = model_with_profiles(vec![]);
        model.config.subscriptions.push(Subscription {
            id: sub_id,
            name: "Sub".to_string(),
            url: "http://example.com/sub".to_string(),
            auto_update: SubscriptionAutoUpdate::Every1d,
            last_updated: Some(now),
            next_auto_update: None,
            retry_state: None,
        });

        let effects = check_due_subscriptions_at(&mut model, now);

        assert!(effects.is_empty());
        assert!(!model.subscription_updates.contains(&sub_id));
    }

    #[test]
    fn disabled_subscription_does_not_retry_persisted_failure() {
        let sub_id = Uuid::new_v4();
        let now = Local::now();
        let mut model = model_with_profiles(vec![]);
        let mut sub = Subscription {
            id: sub_id,
            name: "Sub".to_string(),
            url: "http://example.com/sub".to_string(),
            auto_update: SubscriptionAutoUpdate::Off,
            last_updated: None,
            next_auto_update: None,
            retry_state: None,
        };
        sub.record_fetch_failure(now - chrono::Duration::hours(1));
        model.config.subscriptions.push(sub);

        assert!(check_due_subscriptions_at(&mut model, now).is_empty());
        assert!(!model.subscription_updates.contains(&sub_id));
    }

    #[test]
    fn compute_rate_zero_elapsed_returns_zero() {
        assert_eq!(compute_rate(0, 1_000_000, 0), 0);
        assert_eq!(compute_rate(100, 1_000, 0), 0);
    }

    #[test]
    fn compute_rate_counter_rollback_saturates_to_zero() {
        // sing-box restarted mid-session — totals reset; saturating_sub.
        assert_eq!(compute_rate(10_000, 500, 1_000), 0);
    }

    #[test]
    fn compute_rate_basic() {
        // 2000 bytes delta over 1000 ms = 2000 B/s
        assert_eq!(compute_rate(1_000, 3_000, 1_000), 2_000);
        // 1500 bytes delta over 500 ms = 3000 B/s
        assert_eq!(compute_rate(0, 1_500, 500), 3_000);
    }

    #[test]
    fn compute_rate_fractional_second() {
        // 100 bytes over 250 ms = 400 B/s
        assert_eq!(compute_rate(0, 100, 250), 400);
    }

    #[test]
    fn traffic_stats_updated_first_sample_records_zero_rate() {
        let mut model = model_with_profiles(vec![]);
        model.connection = ConnectionState::Connected;
        let effects = handle_traffic_stats_updated(&mut model, 0, 1, 10_000, 50_000, 3, 1_000);
        assert_eq!(model.traffic.up_total, 10_000);
        assert_eq!(model.traffic.down_total, 50_000);
        assert_eq!(model.traffic.conn_count, 3);
        // First sample → no prior baseline → rates remain zero.
        assert_eq!(model.traffic.up_rate_bps, 0);
        assert_eq!(model.traffic.down_rate_bps, 0);
        assert_eq!(model.last_traffic_sample_at_ms, 1_000);
        assert_eq!(model.last_traffic_response_id, 1);
        assert_eq!(effects, vec![Effect::BroadcastState]);
    }

    #[test]
    fn traffic_stats_updated_second_sample_computes_rate() {
        let mut model = model_with_profiles(vec![]);
        model.connection = ConnectionState::Connected;
        // Prime with a first sample.
        handle_traffic_stats_updated(&mut model, 0, 1, 0, 0, 0, 1_000);
        // 5000 ↑ + 12000 ↓ over 1 second.
        let effects = handle_traffic_stats_updated(&mut model, 0, 2, 5_000, 12_000, 7, 2_000);
        assert_eq!(model.traffic.up_rate_bps, 5_000);
        assert_eq!(model.traffic.down_rate_bps, 12_000);
        assert_eq!(model.traffic.conn_count, 7);
        assert_eq!(effects, vec![Effect::BroadcastState]);
    }

    #[test]
    fn traffic_stats_updated_handles_singbox_restart() {
        let mut model = model_with_profiles(vec![]);
        model.connection = ConnectionState::Connected;
        handle_traffic_stats_updated(&mut model, 0, 1, 10_000, 50_000, 5, 1_000);
        // sing-box restarts → counters reset, but we still get a sample.
        handle_traffic_stats_updated(&mut model, 0, 2, 100, 200, 1, 2_000);
        // Rate saturates to 0 for this transition sample.
        assert_eq!(model.traffic.up_rate_bps, 0);
        assert_eq!(model.traffic.down_rate_bps, 0);
        // Totals reflect the new (lower) baseline.
        assert_eq!(model.traffic.up_total, 100);
        assert_eq!(model.traffic.down_total, 200);
    }

    #[test]
    fn handle_tick_emits_fetch_when_connected() {
        let mut model = model_with_profiles(vec![]);
        model.connection = ConnectionState::Connected;
        model.last_traffic_fetch_at = None;
        let effects = handle_tick(&mut model);
        assert!(
            effects.iter().any(|e| matches!(
                e,
                Effect::FetchTrafficStats {
                    attempt_id: 0,
                    request_id: 1,
                }
            )),
            "expected Effect::FetchTrafficStats, got {:?}",
            effects
        );
        assert!(model.last_traffic_fetch_at.is_some());
        assert_eq!(model.traffic_request_id, 1);
    }

    #[test]
    fn traffic_stats_ignore_previous_connection_attempt() {
        let mut model = model_with_profiles(vec![]);
        model.connection = ConnectionState::Connected;
        model.connect_attempt_id = 2;

        let effects = handle_traffic_stats_updated(&mut model, 1, 1, 10, 20, 3, 1_000);

        assert!(effects.is_empty());
        assert_eq!(model.traffic, TrafficStats::default());
        assert_eq!(model.last_traffic_response_id, 0);
    }

    #[test]
    fn traffic_stats_ignore_response_when_not_connected() {
        let mut model = model_with_profiles(vec![]);

        let effects = handle_traffic_stats_updated(&mut model, 0, 1, 10, 20, 3, 1_000);

        assert!(effects.is_empty());
        assert_eq!(model.traffic, TrafficStats::default());
    }

    #[test]
    fn traffic_stats_do_not_roll_back_on_out_of_order_response() {
        let mut model = model_with_profiles(vec![]);
        model.connection = ConnectionState::Connected;
        handle_traffic_stats_updated(&mut model, 0, 2, 200, 400, 4, 2_000);

        let effects = handle_traffic_stats_updated(&mut model, 0, 1, 100, 200, 2, 1_000);

        assert!(effects.is_empty());
        assert_eq!(model.traffic.up_total, 200);
        assert_eq!(model.traffic.down_total, 400);
        assert_eq!(model.last_traffic_sample_at_ms, 2_000);
        assert_eq!(model.last_traffic_response_id, 2);
    }

    #[test]
    fn handle_tick_skips_fetch_when_idle() {
        let mut model = model_with_profiles(vec![]);
        model.connection = ConnectionState::Idle;
        let effects = handle_tick(&mut model);
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::FetchTrafficStats { .. })),
            "Idle connection must not poll Clash API"
        );
    }

    #[test]
    fn handle_tick_throttles_within_one_second() {
        let mut model = model_with_profiles(vec![]);
        model.connection = ConnectionState::Connected;
        // First tick → fetch emitted.
        let _ = handle_tick(&mut model);
        let first_at = model.last_traffic_fetch_at;
        // Second tick almost immediately → no additional fetch.
        let effects = handle_tick(&mut model);
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::FetchTrafficStats { .. })),
            "second tick within 1s must not emit FetchTrafficStats"
        );
        assert_eq!(model.last_traffic_fetch_at, first_at);
    }

    #[test]
    fn connected_resets_traffic_state() {
        let mut model = model_with_profiles(vec![Profile::new_vless(
            "A".into(),
            "1.1.1.1".into(),
            443,
            "u".into(),
        )]);
        model.traffic.up_total = 9999;
        model.traffic.down_total = 8888;
        model.traffic.up_rate_bps = 100;
        model.last_traffic_sample_at_ms = 1_000;
        model.last_traffic_fetch_at = Some(Instant::now());
        model.traffic_request_id = 4;
        model.last_traffic_response_id = 3;
        let id = model.config.profiles[0].id;
        let _ = update(
            &mut model,
            Msg::Connected {
                pid: 1234,
                profile_id: id,
                attempt_id: 0,
            },
        );
        assert_eq!(model.traffic, TrafficStats::default());
        assert_eq!(model.last_traffic_sample_at_ms, 0);
        assert!(model.last_traffic_fetch_at.is_none());
        assert_eq!(model.traffic_request_id, 0);
        assert_eq!(model.last_traffic_response_id, 0);
    }

    /// `t` opens the theme picker overlay and positions the cursor on the
    /// currently committed theme slug.
    #[test]
    fn t_key_opens_theme_picker_at_current_theme() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let mut model = model_with_profiles(vec![]);
        model.config.settings.theme = "gruvbox".into();
        let key = KeyEvent::new(KeyCode::Char('C'), crossterm::event::KeyModifiers::NONE);
        let _ = update(&mut model, Msg::Key(key));
        assert_eq!(model.overlay, Overlay::ThemeSettings);
        let slugs = crate::app::update::theme_picker_slugs();
        assert_eq!(
            slugs.get(model.theme_selected).map(String::as_str),
            Some("gruvbox")
        );
        assert!(model.theme_draft.is_none(), "draft starts cleared on open");
    }

    /// j/k inside the picker update both the cursor and the draft slug —
    /// the TUI client maps draft → live `model.theme` on snapshot apply.
    #[test]
    fn theme_picker_j_k_set_draft() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let mut model = model_with_profiles(vec![]);
        model.config.settings.theme = "tokyo-night".into();
        let _ = update(
            &mut model,
            Msg::Key(KeyEvent::new(
                KeyCode::Char('C'),
                crossterm::event::KeyModifiers::NONE,
            )),
        );
        let before = model.theme_selected;
        let _ = update(
            &mut model,
            Msg::Key(KeyEvent::new(
                KeyCode::Char('j'),
                crossterm::event::KeyModifiers::NONE,
            )),
        );
        assert_eq!(model.theme_selected, before + 1);
        let slugs = crate::app::update::theme_picker_slugs();
        assert_eq!(
            model.theme_draft.as_deref(),
            slugs.get(model.theme_selected).map(String::as_str)
        );
    }

    /// Enter persists the draft into `settings.theme`, clears the draft,
    /// closes the overlay, and emits SaveConfig (only when changed).
    #[test]
    fn theme_picker_enter_commits_and_saves() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let mut model = model_with_profiles(vec![]);
        model.config.settings.theme = "tokyo-night".into();
        let _ = update(
            &mut model,
            Msg::Key(KeyEvent::new(
                KeyCode::Char('C'),
                crossterm::event::KeyModifiers::NONE,
            )),
        );
        // Move cursor to a known slug.
        let slugs = crate::app::update::theme_picker_slugs();
        let target_idx = slugs
            .iter()
            .position(|s| s == "nord")
            .expect("nord present");
        model.theme_selected = target_idx;
        let effects = update(
            &mut model,
            Msg::Key(KeyEvent::new(
                KeyCode::Enter,
                crossterm::event::KeyModifiers::NONE,
            )),
        );
        assert_eq!(model.config.settings.theme, "nord");
        assert!(model.theme_draft.is_none());
        assert_eq!(model.overlay, Overlay::None);
        assert!(
            effects.iter().any(|e| matches!(e, Effect::SaveConfig)),
            "Enter must request SaveConfig"
        );
    }

    /// Esc discards the draft and closes the overlay without touching
    /// `settings.theme`.
    #[test]
    fn theme_picker_esc_discards_draft() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let mut model = model_with_profiles(vec![]);
        model.config.settings.theme = "tokyo-night".into();
        let _ = update(
            &mut model,
            Msg::Key(KeyEvent::new(
                KeyCode::Char('C'),
                crossterm::event::KeyModifiers::NONE,
            )),
        );
        let _ = update(
            &mut model,
            Msg::Key(KeyEvent::new(
                KeyCode::Char('j'),
                crossterm::event::KeyModifiers::NONE,
            )),
        );
        assert!(model.theme_draft.is_some());
        let _ = update(
            &mut model,
            Msg::Key(KeyEvent::new(
                KeyCode::Esc,
                crossterm::event::KeyModifiers::NONE,
            )),
        );
        assert_eq!(model.overlay, Overlay::None);
        assert!(model.theme_draft.is_none());
        assert_eq!(model.config.settings.theme, "tokyo-night");
    }

    /// `Msg::ThemeChanged` from the Omarchy watcher is a no-op whenever
    /// the user has picked an explicit non-`"omarchy"` theme.
    #[test]
    fn theme_changed_msg_ignored_when_manual_override_set() {
        let mut model = model_with_profiles(vec![]);
        model.config.settings.theme = "gruvbox".into();
        let original = model.theme;
        let _ = update(
            &mut model,
            Msg::ThemeChanged(crate::ui::styles::Theme::resolve("nord")),
        );
        assert_eq!(model.theme, original, "manual override blocks watcher");
    }

    /// `Msg::ThemeChanged` applies when the user is in Auto-follow mode.
    #[test]
    fn theme_changed_msg_applies_in_omarchy_mode() {
        let mut model = model_with_profiles(vec![]);
        model.config.settings.theme = "omarchy".into();
        let new_theme = crate::ui::styles::Theme::resolve("nord");
        let _ = update(&mut model, Msg::ThemeChanged(new_theme));
        assert_eq!(model.theme, new_theme);
    }

    /// On a non-Omarchy system the picker omits the Auto entry, so the
    /// list is exactly the 22 bundled palette names.
    #[test]
    fn theme_picker_omits_auto_when_omarchy_absent() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        unsafe { std::env::set_var("XDG_STATE_HOME", state.path()) };
        let slugs = crate::app::update::theme_picker_slugs();
        assert!(!slugs.iter().any(|s| s == "omarchy"));
        assert_eq!(slugs.len(), 22);
    }

    // ── Profile testing ──────────────────────────────────────────────────────

    #[test]
    fn t_key_with_no_profile_does_nothing() {
        let mut model = model_with_profiles(vec![]);
        let key = KeyEvent::new(KeyCode::Char('t'), crossterm::event::KeyModifiers::NONE);
        let _ = update(&mut model, Msg::Key(key));
        assert!(model.pending_tests.is_empty());
        assert!(model.testing_profiles.is_empty());
    }

    #[test]
    fn t_key_adds_selected_profile_to_pending() {
        let profiles = sample_profiles();
        let id = profiles[0].id;
        let mut model = model_with_profiles(profiles);
        model.selected = 0;
        let key = KeyEvent::new(KeyCode::Char('t'), crossterm::event::KeyModifiers::NONE);
        let _ = update(&mut model, Msg::Key(key));
        assert_eq!(model.pending_tests.len(), 1);
        assert_eq!(model.pending_tests[0], id);
    }

    #[test]
    fn t_key_does_not_duplicate_already_queued_profile() {
        let profiles = sample_profiles();
        let id = profiles[0].id;
        let mut model = model_with_profiles(profiles);
        model.selected = 0;
        let key = KeyEvent::new(KeyCode::Char('t'), crossterm::event::KeyModifiers::NONE);
        let _ = update(&mut model, Msg::Key(key));
        let _ = update(&mut model, Msg::Key(key));
        assert_eq!(model.pending_tests.len(), 1, "no duplicate enqueue");
        assert_eq!(model.pending_tests[0], id);
    }

    #[test]
    fn shift_t_adds_all_profiles_to_pending() {
        let profiles = sample_profiles();
        let count = profiles.len();
        let mut model = model_with_profiles(profiles);
        let key = KeyEvent::new(KeyCode::Char('T'), crossterm::event::KeyModifiers::NONE);
        let _ = update(&mut model, Msg::Key(key));
        assert_eq!(model.pending_tests.len(), count);
    }

    #[test]
    fn shift_t_does_not_duplicate_already_queued_profiles() {
        let profiles = sample_profiles();
        let count = profiles.len();
        let mut model = model_with_profiles(profiles);
        let key = KeyEvent::new(KeyCode::Char('T'), crossterm::event::KeyModifiers::NONE);
        let _ = update(&mut model, Msg::Key(key));
        let _ = update(&mut model, Msg::Key(key));
        assert_eq!(
            model.pending_tests.len(),
            count,
            "second T adds no duplicates"
        );
    }

    #[test]
    fn tick_dispatches_up_to_4_tests_from_pending() {
        let profiles = sample_profiles();
        // Need 5 profiles — add two more manually.
        let mut profiles5 = profiles;
        let extra_a = crate::config::profile::Profile::new_vless(
            "D".into(),
            "d.example.com".into(),
            443,
            "00000000-0000-0000-0000-000000000004".into(),
        );
        let extra_b = crate::config::profile::Profile::new_vless(
            "E".into(),
            "e.example.com".into(),
            443,
            "00000000-0000-0000-0000-000000000005".into(),
        );
        profiles5.push(extra_a);
        profiles5.push(extra_b);
        let mut model = model_with_profiles(profiles5);
        // Enqueue all 5.
        let key = KeyEvent::new(KeyCode::Char('T'), crossterm::event::KeyModifiers::NONE);
        let _ = update(&mut model, Msg::Key(key));
        assert_eq!(model.pending_tests.len(), 5);
        // Tick should dispatch first 4.
        let effects = update(&mut model, Msg::Tick);
        let test_effects: Vec<_> = effects
            .iter()
            .filter(|e| matches!(e, Effect::TestProfile { .. }))
            .collect();
        assert_eq!(test_effects.len(), 4);
        assert_eq!(model.testing_profiles.len(), 4);
        assert_eq!(model.pending_tests.len(), 1, "one still waiting");
    }

    #[test]
    fn test_result_ok_stores_latency_and_clears_testing() {
        let profiles = sample_profiles();
        let id = profiles[0].id;
        let mut model = model_with_profiles(profiles);
        model.testing_profiles.insert(id);
        let effects = update(
            &mut model,
            Msg::TestResult {
                id,
                latency_ms: Some(42),
            },
        );
        assert!(!model.testing_profiles.contains(&id));
        assert_eq!(model.profile_latencies.get(&id), Some(&Some(42)));
        assert!(effects.iter().any(|e| matches!(e, Effect::BroadcastState)));
    }

    #[test]
    fn test_result_err_stores_none_and_clears_testing() {
        let profiles = sample_profiles();
        let id = profiles[0].id;
        let mut model = model_with_profiles(profiles);
        model.profile_latencies.insert(id, Some(100));
        model.testing_profiles.insert(id);
        let effects = update(
            &mut model,
            Msg::TestResult {
                id,
                latency_ms: None,
            },
        );
        assert!(!model.testing_profiles.contains(&id));
        assert_eq!(
            model.profile_latencies.get(&id),
            Some(&None),
            "failure stores None (shown as err in UI)"
        );
        assert!(effects.iter().any(|e| matches!(e, Effect::BroadcastState)));
    }

    #[test]
    fn manual_geo_update_is_blocked_only_by_disconnected_kill_switch() {
        let mut direct = model_with_profiles(vec![]);
        direct.config.settings.geo_routing.set_region(GeoRegion::Ru);
        let effects = update(&mut direct, Msg::Key(key('u')));
        assert!(effects.contains(&Effect::DownloadGeo));

        let mut blocked = model_with_profiles(vec![]);
        blocked
            .config
            .settings
            .geo_routing
            .set_region(GeoRegion::Ru);
        blocked.config.settings.kill_switch = true;
        let effects = update(&mut blocked, Msg::Key(key('u')));
        assert!(!effects.contains(&Effect::DownloadGeo));
        assert!(!blocked.geo_updating);
        assert!(effects.iter().any(|effect| matches!(
            effect,
            Effect::AppendAppLog { message, .. }
                if message.contains("blocked by the kill switch")
        )));

        blocked.connection = ConnectionState::Connected;
        let effects = update(&mut blocked, Msg::Key(key('u')));
        assert!(effects.contains(&Effect::DownloadGeo));
    }

    #[test]
    fn pasted_subscription_is_saved_but_not_fetched_behind_kill_switch() {
        let mut model = model_with_profiles(vec![]);
        model.config.settings.kill_switch = true;
        let effects = handle_clipboard_text(&mut model, "https://example.com/sub");

        assert_eq!(model.config.subscriptions.len(), 1);
        assert!(!model.subscription_fetching);
        assert!(model.subscription_updates.is_empty());
        assert!(effects.contains(&Effect::SaveConfig));
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, Effect::UpdateSubscription { .. }))
        );
        assert!(effects.iter().any(|effect| matches!(
            effect,
            Effect::AppendAppLog { message, .. }
                if message.contains("Subscription update is blocked")
        )));
    }

    #[test]
    fn pasted_subscription_can_fetch_directly_or_through_vpn() {
        let mut direct = model_with_profiles(vec![]);
        let effects = handle_clipboard_text(&mut direct, "https://example.com/direct");
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::UpdateSubscription { .. }))
        );

        let mut tunneled = model_with_profiles(vec![]);
        tunneled.config.settings.kill_switch = true;
        tunneled.connection = ConnectionState::Connected;
        let effects = handle_clipboard_text(&mut tunneled, "https://example.com/tunneled");
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::UpdateSubscription { .. }))
        );
    }

    #[test]
    fn automatic_service_update_can_run_directly_without_kill_switch() {
        let mut model = model_with_profiles(vec![]);
        model
            .config
            .settings
            .geo_routing
            .set_region(GeoRegion::Global);
        model.config.settings.geo_routing.auto_update = GeoAutoUpdate::Every1d;
        model.config.settings.geo_routing.service_routes.insert(
            RoutedService::Steam,
            crate::config::profile::ServiceRoute::Direct,
        );
        let now = after_update_window();
        model
            .service_next_updates
            .insert(RoutedService::Steam, now.date_naive());

        let effects = handle_tick_at(&mut model, now);
        assert!(effects.iter().any(|effect| matches!(
            effect,
            Effect::RetryServiceRuleSets { services }
                if services == &vec![RoutedService::Steam]
        )));
    }
}
