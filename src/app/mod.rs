use std::path::PathBuf;
use std::process::Child;

use anyhow::Result;
use uuid::Uuid;

use crate::config::profile::{Config, Profile, Protocol};
use crate::config::{load_config, save_config};
use crate::geo::GeoManager;

pub mod services;

/// Current screen mode of the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    Normal,
    Help,
    ConfirmDelete,
    ConfirmQuit,
    CreateProfile,
    PasteUri,
    Connecting,
    Connected,
    Error,
    RoutingMode,
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

/// Application state shared across the UI and background tasks.
pub struct App {
    pub mode: AppMode,
    pub config: Config,
    pub selected: usize,
    pub status: AppStatus,
    pub singbox_process: Option<Child>,
    pub connecting: bool,
    pub logs: Vec<String>,
    pub log_scroll: usize,
    pub input_buffer: String,
    pub input_field: InputField,
    pub draft_profile: Option<Profile>,
    pub active_profile_id: Option<Uuid>,
    pub routing_selected: usize,
    pub needs_redraw: bool,
    pub geo_manager: GeoManager,
    pub geo_updating: bool,
    log_tailer: services::LogTailer,
    geo_updater: services::GeoUpdater,
    suspend_watcher: services::SuspendWatcher,
}

/// Which input field is currently being edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputField {
    None,
    Name,
    Address,
    Port,
    Uuid,
    Protocol,
}

impl InputField {
    /// Human-readable label for the field.
    pub fn label(self) -> &'static str {
        match self {
            InputField::Name => "Profile Name",
            InputField::Address => "Server Address",
            InputField::Port => "Server Port",
            InputField::Uuid => "UUID / Password",
            InputField::Protocol => "Protocol (vless)",
            InputField::None => "Input",
        }
    }

    /// Apply the buffered input to the draft profile.
    pub fn apply(self, draft: &mut Profile, buffer: &str) {
        let trimmed = buffer.trim();
        match self {
            InputField::Name => draft.name = trimmed.to_string(),
            InputField::Address => draft.address = trimmed.to_string(),
            InputField::Port => {
                if let Ok(port) = trimmed.parse::<u16>() {
                    draft.port = port;
                }
            }
            InputField::Uuid => draft.uuid = trimmed.to_string(),
            InputField::Protocol => {
                // Currently only VLESS is supported.
                draft.protocol = Protocol::Vless;
            }
            InputField::None => {}
        }
    }

    /// Return the next field in the creation/editing sequence.
    pub fn next(self) -> Option<Self> {
        match self {
            InputField::Name => Some(InputField::Address),
            InputField::Address => Some(InputField::Port),
            InputField::Port => Some(InputField::Uuid),
            InputField::Uuid => Some(InputField::Protocol),
            InputField::Protocol | InputField::None => None,
        }
    }

    /// Return a default buffer value from the draft profile for this field.
    pub fn default_buffer(self, draft: &Profile) -> String {
        match self {
            InputField::Name => draft.name.clone(),
            InputField::Address => draft.address.clone(),
            InputField::Port => draft.port.to_string(),
            InputField::Uuid => draft.uuid.clone(),
            InputField::Protocol => draft.protocol.to_string(),
            InputField::None => String::new(),
        }
    }
}

/// Maximum number of log lines kept in the UI buffer.
const MAX_LOG_LINES: usize = 1000;

impl App {
    /// Initialize application state and load persisted configuration.
    pub fn new() -> Result<Self> {
        let config = load_config().unwrap_or_default();
        let geo_manager = GeoManager::new().unwrap_or_else(|e| {
            tracing::warn!("Failed to initialize geo manager: {}", e);
            // Create a dummy manager that will fail gracefully
            GeoManager::new().expect("GeoManager creation should not fail")
        });
        if let Err(e) = geo_manager.ensure_databases() {
            tracing::warn!("Failed to ensure geo databases: {}", e);
        }
        let selected = config.resolve_selected();

        Ok(Self {
            mode: AppMode::Normal,
            config,
            selected,
            status: AppStatus::Info("Press ? for help".to_string()),
            singbox_process: None,
            connecting: false,
            logs: Vec::new(),
            log_scroll: 0,
            input_buffer: String::new(),
            input_field: InputField::None,
            draft_profile: None,
            active_profile_id: None,
            routing_selected: 0,
            needs_redraw: false,
            geo_manager,
            geo_updating: false,
            log_tailer: services::LogTailer::new(crate::paths::singbox_log_path()),
            geo_updater: services::GeoUpdater::new(),
            suspend_watcher: services::SuspendWatcher::new(),
        })
    }

