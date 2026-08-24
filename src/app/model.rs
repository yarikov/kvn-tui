use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;
use uuid::Uuid;

use crate::config::profile::{
    Config, DnsStrategy, GeoRegion, Profile, RoutedService, ServiceRoute,
};
use crate::config::{load_config, save_config};
use crate::ui::styles::Theme;
use chrono::{DateTime, Local};

/// UI overlay shown on top of the main screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Overlay {
    #[default]
    None,
    Help,
    ConfirmDelete,
    RoutingMode,
    GeoRegions,
    DnsSettings,
    ThemeSettings,
    ServiceRouting,
}

/// A single selectable row in the unified Sources list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceRow {
    StandaloneProfile(usize),
    SubscriptionHeader(usize),
    SubscriptionProfile { sub_idx: usize, profile_idx: usize },
}

/// VPN connection state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ConnectionState {
    #[default]
    Idle,
    Connecting,
    ConnectPending,
    Connected,
}

/// Typed status message for the application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppStatus {
    Info(String),
    Error(String),
}

impl AppStatus {
    /// Return the text content of the status.
    pub fn text(&self) -> &str {
        match self {
            AppStatus::Info(s) | AppStatus::Error(s) => s.as_str(),
        }
    }

    /// Returns true if this is an error status.
    #[cfg(test)]
    pub fn is_error(&self) -> bool {
        matches!(self, AppStatus::Error(_))
    }
}

/// Serializable application state for external integrations (waybar, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppState {
    pub connected: bool,
    pub profile_name: Option<String>,
    pub active_profile_id: Option<String>,
    pub singbox_pid: Option<u32>,
}

/// Live traffic statistics scraped from sing-box's Clash API. Default values
/// (all zeros) mean "no data yet" and the status bar segment is hidden.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrafficStats {
    /// Instantaneous upload rate in bytes per second.
    #[serde(default)]
    pub up_rate_bps: u64,
    /// Instantaneous download rate in bytes per second.
    #[serde(default)]
    pub down_rate_bps: u64,
    /// Cumulative upload bytes since sing-box started.
    #[serde(default)]
    pub up_total: u64,
    /// Cumulative download bytes since sing-box started.
    #[serde(default)]
    pub down_total: u64,
    /// Number of currently active connections proxied by sing-box.
    #[serde(default)]
    pub conn_count: usize,
}

/// Maximum number of log lines kept in the UI buffer.
pub(crate) const MAX_LOG_LINES: usize = 1000;

/// Build the flat list of selectable rows for a given config.
fn source_rows(config: &Config) -> Vec<SourceRow> {
    let mut rows = Vec::new();
    for (idx, profile) in config.profiles.iter().enumerate() {
        if profile.subscription_id.is_none() {
            rows.push(SourceRow::StandaloneProfile(idx));
        }
    }
    for (sub_idx, sub) in config.subscriptions.iter().enumerate() {
        rows.push(SourceRow::SubscriptionHeader(sub_idx));
        for (profile_idx, profile) in config.profiles.iter().enumerate() {
            if profile.subscription_id == Some(sub.id) {
                rows.push(SourceRow::SubscriptionProfile {
                    sub_idx,
                    profile_idx,
                });
            }
        }
    }
    rows
}

/// Return the flat row index that points at the given config profile index.
pub(crate) fn row_for_profile(config: &Config, profile_idx: usize) -> usize {
    source_rows(config)
        .iter()
        .position(|row| match row {
            SourceRow::StandaloneProfile(idx) => *idx == profile_idx,
            SourceRow::SubscriptionProfile {
                profile_idx: idx, ..
            } => *idx == profile_idx,
            _ => false,
        })
        .unwrap_or(0)
}

/// Return the flat row index that points at the given subscription header.
pub(crate) fn row_for_subscription_header(config: &Config, sub_idx: usize) -> usize {
    source_rows(config)
        .iter()
        .position(|row| matches!(row, SourceRow::SubscriptionHeader(idx) if *idx == sub_idx))
        .unwrap_or(0)
}

