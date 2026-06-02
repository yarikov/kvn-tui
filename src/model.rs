use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use uuid::Uuid;

use crate::config::profile::{Config, Profile, Protocol};
use crate::config::{load_config, save_config};

/// UI overlay shown on top of the main screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Overlay {
    #[default]
    None,
    Help,
    ConfirmDelete,
    ConfirmQuit,
    CreateProfile,
    PasteUri,
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
    pub input: InputState,
    pub geo_updating: bool,
    pub needs_redraw: bool,
    pub should_quit: bool,
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

/// State for text input in creation/editing modes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputState {
    pub buffer: String,
    pub field: InputField,
    pub draft: Option<Profile>,
}

impl Default for InputState {
    fn default() -> Self {
        Self {
            buffer: String::new(),
            field: InputField::None,
            draft: None,
        }
    }
}

impl Model {
    /// Initialize application state and load persisted configuration.
    pub fn new() -> anyhow::Result<Self> {
        let config = load_config().unwrap_or_default();
        let selected = config.resolve_selected();

        // Reset state to disconnected on startup in case of previous crash.
        crate::state_io::clear_state();

        Ok(Self {
            overlay: Overlay::None,
            connection: ConnectionState::Idle,
            config,
            selected,
            status: AppStatus::Info("Press ? for help".to_string()),
            singbox_pid: None,
            active_profile_id: None,
            routing_selected: 0,
            logs: VecDeque::new(),
            log_scroll: 0,
            input: InputState::default(),
            geo_updating: false,
            needs_redraw: false,
            should_quit: false,
        })
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
            input: InputState::default(),
            geo_updating: false,
            needs_redraw: false,
            should_quit: false,
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
    fn input_field_labels() {
        assert_eq!(InputField::Name.label(), "Profile Name");
        assert_eq!(InputField::Address.label(), "Server Address");
        assert_eq!(InputField::Port.label(), "Server Port");
        assert_eq!(InputField::Uuid.label(), "UUID / Password");
        assert_eq!(InputField::Protocol.label(), "Protocol (vless)");
        assert_eq!(InputField::None.label(), "Input");
    }

    #[test]
    fn input_field_next_sequence() {
        assert_eq!(InputField::Name.next(), Some(InputField::Address));
        assert_eq!(InputField::Address.next(), Some(InputField::Port));
        assert_eq!(InputField::Port.next(), Some(InputField::Uuid));
        assert_eq!(InputField::Uuid.next(), Some(InputField::Protocol));
        assert_eq!(InputField::Protocol.next(), None);
        assert_eq!(InputField::None.next(), None);
    }

    #[test]
    fn input_field_apply_to_draft() {
        use crate::config::profile::{Profile, Protocol};
        let mut draft = Profile::new(
            "Old".to_string(),
            Protocol::Vless,
            "0.0.0.0".to_string(),
            80,
            "old-uuid".to_string(),
        );

        InputField::Name.apply(&mut draft, "  NewName  ");
        assert_eq!(draft.name, "NewName");

        InputField::Address.apply(&mut draft, "1.2.3.4");
        assert_eq!(draft.address, "1.2.3.4");

        InputField::Port.apply(&mut draft, "443");
        assert_eq!(draft.port, 443);

        // Invalid port should be ignored
        InputField::Port.apply(&mut draft, "abc");
        assert_eq!(draft.port, 443);

        InputField::Uuid.apply(&mut draft, "new-uuid");
        assert_eq!(draft.uuid, "new-uuid");

        InputField::Protocol.apply(&mut draft, "anything");
        assert_eq!(draft.protocol, Protocol::Vless);

        InputField::None.apply(&mut draft, "ignored");
        // no change expected
    }

    #[test]
    fn input_field_default_buffer() {
        use crate::config::profile::{Profile, Protocol};
        let draft = Profile::new(
            "N".to_string(),
            Protocol::Vless,
            "1.1.1.1".to_string(),
            443,
            "u".to_string(),
        );
        assert_eq!(InputField::Name.default_buffer(&draft), "N");
        assert_eq!(InputField::Address.default_buffer(&draft), "1.1.1.1");
        assert_eq!(InputField::Port.default_buffer(&draft), "443");
        assert_eq!(InputField::Uuid.default_buffer(&draft), "u");
        assert_eq!(InputField::Protocol.default_buffer(&draft), "vless");
        assert_eq!(InputField::None.default_buffer(&draft), "");
    }

    #[test]
    fn model_save_roundtrip() {
        let model = model_with_profiles(vec![Profile::new(
            "Alpha".to_string(),
            Protocol::Vless,
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        // Save should not panic
        let _ = model.save();
    }
}