    /// Save current configuration to disk.
    pub fn save(&self) -> Result<()> {
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
            if let Err(e) = self.save() {
                tracing::warn!("Failed to save config after delete: {}", e);
            }
            self.status = AppStatus::Info("Profile deleted".to_string());
        }
        self.mode = AppMode::Normal;
    }

    /// Add a new profile and persist.
    pub fn add_profile(&mut self, profile: Profile) {
        self.config.profiles.push(profile);
        if let Err(e) = self.save() {
            tracing::warn!("Failed to save config after add: {}", e);
        }
        self.selected = self.config.profiles.len().saturating_sub(1);
    }

    /// Set status message and switch to normal mode.
    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status = AppStatus::Info(msg.into());
        self.mode = AppMode::Normal;
    }

    /// Set error message and switch to error mode.
    pub fn set_error(&mut self, msg: impl Into<String>) {
        self.status = AppStatus::Error(msg.into());
        self.mode = AppMode::Error;
    }

    /// Append a line to the in-application log buffer.
    pub fn push_log(&mut self, line: impl Into<String>) {
        self.logs.push(line.into());
        if self.logs.len() > MAX_LOG_LINES {
            self.logs.remove(0);
        }
        self.log_scroll = self.logs.len().saturating_sub(1);
    }

    /// Periodic tick for background updates.
    pub fn on_tick(&mut self) {
        let connected = self.mode == AppMode::Connected;

        if self.suspend_watcher.check(connected) {
            self.push_log("[suspend] Resume detected");
            self.status = AppStatus::Info("Resumed from suspend — reconnecting…".to_string());
            self.mode = AppMode::Connecting;
            self.push_log("[suspend] Triggering reconnect");
        }

        for line in self.log_tailer.tail() {
            self.push_log(line);
        }

        let geo_messages = self.geo_updater.poll();
        for msg in geo_messages {
            self.geo_updating = false;
            self.push_log(format!("[geo] {}", msg));

            // If update was successful and VPN is connected, trigger reconnect.
            if !msg.starts_with("Error")
                && !msg.starts_with("Up to date")
                && self.singbox_process.is_some()
            {
                self.push_log("[geo] Reconnecting to apply new geo databases");
                self.mode = AppMode::Connecting;
            } else if msg.starts_with("Up to date") {
                self.status = AppStatus::Info("Geo databases are up to date".to_string());
            } else {
                self.status = AppStatus::Info(msg.clone());
            }
        }
    }

    /// Trigger a background geo database update.
    pub fn trigger_geo_update(&mut self) {
        if self.geo_updating {
            self.status = AppStatus::Info("Geo update already in progress".to_string());
            return;
        }

        self.geo_updating = true;
        self.status = AppStatus::Info("Checking for geo updates...".to_string());
        self.geo_updater.trigger();
    }

    /// Return a human-readable string of the last geo update time.
    pub fn geo_last_updated(&self) -> Option<String> {
        self.geo_manager.last_updated()
    }

    /// Set the path to the sing-box log file to tail in the UI.
    pub fn set_singbox_log_path(&mut self, path: PathBuf) {
        self.log_tailer.set_path(path);
    }

    /// Read new lines from the sing-box log file and append them to the UI log buffer.
    #[cfg(test)]
    pub fn tail_singbox_logs(&mut self) {
        for line in self.log_tailer.tail() {
            self.push_log(line);
        }
    }
}