/// Application data model — no side effects.
pub struct Model {
    pub overlay: Overlay,
    pub connection: ConnectionState,
    pub config: Config,
    pub selected: usize,
    pub status: AppStatus,
    pub singbox_pid: Option<u32>,
    pub active_profile_id: Option<Uuid>,
    /// Explicit target for a queued connection attempt. This must never be
    /// inferred from `selected`, which is only the UI cursor.
    pub connecting_profile_id: Option<Uuid>,
    /// Monotonic token identifying the only connection attempt whose result
    /// may still mutate the model. Incremented for every queued connection
    /// and invalidated by the daemon on disconnect.
    pub connect_attempt_id: u64,
    /// Desired kill-switch state while the privileged helper is running.
    /// Prevents repeated `K` presses from launching concurrent systemd/nft
    /// operations whose replies could arrive out of order.
    pub kill_switch_pending: Option<bool>,
    pub routing_selected: usize,
    pub geo_region_selected: usize,
    pub dns_selected: usize,
    /// Pending DNS strategy preview while the DNS overlay is open. Set by
    /// `h`/`l` on the Strategy row, committed by Enter, discarded by Esc.
    pub dns_strategy_draft: Option<DnsStrategy>,
    /// Pending Fake-IP preview while the DNS overlay is open. Set by `h`/`l`
    /// on the Fake-IP row, committed by Enter, discarded by Esc.
    pub dns_fakeip_draft: Option<bool>,
    /// Cursor position inside the service routing overlay (index into
    /// `RoutedService::ALL`).
    pub service_routing_selected: usize,
    /// Draft copy of `geo_routing.service_routes` while the service routing
    /// overlay is open. Committed as a whole by Enter, discarded by Esc — a
    /// separate map keeps partially edited routes out of the running
    /// configuration until the user confirms them.
    pub service_routing_draft: Option<HashMap<RoutedService, ServiceRoute>>,
    /// Set when a service-routing commit is waiting for its rule-sets to be
    /// fetched through the still-active tunnel; `Msg::ServiceRuleSetsReady`
    /// consumes it and triggers the reconnect. Downloading BEFORE the
    /// reconnect matters: reconnecting first would build the new sing-box
    /// config while the files are still missing, leaving the freshly enabled
    /// service inert until a second manual reconnect.
    pub pending_service_reconnect: bool,
    pub logs: VecDeque<String>,
    pub log_scroll: usize,
    pub geo_updating: bool,
    pub subscription_fetching: bool,
    pub subscription_updates: HashSet<Uuid>,
    pub automatic_subscription_updates: HashSet<Uuid>,
    pub needs_redraw: bool,
    pub should_quit: bool,
    pub geo_last_updated: Option<String>,
    /// Last successful remote check for the active geo region.
    pub geo_last_checked_at: Option<DateTime<Local>>,
    /// Last check attempt in this daemon session. Prevents an overdue failed
    /// check from being retried on every 250 ms tick.
    pub geo_last_attempt_at: Option<DateTime<Local>>,
    pub geo_retry_state: Option<crate::geo::GeoRetryState>,
    pub geo_next_update: Option<chrono::NaiveDate>,
    pub service_retry_states: HashMap<RoutedService, crate::geo::GeoRetryState>,
    pub service_checked_at: HashMap<RoutedService, DateTime<Local>>,
    pub service_next_updates: HashMap<RoutedService, chrono::NaiveDate>,
    pub geo_automatic_update: bool,
    /// Latest traffic stats sample, applied either by the daemon (after a
    /// Clash-API fetch) or by the TUI client (from a `StateSnapshot`).
    pub traffic: TrafficStats,
    /// Wall-clock time of the previous traffic sample, in ms since Unix
    /// epoch. Used by the pure reducer to compute rate deltas. Set to 0
    /// when there is no prior sample.
    pub last_traffic_sample_at_ms: u64,
    /// Monotonic ID assigned to the next Clash API poll for this connection.
    pub traffic_request_id: u64,
    /// Newest traffic response accepted by the reducer. Older replies from
    /// overlapping workers are ignored instead of rolling counters back.
    pub last_traffic_response_id: u64,
    /// When the daemon last issued an `Effect::FetchTrafficStats`. Used by
    /// `handle_tick` to throttle Clash-API polling to ~1 Hz. Not serialized —
    /// clients don't need it.
    pub last_traffic_fetch_at: Option<Instant>,
    /// UI color theme. Owned by the TUI client; the daemon never reads it.
    /// On Omarchy systems the client resolves it from `theme.name` at
    /// startup and updates it on file change.
    pub theme: Theme,
    /// Cursor position inside the theme picker overlay.
    pub theme_selected: usize,
    /// Live-preview theme slug while the theme picker is open. `None`
    /// outside the overlay; cleared on Enter/Esc.
    pub theme_draft: Option<String>,
    /// Latency results from profile tests. `Some(ms)` = success, `None` = error.
    /// Absent key = not yet tested. Transient — never persisted to disk.
    pub profile_latencies: HashMap<Uuid, Option<u64>>,
    /// Profile UUIDs whose latency test is currently running (shows spinner).
    pub testing_profiles: HashSet<Uuid>,
    /// Queue of profile UUIDs waiting to be tested (batch dispatch).
    pub pending_tests: VecDeque<Uuid>,
}

