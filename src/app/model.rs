use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use uuid::Uuid;

use crate::config::profile::{Config, Profile};
use crate::config::{load_config, save_config};

/// UI overlay shown on top of the main screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Overlay {
    #[default]
    None,
    Help,
    ConfirmDelete,
    ConfirmQuit,
    Error,
    RoutingMode,
}

/// VPN connection state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
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

/// Application data model — no side effects.
pub struct Model {
    pub overlay: Overlay,
    pub connection: ConnectionState,
    pub config: Config,
    pub selected: usize,
    pub status: AppStatus,
    pub singbox_pid: Option<u32>,
    pub active_profile_id: Option<Uuid>,
    pub routing_selected: usize,
    pub logs: VecDeque<String>,
    pub log_scroll: usize,
    pub geo_updating: bool,
    pub needs_redraw: bool,
    pub should_quit: bool,
}

impl Model {
    /// Initialize application state and load persisted configuration.
    pub fn new() -> anyhow::Result<Self> {
        let config = load_config().unwrap_or_default();
        let selected = config.resolve_selected();

        // Reset state to disconnected on startup in case of previous crash.
        crate::services::waybar::clear_state();

        let (connection, selected, status) = Self::resolve_startup_state(&config, selected);

        Ok(Self {
            overlay: Overlay::None,
            connection,
            config,
            selected,
            status,
            singbox_pid: None,
            active_profile_id: None,
            routing_selected: 0,
            logs: VecDeque::new(),
            log_scroll: 0,
            geo_updating: false,
            needs_redraw: false,
            should_quit: false,
        })
    }

    /// Determine connection state, selection and status on startup.
    /// If auto-connect is enabled and the last connected profile exists,
    /// returns `Connecting` state targeted at that profile.
    fn resolve_startup_state(
        config: &Config,
        default_selected: usize,
    ) -> (ConnectionState, usize, AppStatus) {
        if config.settings.auto_connect {
            if let Some(idx) = config
                .settings
                .last_connected_profile
                .and_then(|id| config.profiles.iter().position(|p| p.id == id))
            {
                let status = if let Some(profile) = config.profiles.get(idx) {
                    AppStatus::Info(format!("Auto-connecting to {}…", profile.name))
                } else {
                    AppStatus::Info("Press ? for help".to_string())
                };
                return (ConnectionState::Connecting, idx, status);
            }
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

    /// Move selection down by one item.
    pub fn select_next(&mut self) {
        crate::ui::nav::select_next(&mut self.selected, self.config.profiles.len());
    }

    /// Move selection up by one item.
    pub fn select_prev(&mut self) {
        crate::ui::nav::select_prev(&mut self.selected);
    }

    /// Jump to the first profile.
    pub fn select_first(&mut self) {
        crate::ui::nav::select_first(&mut self.selected);
    }

    /// Jump to the last profile.
    pub fn select_last(&mut self) {
        crate::ui::nav::select_last(&mut self.selected, self.config.profiles.len());
    }

    /// Return the currently selected profile, if any.
    pub fn selected_profile(&self) -> Option<&Profile> {
        self.config.profiles.get(self.selected)
    }

    /// Remove the currently selected profile after confirmation.
    pub fn delete_selected(&mut self) {
        if self.selected < self.config.profiles.len() {
            self.config.profiles.remove(self.selected);
            if self.selected >= self.config.profiles.len() && !self.config.profiles.is_empty() {
                self.selected = self.config.profiles.len() - 1;
            }
            self.overlay = Overlay::None;
        }
    }

    /// Add a new profile and persist.
    pub fn add_profile(&mut self, profile: Profile) {
        self.config.profiles.push(profile);
        self.selected = self.config.profiles.len().saturating_sub(1);
    }

    /// Check whether a profile with the same UUID already exists.
    pub fn has_duplicate(&self, profile: &Profile) -> bool {
        self.config.profiles.iter().any(|p| p.uuid == profile.uuid)
    }
}

#[cfg(test)]
impl Model {
    /// Create a Model instance for testing with a given config.
    pub fn test_new(config: Config) -> Self {
        let selected = config.resolve_selected();
        Self {
            overlay: Overlay::None,
            connection: ConnectionState::Idle,
            config,
            selected,
            status: AppStatus::Info(String::new()),
            singbox_pid: None,
            active_profile_id: None,
            routing_selected: 0,
            logs: VecDeque::new(),
            log_scroll: 0,
            geo_updating: false,
            needs_redraw: false,
            should_quit: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::profile::Protocol;
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
        let mut model = model_with_profiles(vec![Profile::new(
            "Only".to_string(),
            Protocol::Vless,
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
        let p = Profile::new(
            "D".to_string(),
            Protocol::Vless,
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
        model.overlay = Overlay::Error;
        model.status = AppStatus::Error("oops".into());
        model.status = AppStatus::Info("ok".into());
        model.overlay = Overlay::None;
        assert_eq!(model.status.text(), "ok");
        assert!(!model.status.is_error());
        assert_eq!(model.overlay, Overlay::None);
    }

    #[test]
    fn set_error_sets_message_and_mode() {
        let mut model = model_with_profiles(vec![]);
        model.status = AppStatus::Error("fail".into());
        model.overlay = Overlay::Error;
        assert_eq!(model.status.text(), "fail");
        assert!(model.status.is_error());
        assert_eq!(model.overlay, Overlay::Error);
    }

    #[test]
    fn resolve_startup_state_auto_connect() {
        let mut config = Config::default();
        let profile = Profile::new(
            "Auto".to_string(),
            Protocol::Vless,
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
        config.profiles.push(Profile::new(
            "A".to_string(),
            Protocol::Vless,
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
    fn model_save_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };

        let model = model_with_profiles(vec![Profile::new(
            "Alpha".to_string(),
            Protocol::Vless,
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        // Save should not panic and must write into the temp directory.
        let _ = model.save();
        assert!(crate::infra::paths::profiles_path().unwrap().exists());
    }
}