#[cfg(test)]
impl App {
    /// Create an App instance for testing with a given config.
    pub fn test_new(config: Config) -> Self {
        let geo_manager = GeoManager::new().unwrap_or_else(|e| {
            eprintln!("Warning: failed to initialize geo manager in test: {}", e);
            GeoManager::new().unwrap()
        });
        let selected = config.resolve_selected();

        Self {
            mode: AppMode::Normal,
            config,
            selected,
            status: AppStatus::Info(String::new()),
            singbox_process: None,
            connecting: false,
            logs: Vec::new(),
            log_scroll: 0,
            input_buffer: String::new(),
            input_field: InputField::None,
            draft_profile: None,
            active_profile_id: None,
            routing_selected: 0,
            needs_redraw: false,
            geo_manager,
            geo_updating: false,
            log_tailer: services::LogTailer::new(crate::paths::singbox_log_path()),
            geo_updater: services::GeoUpdater::new(),
            suspend_watcher: services::SuspendWatcher::test_new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::profile::{Profile, Protocol};
    use crate::test_helpers::*;
    use std::io::Write;

    #[test]
    fn select_next_basic() {
        let mut app = app_with_profiles(sample_profiles());
        assert_eq!(app.selected, 0);
        app.select_next();
        assert_eq!(app.selected, 1);
        app.select_next();
        assert_eq!(app.selected, 2);
        app.select_next();
        assert_eq!(app.selected, 2); // clamp at last
    }

    #[test]
    fn select_prev_basic() {
        let mut app = app_with_profiles(sample_profiles());
        app.selected = 2;
        app.select_prev();
        assert_eq!(app.selected, 1);
        app.select_prev();
        assert_eq!(app.selected, 0);
        app.select_prev();
        assert_eq!(app.selected, 0); // saturate at 0
    }

    #[test]
    fn select_first_last() {
        let mut app = app_with_profiles(sample_profiles());
        app.selected = 1;
        app.select_first();
        assert_eq!(app.selected, 0);
        app.select_last();
        assert_eq!(app.selected, 2);
    }

    #[test]
    fn select_next_empty_list() {
        let mut app = app_with_profiles(vec![]);
        app.select_next();
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn selected_profile_some_and_none() {
        let mut app = app_with_profiles(sample_profiles());
        assert_eq!(app.selected_profile().unwrap().name, "A");
        app.config.profiles.clear();
        assert!(app.selected_profile().is_none());
    }

    #[test]
    fn delete_selected_basic() {
        let mut app = app_with_profiles(sample_profiles());
        app.selected = 1;
        app.delete_selected();
        assert_eq!(app.config.profiles.len(), 2);
        assert_eq!(app.config.profiles[0].name, "A");
        assert_eq!(app.config.profiles[1].name, "C");
        assert_eq!(app.selected, 1);
        assert_eq!(app.mode, AppMode::Normal);
    }

    #[test]
    fn delete_selected_last_item() {
        let mut app = app_with_profiles(sample_profiles());
        app.selected = 2;
        app.delete_selected();
        assert_eq!(app.config.profiles.len(), 2);
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn delete_selected_only_item() {
        let mut app = app_with_profiles(vec![Profile::new(
            "Only".to_string(),
            Protocol::Vless,
            "1.1.1.1".to_string(),
            443,
            "u".to_string(),
        )]);
        app.selected = 0;
        app.delete_selected();
        assert!(app.config.profiles.is_empty());
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn add_profile_updates_state() {
        let mut app = app_with_profiles(sample_profiles());
        let p = Profile::new(
            "D".to_string(),
            Protocol::Vless,
            "4.4.4.4".to_string(),
            443,
            "u4".to_string(),
        );
        app.add_profile(p);
        assert_eq!(app.config.profiles.len(), 4);
        assert_eq!(app.selected, 3);
    }

    #[test]
    fn push_log_and_truncate() {
        let mut app = app_with_profiles(vec![]);
        for i in 0..1005 {
            app.push_log(format!("line {}", i));
        }
        assert_eq!(app.logs.len(), 1000);
        assert_eq!(app.logs[0], "line 5");
        assert_eq!(app.logs[999], "line 1004");
        assert_eq!(app.log_scroll, 999);
    }

    #[test]
    fn set_status_clears_error_and_mode() {
        let mut app = app_with_profiles(vec![]);
        app.mode = AppMode::Error;
        app.set_error("oops");
        app.set_status("ok");
        assert_eq!(app.status.text(), "ok");
        assert!(!app.status.is_error());
        assert_eq!(app.mode, AppMode::Normal);
    }

    #[test]
    fn set_error_sets_message_and_mode() {
        let mut app = app_with_profiles(vec![]);
        app.set_error("fail");
        assert_eq!(app.status.text(), "fail");
        assert!(app.status.is_error());
        assert_eq!(app.mode, AppMode::Error);
    }

    #[test]
    fn tail_singbox_logs_reads_new_lines() {
        let mut app = app_with_profiles(vec![]);
        let mut temp = tempfile::NamedTempFile::new().unwrap();
        writeln!(temp, "log line 1").unwrap();
        writeln!(temp, "log line 2").unwrap();
        let path = temp.path().to_path_buf();
        app.set_singbox_log_path(path);
        app.tail_singbox_logs();
        assert_eq!(app.logs.len(), 2);
        assert!(app.logs[0].contains("log line 1"));
        assert!(app.logs[1].contains("log line 2"));
    }

    #[test]
    fn tail_singbox_logs_resets_on_rotation() {
        let mut app = app_with_profiles(vec![]);
        let mut temp = tempfile::NamedTempFile::new().unwrap();
        writeln!(temp, "this is a long old log line").unwrap();
        let path = temp.path().to_path_buf();
        app.set_singbox_log_path(path.clone());
        app.tail_singbox_logs();
        assert_eq!(app.logs.len(), 1);

        // Simulate rotation: file shrinks
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, "new").unwrap();
        drop(file);

        app.tail_singbox_logs();
        assert_eq!(app.logs.len(), 2);
        assert!(app.logs[1].contains("new"));
    }
}