impl Model {
    /// Push a line to the log buffer, truncating if needed.
    pub fn push_log(&mut self, line: String) {
        self.logs.push_back(line);
        if self.logs.len() > MAX_LOG_LINES {
            self.logs.pop_front();
        }
        self.log_scroll = self.logs.len().saturating_sub(1);
    }

    /// Set application status and also push it to the in-memory logs panel.
    /// This method is side-effect-free: it does not write to disk.
    pub fn set_status(&mut self, status: AppStatus) {
        let text = status.text();
        if !text.is_empty() {
            self.push_log(text.to_string());
        }
        self.status = status;
    }

    /// Initialize application state and load persisted configuration.
    ///
    /// Load and validation errors fall back to [`Config::default`] — the
    /// daemon must always start so the TUI can surface the problem to the
    /// user. The captured error is stashed and applied to the status (which
    /// also appends to the log panel) after the [`Model`] is constructed
    /// (see the `startup_error` block near the end of this function).
    pub fn new() -> anyhow::Result<Self> {
        let mut startup_error: Option<String> = None;
        let config = match load_config() {
            Ok(cfg) => match cfg.validate() {
                Ok(()) => cfg,
                Err(e) => {
                    let msg = format!("Config invalid, using defaults: {e:#}");
                    tracing::error!("{msg}");
                    startup_error = Some(msg);
                    Config::default()
                }
            },
            Err(e) => {
                let msg = format!("Failed to load config, using defaults: {e:#}");
                tracing::error!("{msg}");
                startup_error = Some(msg);
                Config::default()
            }
        };
        let default_selected = 0;

        // A live PID in state.json can only be left by a crashed daemon: q
        // detaches the TUI but deliberately leaves the daemon itself running.
        // Stop the orphan before a normal auto-connect can launch a competing
        // sing-box instance for the same TUN and API ports.
        let stale_cleanup_failed = match crate::services::waybar::cleanup_stale_session() {
            Ok(stale_pid) => {
                if let Some(pid) = stale_pid {
                    tracing::warn!("Stopped stale sing-box process {pid} during crash recovery");
                }
                crate::services::waybar::clear_state();
                false
            }
            Err(e) => {
                let msg = format!("Failed to clean up stale VPN process: {e:#}");
                tracing::error!("{msg}");
                startup_error = Some(match startup_error.take() {
                    Some(previous) => format!("{previous}; {msg}"),
                    None => msg,
                });
                true
            }
        };

        let (mut connection, selected, mut status) =
            Self::resolve_startup_state(&config, default_selected);
        if stale_cleanup_failed {
            // Never launch a second sing-box while the stale process may still
            // own the TUN interface or Clash API port.
            connection = ConnectionState::Idle;
        }

        // Block auto-connect until the user has picked a geo region.
        if config.settings.geo_routing.current_region.is_none() {
            connection = ConnectionState::Idle;
            status = AppStatus::Info("Press ? for help".to_string());
        }
        let connecting_profile_id = (connection == ConnectionState::Connecting)
            .then_some(config.settings.last_connected_profile)
            .flatten();

        let region = config
            .settings
            .geo_routing
            .current_region
            .unwrap_or(GeoRegion::Global);
        let geo_manager = crate::geo::GeoManager::new().ok();
        if let Some(manager) = &geo_manager {
            let _ = manager.ensure_update_schedules(
                region,
                &config.settings.geo_routing.enabled_services(),
                config.settings.geo_routing.auto_update
                    != crate::config::profile::GeoAutoUpdate::Off,
            );
        }
        let geo_last_updated = geo_manager.as_ref().and_then(|g| g.last_updated(region));
        let geo_last_checked_at = geo_manager.as_ref().and_then(|g| g.last_checked_at(region));
        let geo_retry_state = geo_manager.as_ref().and_then(|g| g.retry_state(region));
        let geo_next_update = geo_manager
            .as_ref()
            .and_then(|g| g.region_next_update(region));
        let service_retry_states = geo_manager
            .as_ref()
            .map(|g| g.service_retry_states())
            .unwrap_or_default();
        let service_checked_at = geo_manager
            .as_ref()
            .map(|g| g.service_checked_at())
            .unwrap_or_default();
        let service_next_updates = geo_manager
            .map(|g| g.service_next_updates())
            .unwrap_or_default();

        let mut model = Self {
            overlay: Overlay::None,
            connection,
            config,
            selected,
            status: AppStatus::Info(String::new()),
            singbox_pid: None,
            active_profile_id: None,
            connecting_profile_id,
            connect_attempt_id: u64::from(connecting_profile_id.is_some()),
            kill_switch_pending: None,
            routing_selected: 0,
            geo_region_selected: 0,
            dns_selected: 0,
            dns_strategy_draft: None,
            dns_fakeip_draft: None,
            service_routing_selected: 0,
            service_routing_draft: None,
            pending_service_reconnect: false,
            logs: VecDeque::new(),
            log_scroll: 0,
            geo_updating: false,
            subscription_fetching: false,
            subscription_updates: HashSet::new(),
            automatic_subscription_updates: HashSet::new(),
            needs_redraw: false,
            should_quit: false,
            geo_last_updated,
            geo_last_checked_at,
            geo_last_attempt_at: None,
            geo_retry_state,
            geo_next_update,
            service_retry_states,
            service_checked_at,
            service_next_updates,
            geo_automatic_update: false,
            traffic: TrafficStats::default(),
            last_traffic_sample_at_ms: 0,
            traffic_request_id: 0,
            last_traffic_response_id: 0,
            last_traffic_fetch_at: None,
            theme: Theme::legacy(),
            theme_selected: 0,
            theme_draft: None,
            profile_latencies: HashMap::new(),
            testing_profiles: HashSet::new(),
            pending_tests: VecDeque::new(),
        };
        if model.config.settings.geo_routing.current_region.is_none() {
            model.overlay = Overlay::GeoRegions;
        }
        if status.text() == "Press ? for help" {
            model.status = status;
        } else {
            model.set_status(status);
        }
        // A load/validate failure at startup must reach the user. set_status
        // populates the status bar and the in-memory log panel; the extra
        // append_app_log write persists it to `~/.config/kvn-tui/logs/app.log`
        // so the user can still see the reason after restarting the TUI.
        if let Some(msg) = startup_error {
            crate::services::log_tailer::append_app_log("ERROR", &msg);
            model.set_status(AppStatus::Error(msg));
        }
        Ok(model)
    }

    /// Build a Model from an already-loaded config (used by the TUI client
    /// after it reloads profiles.json independently of the daemon).
    pub fn from_config(config: Config) -> Self {
        let selected = 0;
        let region = config
            .settings
            .geo_routing
            .current_region
            .unwrap_or(GeoRegion::Global);
        let geo_manager = crate::geo::GeoManager::new().ok();
        if let Some(manager) = &geo_manager {
            let _ = manager.ensure_update_schedules(
                region,
                &config.settings.geo_routing.enabled_services(),
                config.settings.geo_routing.auto_update
                    != crate::config::profile::GeoAutoUpdate::Off,
            );
        }
        let geo_last_updated = geo_manager.as_ref().and_then(|g| g.last_updated(region));
        let geo_last_checked_at = geo_manager.as_ref().and_then(|g| g.last_checked_at(region));
        let geo_retry_state = geo_manager.as_ref().and_then(|g| g.retry_state(region));
        let geo_next_update = geo_manager
            .as_ref()
            .and_then(|g| g.region_next_update(region));
        let service_retry_states = geo_manager
            .as_ref()
            .map(|g| g.service_retry_states())
            .unwrap_or_default();
        let service_checked_at = geo_manager
            .as_ref()
            .map(|g| g.service_checked_at())
            .unwrap_or_default();
        let service_next_updates = geo_manager
            .map(|g| g.service_next_updates())
            .unwrap_or_default();

        let mut model = Self {
            overlay: Overlay::None,
            connection: ConnectionState::Idle,
            config,
            selected,
            status: AppStatus::Info(String::new()),
            singbox_pid: None,
            active_profile_id: None,
            connecting_profile_id: None,
            connect_attempt_id: 0,
            kill_switch_pending: None,
            routing_selected: 0,
            geo_region_selected: 0,
            dns_selected: 0,
            dns_strategy_draft: None,
            dns_fakeip_draft: None,
            service_routing_selected: 0,
            service_routing_draft: None,
            pending_service_reconnect: false,
            logs: VecDeque::new(),
            log_scroll: 0,
            geo_updating: false,
            subscription_fetching: false,
            subscription_updates: HashSet::new(),
            automatic_subscription_updates: HashSet::new(),
            needs_redraw: false,
            should_quit: false,
            geo_last_updated,
            geo_last_checked_at,
            geo_last_attempt_at: None,
            geo_retry_state,
            geo_next_update,
            service_retry_states,
            service_checked_at,
            service_next_updates,
            geo_automatic_update: false,
            traffic: TrafficStats::default(),
            last_traffic_sample_at_ms: 0,
            traffic_request_id: 0,
            last_traffic_response_id: 0,
            last_traffic_fetch_at: None,
            theme: Theme::legacy(),
            theme_selected: 0,
            theme_draft: None,
            profile_latencies: HashMap::new(),
            testing_profiles: HashSet::new(),
            pending_tests: VecDeque::new(),
        };
        if model.config.settings.geo_routing.current_region.is_none() {
            model.overlay = Overlay::GeoRegions;
        }
        model
    }

    /// Determine connection state, selection and status on startup.
    /// If auto-connect is enabled and the last connected profile exists,
    /// returns `Connecting` state targeted at that profile.
    fn resolve_startup_state(
        config: &Config,
        default_selected: usize,
    ) -> (ConnectionState, usize, AppStatus) {
        if config.settings.auto_connect
            && let Some(idx) = config
                .settings
                .last_connected_profile
                .and_then(|id| config.profiles.iter().position(|p| p.id == id))
        {
            let status = if let Some(profile) = config.profiles.get(idx) {
                AppStatus::Info(format!("Auto-connecting to {}…", profile.name))
            } else {
                AppStatus::Info("Press ? for help".to_string())
            };
            return (
                ConnectionState::Connecting,
                row_for_profile(config, idx),
                status,
            );
        }
        (
            ConnectionState::Idle,
            default_selected,
            AppStatus::Info("Press ? for help".to_string()),
        )
    }

    /// Save current configuration to disk.
    pub fn save(&self) -> anyhow::Result<()> {
        save_config(&self.config)
    }

    /// Build the flat list of selectable rows for the current config.
    pub fn source_rows(&self) -> Vec<SourceRow> {
        source_rows(&self.config)
    }

    /// Return the currently selected row, if any.
    pub fn selected_row(&self) -> Option<SourceRow> {
        self.source_rows().get(self.selected).copied()
    }

    /// Move selection down by one item.
    pub fn select_next(&mut self) {
        let len = self.source_rows().len();
        crate::ui::nav::select_next(&mut self.selected, len);
    }

    /// Move selection up by one item.
    pub fn select_prev(&mut self) {
        crate::ui::nav::select_prev(&mut self.selected);
    }

    /// Jump to the first item.
    pub fn select_first(&mut self) {
        self.selected = 0;
    }

    /// Jump to the last item.
    pub fn select_last(&mut self) {
        let len = self.source_rows().len();
        self.selected = len.saturating_sub(1);
    }

    /// Return the currently selected profile, if any.
    pub fn selected_profile(&self) -> Option<&Profile> {
        match self.selected_row()? {
            SourceRow::StandaloneProfile(idx) => self.config.profiles.get(idx),
            SourceRow::SubscriptionProfile { profile_idx, .. } => {
                self.config.profiles.get(profile_idx)
            }
            _ => None,
        }
    }

    /// Return the currently selected subscription, if any.
    pub fn selected_subscription(&self) -> Option<&crate::config::profile::Subscription> {
        match self.selected_row()? {
            SourceRow::SubscriptionHeader(idx) => self.config.subscriptions.get(idx),
            _ => None,
        }
    }

    /// Return the index of the currently selected subscription, if any.
    pub fn selected_subscription_index(&self) -> Option<usize> {
        match self.selected_row()? {
            SourceRow::SubscriptionHeader(idx) => Some(idx),
            _ => None,
        }
    }

    /// Replace the config while keeping the cursor on the same source item.
    /// If the selected item was removed, clamp the previous row to the new list.
    pub fn replace_config_preserving_selection(&mut self, config: Config) {
        let selected_row = self.selected;
        let selected_profile_id = self.selected_profile().map(|profile| profile.id);
        let selected_subscription_id = self.selected_subscription().map(|sub| sub.id);

        self.config = config;
        self.selected = selected_profile_id
            .and_then(|id| {
                self.config
                    .profiles
                    .iter()
                    .position(|profile| profile.id == id)
            })
            .map(|idx| row_for_profile(&self.config, idx))
            .or_else(|| {
                selected_subscription_id
                    .and_then(|id| {
                        self.config
                            .subscriptions
                            .iter()
                            .position(|sub| sub.id == id)
                    })
                    .map(|idx| row_for_subscription_header(&self.config, idx))
            })
            .unwrap_or_else(|| selected_row.min(self.source_rows().len().saturating_sub(1)));
    }

    /// Remove the currently selected source item after confirmation.
    pub fn delete_selected(&mut self) {
        let rows = self.source_rows();
        let Some(row) = rows.get(self.selected).copied() else {
            return;
        };
        match row {
            SourceRow::StandaloneProfile(idx)
            | SourceRow::SubscriptionProfile {
                profile_idx: idx, ..
            } => {
                if idx < self.config.profiles.len() {
                    self.config.profiles.remove(idx);
                }
            }
            SourceRow::SubscriptionHeader(idx) => {
                if let Some(sub) = self.config.subscriptions.get(idx) {
                    let sub_id = sub.id;
                    self.config
                        .profiles
                        .retain(|p| p.subscription_id != Some(sub_id));
                    self.config.subscriptions.remove(idx);
                }
            }
        }
        self.overlay = Overlay::None;
        let len = self.source_rows().len();
        if self.selected >= len && len > 0 {
            self.selected = len - 1;
        }
    }

    /// Add a new standalone profile and move selection to it.
    pub fn add_profile(&mut self, profile: Profile) {
        self.config.profiles.push(profile);
        self.selected = row_for_profile(&self.config, self.config.profiles.len() - 1);
    }

    /// Check whether a profile with the same credentials already exists.
    pub fn has_duplicate(&self, profile: &Profile) -> bool {
        let key = profile.dedup_key();
        self.config.profiles.iter().any(|p| p.dedup_key() == key)
    }
}

#[cfg(test)]
impl Model {
    /// Create a Model instance for testing with a given config.
    pub fn test_new(config: Config) -> Self {
        let selected = 0;
        Self {
            overlay: Overlay::None,
            connection: ConnectionState::Idle,
            config,
            selected,
            status: AppStatus::Info(String::new()),
            singbox_pid: None,
            active_profile_id: None,
            connecting_profile_id: None,
            connect_attempt_id: 0,
            kill_switch_pending: None,
            routing_selected: 0,
            geo_region_selected: 0,
            dns_selected: 0,
            dns_strategy_draft: None,
            dns_fakeip_draft: None,
            service_routing_selected: 0,
            service_routing_draft: None,
            pending_service_reconnect: false,
            logs: VecDeque::new(),
            log_scroll: 0,
            geo_updating: false,
            subscription_fetching: false,
            subscription_updates: HashSet::new(),
            automatic_subscription_updates: HashSet::new(),
            needs_redraw: false,
            should_quit: false,
            geo_last_updated: None,
            geo_last_checked_at: None,
            geo_last_attempt_at: None,
            geo_retry_state: None,
            geo_next_update: None,
            service_retry_states: HashMap::new(),
            service_checked_at: HashMap::new(),
            service_next_updates: HashMap::new(),
            geo_automatic_update: false,
            traffic: TrafficStats::default(),
            last_traffic_sample_at_ms: 0,
            traffic_request_id: 0,
            last_traffic_response_id: 0,
            last_traffic_fetch_at: None,
            theme: Theme::legacy(),
            theme_selected: 0,
            theme_draft: None,
            profile_latencies: HashMap::new(),
            testing_profiles: HashSet::new(),
            pending_tests: VecDeque::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::*;

    #[test]
    fn select_next_basic() {
        let mut model = model_with_profiles(sample_profiles());
        assert_eq!(model.selected, 0);
        model.select_next();
        assert_eq!(model.selected, 1);
        model.select_next();
        assert_eq!(model.selected, 2);
        model.select_next();
        assert_eq!(model.selected, 2); // clamp at last
    }

    #[test]
    fn select_prev_basic() {
        let mut model = model_with_profiles(sample_profiles());
        model.selected = 2;
        model.select_prev();
        assert_eq!(model.selected, 1);
        model.select_prev();
        assert_eq!(model.selected, 0);
        model.select_prev();
        assert_eq!(model.selected, 0); // saturate at 0
    }

    #[test]
    fn select_first_last() {
        let mut model = model_with_profiles(sample_profiles());
        model.selected = 1;
        model.select_first();
        assert_eq!(model.selected, 0);
        model.select_last();
        assert_eq!(model.selected, 2);
    }

    #[test]
    fn select_next_empty_list() {
        let mut model = model_with_profiles(vec![]);
        model.select_next();
        assert_eq!(model.selected, 0);
    }

    #[test]
    fn selected_profile_some_and_none() {
        let mut model = model_with_profiles(sample_profiles());
        assert_eq!(model.selected_profile().unwrap().name, "A");
        model.config.profiles.clear();
        assert!(model.selected_profile().is_none());
    }

    #[test]
    fn delete_selected_basic() {
        let mut model = model_with_profiles(sample_profiles());
        model.selected = 1;
        model.delete_selected();
        assert_eq!(model.config.profiles.len(), 2);
        assert_eq!(model.config.profiles[0].name, "A");
        assert_eq!(model.config.profiles[1].name, "C");
        assert_eq!(model.selected, 1);
        assert_eq!(model.overlay, Overlay::None);
    }

    #[test]
    fn delete_selected_last_item() {
        let mut model = model_with_profiles(sample_profiles());
        model.selected = 2;
        model.delete_selected();
        assert_eq!(model.config.profiles.len(), 2);
        assert_eq!(model.selected, 1);
    }

    #[test]
    fn delete_selected_only_item() {
        let mut model = model_with_profiles(vec![Profile::new_vless(
            "Only".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u".to_string(),
        )]);
        model.selected = 0;
        model.delete_selected();
        assert!(model.config.profiles.is_empty());
        assert_eq!(model.selected, 0);
    }

    #[test]
    fn add_profile_updates_state() {
        let mut model = model_with_profiles(sample_profiles());
        let p = Profile::new_vless(
            "D".to_string(),
            "4.4.4.4".to_string(),
            443,
            "u4".to_string(),
        );
        model.add_profile(p);
        assert_eq!(model.config.profiles.len(), 4);
        assert_eq!(model.selected, 3);
    }

    #[test]
    fn set_status_clears_error_and_mode() {
        let mut model = model_with_profiles(vec![]);
        model.status = AppStatus::Error("oops".into());
        model.status = AppStatus::Info("ok".into());
        assert_eq!(model.status.text(), "ok");
        assert!(!model.status.is_error());
    }

    #[test]
    fn set_error_sets_message_and_mode() {
        let mut model = model_with_profiles(vec![]);
        model.status = AppStatus::Error("fail".into());
        assert_eq!(model.status.text(), "fail");
        assert!(model.status.is_error());
    }

    #[test]
    fn resolve_startup_state_auto_connect() {
        let mut config = Config::default();
        let profile = Profile::new_vless(
            "Auto".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        );
        let id = profile.id;
        config.profiles.push(profile);
        config.settings.auto_connect = true;
        config.settings.last_connected_profile = Some(id);

        let (state, selected, status) = Model::resolve_startup_state(&config, 0);
        assert_eq!(state, ConnectionState::Connecting);
        assert_eq!(selected, 0);
        assert!(status.text().contains("Auto-connecting"));
    }

    #[test]
    fn resolve_startup_state_auto_connect_missing_profile() {
        let mut config = Config::default();
        config.settings.auto_connect = true;
        config.settings.last_connected_profile = Some(Uuid::new_v4());
        config.profiles.push(Profile::new_vless(
            "A".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        ));

        let (state, selected, status) = Model::resolve_startup_state(&config, 0);
        assert_eq!(state, ConnectionState::Idle);
        assert_eq!(selected, 0);
        assert_eq!(status.text(), "Press ? for help");
    }

    #[test]
    fn resolve_startup_state_no_auto_connect() {
        let config = Config::default();
        let (state, selected, status) = Model::resolve_startup_state(&config, 0);
        assert_eq!(state, ConnectionState::Idle);
        assert_eq!(selected, 0);
        assert_eq!(status.text(), "Press ? for help");
    }

    #[test]
    fn new_blocks_auto_connect_when_geo_region_none() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };

        let mut config = Config::default();
        let profile = Profile::new_vless(
            "Auto".to_string(),
            "1.1.1.1".to_string(),
            443,
            crate::test_helpers::TEST_UUID.to_string(),
        );
        let id = profile.id;
        config.profiles.push(profile);
        config.settings.auto_connect = true;
        config.settings.last_connected_profile = Some(id);
        // current_region is None → overlay must be shown and auto-connect blocked

        crate::config::save_config(&config).unwrap();

        let model = Model::new().unwrap();
        assert_eq!(model.connection, ConnectionState::Idle);
        assert_eq!(model.overlay, Overlay::GeoRegions);
    }

    #[test]
    fn new_surfaces_invalid_config_in_status_and_log() {
        // profiles.json that parses but fails semantic validation (garbage
        // UUID) must not silently swallow the error — the TUI needs to show
        // it via the red status bar, the in-memory log panel, and the on-disk
        // app.log so the user can still see it after restarting.
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };

        // Model::new writes to app.log, which requires the logs subdirectory
        // to exist; production code calls ensure_config_dirs() in main().
        crate::paths::ensure_config_dirs().unwrap();

        let path = crate::paths::profiles_path().unwrap();
        // Handwritten JSON — bypasses save_config's fail-close guard.
        let json = r#"{
            "schema_version": 2,
            "profiles": [{
                "name": "Broken",
                "protocol": "vless",
                "address": "1.2.3.4",
                "port": 443,
                "uuid": "not-a-uuid"
            }]
        }"#;
        std::fs::write(&path, json).unwrap();

        let model = Model::new().unwrap();
        assert!(
            model.status.is_error(),
            "status not error: {:?}",
            model.status
        );
        assert!(
            model.status.text().contains("Config invalid"),
            "status was: {}",
            model.status.text(),
        );
        // Fell back to defaults, so the broken profile is not visible.
        assert!(model.config.profiles.is_empty());
        // Error is also persisted in the in-memory log panel.
        assert!(
            model.logs.iter().any(|l| l.contains("Config invalid")),
            "logs: {:?}",
            model.logs,
        );
        // …and on disk in app.log.
        let on_disk = std::fs::read_to_string(crate::paths::app_log_path()).unwrap();
        assert!(
            on_disk.contains("Config invalid") && on_disk.contains("ERROR"),
            "app.log was: {on_disk:?}",
        );
    }

    #[test]
    fn new_surfaces_load_error_when_json_is_malformed() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };

        let path = crate::paths::profiles_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not json at all").unwrap();

        let model = Model::new().unwrap();
        assert!(model.status.is_error());
        assert!(
            model.status.text().contains("Failed to load"),
            "status was: {}",
            model.status.text(),
        );
        assert!(
            model.logs.iter().any(|l| l.contains("Failed to load")),
            "logs: {:?}",
            model.logs,
        );
    }

    #[test]
    fn model_save_roundtrip() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };

        let model = model_with_profiles(vec![Profile::new_vless(
            "Alpha".to_string(),
            "1.1.1.1".to_string(),
            443,
            crate::test_helpers::TEST_UUID.to_string(),
        )]);
        // Save should not panic and must write into the temp directory.
        let _ = model.save();
        assert!(crate::paths::profiles_path().unwrap().exists());
    }

    #[test]
    fn push_log_truncates_and_updates_scroll() {
        let mut model = Model::test_new(Config::default());
        for i in 0..1005 {
            model.push_log(format!("line {}", i));
        }
        assert_eq!(model.logs.len(), 1000);
        assert_eq!(model.logs[0], "line 5");
        assert_eq!(model.logs[999], "line 1004");
        assert_eq!(model.log_scroll, 999);
    }

    #[test]
    fn set_status_logs_plain_text() {
        let mut model = Model::test_new(Config::default());
        model.set_status(AppStatus::Info("hello".into()));
        assert_eq!(model.status.text(), "hello");
        assert_eq!(model.logs.back().unwrap(), "hello");

        model.set_status(AppStatus::Error("oops".into()));
        assert_eq!(model.status.text(), "oops");
        assert_eq!(model.logs.back().unwrap(), "oops");
    }

    #[test]
    fn set_status_does_not_log_empty() {
        let mut model = Model::test_new(Config::default());
        model.set_status(AppStatus::Info("".into()));
        assert!(model.logs.is_empty());
    }
}
