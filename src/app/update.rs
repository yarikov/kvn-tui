use crate::app::effect::Effect;
use crate::app::model::{ConnectionState, InputField, Model, Overlay};
use crate::app::msg::{GeoResult, Msg};
use crate::config::profile::{Profile, Protocol, RoutingMode};
use crossterm::event::{KeyCode, KeyEvent};

/// Maximum number of log lines kept in the UI buffer.
const MAX_LOG_LINES: usize = 1000;

/// Pure function: Model + Msg → updated Model + list of Effects.
/// No I/O, no threads, no system calls.
pub fn update(model: &mut Model, msg: Msg) -> Vec<Effect> {
    match msg {
        Msg::Key(key) => handle_key(model, key),
        Msg::Tick => handle_tick(model),
        Msg::LogLine(line) => {
            model.logs.push_back(line);
            if model.logs.len() > MAX_LOG_LINES {
                model.logs.pop_front();
            }
            model.log_scroll = model.logs.len().saturating_sub(1);
            vec![]
        }
        Msg::GeoUpdated(result) => handle_geo_result(model, result),
        Msg::SystemResumed => {
            if model.connection == ConnectionState::Connected {
                model.status = crate::app::model::AppStatus::Info("Resumed — reconnecting…".into());
                let profile = model.selected_profile().cloned();
                let settings = model.config.settings.clone();
                profile
                    .map(|p| {
                        vec![Effect::Connect {
                            profile: p,
                            settings,
                        }]
                    })
                    .unwrap_or_default()
            } else {
                vec![]
            }
        }
        Msg::Connected { pid } => {
            model.singbox_pid = Some(pid);
            model.connection = ConnectionState::Connected;
            model.overlay = Overlay::None;
            let mut effects = vec![Effect::WriteState];
            if let Some(profile) = model.selected_profile() {
                let profile_id = profile.id;
                let profile_name = profile.name.clone();
                model.active_profile_id = Some(profile_id);
                model.status =
                    crate::app::model::AppStatus::Info(format!("Connected to {}", profile_name));
                // Persist last connected profile for auto-connect on next startup.
                if model.config.settings.last_connected_profile != Some(profile_id) {
                    model.config.settings.last_connected_profile = Some(profile_id);
                    effects.push(Effect::SaveConfig);
                }
            }
            effects
        }
        Msg::ConnectFailed(err) => {
            model.connection = ConnectionState::Idle;
            model.overlay = Overlay::Error;
            model.status =
                crate::app::model::AppStatus::Error(format!("Connection failed: {}", err));
            vec![]
        }
        Msg::ClipboardRead(result) => match result {
            Ok(text) => handle_clipboard_text(model, &text),
            Err(e) => {
                model.status =
                    crate::app::model::AppStatus::Error(format!("Clipboard error: {}", e));
                vec![]
            }
        },
        Msg::EditorClosed(result) => {
            model.needs_redraw = true;
            match result {
                Ok(config) => {
                    model.config = config;
                    model.selected = model.config.resolve_selected();
                    model.status =
                        crate::app::model::AppStatus::Info("Profiles updated from editor".into());
                    vec![]
                }
                Err(e) => {
                    model.status =
                        crate::app::model::AppStatus::Error(format!("Editor failed: {}", e));
                    vec![]
                }
            }
        }
        Msg::Resize => {
            model.needs_redraw = true;
            vec![]
        }
    }
}

fn handle_tick(model: &mut Model) -> Vec<Effect> {
    let mut effects = Vec::new();

    // Tail logs
    effects.push(Effect::TailLogs);

    // Check geo updates — in the new architecture geo runs in its own thread
    // and sends GeoUpdated messages, so nothing to do here directly.

    // Connection handling
    if model.connection == ConnectionState::Connecting {
        if let Some(profile) = model.selected_profile().cloned() {
            let settings = model.config.settings.clone();
            effects.push(Effect::Connect { profile, settings });
        } else {
            model.connection = ConnectionState::Idle;
            model.overlay = Overlay::None;
        }
    }

    effects
}

fn handle_key(model: &mut Model, key: KeyEvent) -> Vec<Effect> {
    match model.overlay {
        Overlay::None => handle_main(model, key),
        Overlay::Help => {
            model.overlay = Overlay::None;
            vec![]
        }
        Overlay::ConfirmDelete => handle_confirm_delete(model, key),
        Overlay::ConfirmQuit => handle_confirm_quit(model, key),
        Overlay::RoutingMode => handle_routing_mode(model, key),
        Overlay::CreateProfile | Overlay::PasteUri => handle_input_mode(model, key),
        Overlay::Error => {
            model.overlay = Overlay::None;
            vec![]
        }
    }
}

fn handle_main(model: &mut Model, key: KeyEvent) -> Vec<Effect> {
    match key.code {
        // Navigation
        KeyCode::Char('j') | KeyCode::Down => {
            model.select_next();
        }
        KeyCode::Char('k') | KeyCode::Up => {
            model.select_prev();
        }
        KeyCode::Char('g') => {
            model.select_first();
        }
        KeyCode::Char('G') => {
            model.select_last();
        }

        // Actions
        KeyCode::Enter if model.selected_profile().is_some() => {
            model.connection = ConnectionState::Connecting;
        }
        KeyCode::Char('p') => {
            return vec![Effect::PasteClipboard];
        }
        KeyCode::Char('n') => {
            model.overlay = Overlay::CreateProfile;
            model.input.field = InputField::Name;
            model.input.buffer.clear();
            model.input.draft = Some(Profile::new(
                String::new(),
                Protocol::Vless,
                String::new(),
                443,
                String::new(),
            ));
        }

        KeyCode::Char('d') if model.selected_profile().is_some() => {
            model.overlay = Overlay::ConfirmDelete;
        }
        KeyCode::Char('m') => {
            model.overlay = Overlay::RoutingMode;
            model.routing_selected = model.config.settings.routing_mode.index();
        }
        KeyCode::Char('u') if !model.geo_updating => {
            model.geo_updating = true;
            model.status =
                crate::app::model::AppStatus::Info("Checking for geo updates...".to_string());
            return vec![Effect::DownloadGeo];
        }
        KeyCode::Char('e') => {
            return vec![Effect::OpenEditor(model.selected)];
        }
        KeyCode::Char('r') if model.connection == ConnectionState::Connected => {
            model.connection = ConnectionState::Connecting;
        }
        KeyCode::Char('s') if model.connection == ConnectionState::Connected => {
            return vec![Effect::Disconnect];
        }
        KeyCode::Char('a') => {
            let new_val = !model.config.settings.auto_connect;
            model.config.settings.auto_connect = new_val;
            model.status = crate::app::model::AppStatus::Info(format!(
                "Auto-connect {}",
                if new_val { "enabled" } else { "disabled" }
            ));
            return vec![Effect::SaveConfig];
        }

        // Help and quit
        KeyCode::Char('?') => model.overlay = Overlay::Help,
        KeyCode::Char('q') | KeyCode::Esc => {
            if model.connection == ConnectionState::Connected {
                model.overlay = Overlay::ConfirmQuit;
            } else {
                return vec![Effect::Quit];
            }
        }

        _ => {}
    }
    vec![]
}

fn handle_confirm_delete(model: &mut Model, key: KeyEvent) -> Vec<Effect> {
    match key.code {
        KeyCode::Char('y') | KeyCode::Enter => {
            model.delete_selected();
            return vec![Effect::SaveConfig];
        }
        KeyCode::Char('n') | KeyCode::Esc => {
            model.overlay = Overlay::None;
        }
        _ => {}
    }
    vec![]
}

fn handle_confirm_quit(model: &mut Model, key: KeyEvent) -> Vec<Effect> {
    match key.code {
        KeyCode::Char('y') | KeyCode::Enter => {
            return vec![Effect::Quit];
        }
        KeyCode::Char('n') | KeyCode::Esc => {
            model.overlay = Overlay::None;
        }
        _ => {}
    }
    vec![]
}

fn handle_routing_mode(model: &mut Model, key: KeyEvent) -> Vec<Effect> {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            crate::ui::nav::select_next(&mut model.routing_selected, RoutingMode::ALL.len());
        }
        KeyCode::Char('k') | KeyCode::Up => {
            crate::ui::nav::select_prev(&mut model.routing_selected);
        }
        KeyCode::Char('g') => {
            crate::ui::nav::select_first(&mut model.routing_selected);
        }
        KeyCode::Char('G') => {
            crate::ui::nav::select_last(&mut model.routing_selected, RoutingMode::ALL.len());
        }
        KeyCode::Enter => {
            if let Some(mode) = RoutingMode::from_index(model.routing_selected) {
                let changed = model.config.settings.routing_mode != mode;
                model.config.settings.routing_mode = mode;
                model.overlay = Overlay::None;
                model.status =
                    crate::app::model::AppStatus::Info(format!("Routing mode: {}", mode.as_str()));

                let effects = vec![Effect::SaveConfig];
                if changed && model.connection == ConnectionState::Connected {
                    model.connection = ConnectionState::Connecting;
                    model.logs.push_back(format!(
                        "[routing] Mode changed to {} — reconnecting",
                        mode.as_str()
                    ));
                }
                return effects;
            }
        }
        KeyCode::Char('q') | KeyCode::Esc => {
            model.overlay = Overlay::None;
        }
        _ => {}
    }
    vec![]
}

fn handle_input_mode(model: &mut Model, key: KeyEvent) -> Vec<Effect> {
    match key.code {
        KeyCode::Enter => {
            if model.overlay == Overlay::PasteUri {
                let text = model.input.buffer.trim().to_string();
                model.input.buffer.clear();
                model.overlay = Overlay::None;
                if !text.is_empty() {
                    return handle_clipboard_text(model, &text);
                }
            } else {
                advance_input_field(model);
            }
        }
        KeyCode::Esc => {
            model.overlay = Overlay::None;
            model.input.buffer.clear();
            model.input.field = InputField::None;
            model.input.draft = None;
        }
        KeyCode::Backspace => {
            model.input.buffer.pop();
        }
        KeyCode::Char(c) => {
            model.input.buffer.push(c);
        }
        _ => {}
    }
    vec![]
}

fn advance_input_field(model: &mut Model) {
    let draft = match model.input.draft.as_mut() {
        Some(d) => d,
        None => return,
    };

    if let Some(_current) = model.input.field.next() {
        let old = model.input.field;
        old.apply(draft, &model.input.buffer);

        if let Some(next) = old.next() {
            model.input.field = next;
            model.input.buffer = next.default_buffer(draft);
        } else {
            commit_profile(model);
        }
    } else {
        commit_profile(model);
    }
}

fn commit_profile(model: &mut Model) {
    if let Some(draft) = model.input.draft.as_mut() {
        model.input.field.apply(draft, &model.input.buffer);
    }

    if model.overlay == Overlay::CreateProfile {
        let profile = model.input.draft.take().unwrap();
        model.add_profile(profile);
        model.status = crate::app::model::AppStatus::Info("Profile created".into());
    }

    model.overlay = Overlay::None;
    model.input.field = InputField::None;
    model.input.buffer.clear();
}

fn handle_clipboard_text(model: &mut Model, text: &str) -> Vec<Effect> {
    match crate::infra::clipboard::parse_share_link(text) {
        Ok(profile) => {
            if model.has_duplicate(&profile) {
                model.status = crate::app::model::AppStatus::Error("Profile already exists".into());
                return vec![];
            }
            let name = profile.name.clone();
            model.add_profile(profile);
            model.status = crate::app::model::AppStatus::Info(format!("Pasted profile: {}", name));
            vec![Effect::SaveConfig]
        }
        Err(e) => {
            model.status = crate::app::model::AppStatus::Error(format!("Invalid URI: {}", e));
            vec![]
        }
    }
}

fn handle_geo_result(model: &mut Model, result: GeoResult) -> Vec<Effect> {
    model.geo_updating = false;
    match result {
        GeoResult::Updated(parts) => {
            for part in &parts {
                model.logs.push_back(format!("[geo] Updated: {}", part));
            }
            if model.connection == ConnectionState::Connected {
                model
                    .logs
                    .push_back("[geo] Reconnecting to apply new geo databases".into());
                model.connection = ConnectionState::Connecting;
            }
            vec![]
        }
        GeoResult::UpToDate => {
            model.status =
                crate::app::model::AppStatus::Info("Geo databases are up to date".into());
            vec![]
        }
        GeoResult::Error(err) => {
            model.status = crate::app::model::AppStatus::Error(format!("[geo] {}", err));
            vec![]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::*;
    use crossterm::event::KeyCode;

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
            Profile::new(
                "A".to_string(),
                Protocol::Vless,
                "1.1.1.1".to_string(),
                443,
                "u1".to_string(),
            ),
            Profile::new(
                "B".to_string(),
                Protocol::Vless,
                "2.2.2.2".to_string(),
                443,
                "u2".to_string(),
            ),
        ]);
        assert_eq!(model.selected, 0);
        let _ = handle_main(&mut model, key('j'));
        assert_eq!(model.selected, 1);
        let _ = handle_main(&mut model, key('k'));
        assert_eq!(model.selected, 0);
        let _ = handle_main(&mut model, key('G'));
        assert_eq!(model.selected, 1);
        let _ = handle_main(&mut model, key('g'));
        assert_eq!(model.selected, 0);
    }

    #[test]
    fn normal_mode_enter_connects() {
        let mut model = model_with_profiles(vec![Profile::new(
            "A".to_string(),
            Protocol::Vless,
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        let effects = handle_main(&mut model, KeyEvent::from(KeyCode::Enter));
        assert_eq!(model.connection, ConnectionState::Connecting);
        assert!(effects.is_empty());
    }

    #[test]
    fn normal_mode_enter_no_profile() {
        let mut model = model_with_profiles(vec![]);
        let effects = handle_main(&mut model, KeyEvent::from(KeyCode::Enter));
        assert_eq!(model.overlay, Overlay::None);
        assert!(effects.is_empty());
    }

    #[test]
    fn normal_mode_n_creates_profile() {
        let mut model = model_with_profiles(vec![]);
        let effects = handle_main(&mut model, key('n'));
        assert_eq!(model.overlay, Overlay::CreateProfile);
        assert_eq!(model.input.field, InputField::Name);
        assert!(model.input.draft.is_some());
        assert!(effects.is_empty());
    }

    #[test]
    fn normal_mode_d_confirms_delete() {
        let mut model = model_with_profiles(vec![Profile::new(
            "A".to_string(),
            Protocol::Vless,
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        let effects = handle_main(&mut model, key('d'));
        assert_eq!(model.overlay, Overlay::ConfirmDelete);
        assert!(effects.is_empty());
    }

    #[test]
    fn normal_mode_m_opens_routing() {
        let mut model = model_with_profiles(vec![]);
        model.config.settings.routing_mode = RoutingMode::BypassRu;
        let effects = handle_main(&mut model, key('m'));
        assert_eq!(model.overlay, Overlay::RoutingMode);
        assert_eq!(model.routing_selected, RoutingMode::BypassRu.index());
        assert!(effects.is_empty());
    }

    #[test]
    fn normal_mode_q_quits_when_no_process() {
        let mut model = model_with_profiles(vec![]);
        let effects = handle_main(&mut model, key('q'));
        assert_eq!(effects, vec![Effect::Quit]);
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
    fn error_mode_any_key_returns_to_normal() {
        let mut model = model_with_profiles(vec![]);
        model.overlay = Overlay::Error;
        let effects = handle_key(&mut model, key('x'));
        assert_eq!(model.overlay, Overlay::None);
        assert!(effects.is_empty());
    }

    #[test]
    fn confirm_delete_yes() {
        let mut model = model_with_profiles(vec![Profile::new(
            "A".to_string(),
            Protocol::Vless,
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        model.overlay = Overlay::ConfirmDelete;
        let effects = handle_confirm_delete(&mut model, key('y'));
        assert!(model.config.profiles.is_empty());
        assert_eq!(model.overlay, Overlay::None);
        assert_eq!(effects, vec![Effect::SaveConfig]);
    }

    #[test]
    fn confirm_delete_no() {
        let mut model = model_with_profiles(vec![Profile::new(
            "A".to_string(),
            Protocol::Vless,
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
    fn confirm_quit_yes() {
        let mut model = model_with_profiles(vec![]);
        model.overlay = Overlay::ConfirmQuit;
        let effects = handle_confirm_quit(&mut model, key('y'));
        assert_eq!(effects, vec![Effect::Quit]);
    }

    #[test]
    fn confirm_quit_no() {
        let mut model = model_with_profiles(vec![]);
        model.overlay = Overlay::ConfirmQuit;
        let effects = handle_confirm_quit(&mut model, key('n'));
        assert!(effects.is_empty());
        assert_eq!(model.overlay, Overlay::None);
    }

    #[test]
    fn routing_mode_navigates() {
        let mut model = model_with_profiles(vec![]);
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
        assert_eq!(model.routing_selected, 0);
        let _ = handle_routing_mode(&mut model, key('G'));
        assert_eq!(model.routing_selected, 2);
    }

    #[test]
    fn routing_mode_enter_changes_mode() {
        let mut model = model_with_profiles(vec![]);
        model.overlay = Overlay::RoutingMode;
        model.routing_selected = RoutingMode::OnlyRu.index();
        model.config.settings.routing_mode = RoutingMode::Global;

        let effects = handle_routing_mode(&mut model, KeyEvent::from(KeyCode::Enter));
        assert_eq!(model.config.settings.routing_mode, RoutingMode::OnlyRu);
        assert_eq!(model.overlay, Overlay::None);
        assert!(model.status.text().contains("Only RU"));
        assert_eq!(effects, vec![Effect::SaveConfig]);
    }

    #[test]
    fn routing_mode_esc_cancels() {
        let mut model = model_with_profiles(vec![]);
        model.overlay = Overlay::RoutingMode;
        model.routing_selected = 2;
        let effects = handle_routing_mode(&mut model, KeyEvent::from(KeyCode::Esc));
        assert_eq!(model.overlay, Overlay::None);
        assert!(effects.is_empty());
    }

    #[test]
    fn input_mode_advances_fields_and_creates_profile() {
        let mut model = model_with_profiles(vec![]);
        model.overlay = Overlay::CreateProfile;
        model.input.field = InputField::Name;
        model.input.draft = Some(Profile::new(
            String::new(),
            Protocol::Vless,
            String::new(),
            443,
            String::new(),
        ));

        // Name
        model.input.buffer = "MyProfile".to_string();
        advance_input_field(&mut model);
        assert_eq!(model.input.field, InputField::Address);
        assert_eq!(model.input.draft.as_ref().unwrap().name, "MyProfile");

        // Address
        model.input.buffer = "1.2.3.4".to_string();
        advance_input_field(&mut model);
        assert_eq!(model.input.field, InputField::Port);
        assert_eq!(model.input.draft.as_ref().unwrap().address, "1.2.3.4");

        // Port
        model.input.buffer = "8080".to_string();
        advance_input_field(&mut model);
        assert_eq!(model.input.field, InputField::Uuid);
        assert_eq!(model.input.draft.as_ref().unwrap().port, 8080);

        // UUID
        model.input.buffer = "uuid-here".to_string();
        advance_input_field(&mut model);
        assert_eq!(model.input.field, InputField::Protocol);
        assert_eq!(model.input.draft.as_ref().unwrap().uuid, "uuid-here");

        // Protocol -> commit
        model.input.buffer = "vless".to_string();
        advance_input_field(&mut model);
        assert_eq!(model.overlay, Overlay::None);
        assert_eq!(model.config.profiles.len(), 1);
        assert_eq!(model.config.profiles[0].name, "MyProfile");
        assert_eq!(model.config.profiles[0].protocol, Protocol::Vless);
    }

    #[test]
    fn input_mode_invalid_port_ignored() {
        let mut model = model_with_profiles(vec![]);
        model.overlay = Overlay::CreateProfile;
        model.input.field = InputField::Port;
        model.input.draft = Some(Profile::new(
            String::new(),
            Protocol::Vless,
            String::new(),
            443,
            String::new(),
        ));

        model.input.buffer = "not_a_port".to_string();
        advance_input_field(&mut model);
        assert_eq!(model.input.draft.as_ref().unwrap().port, 443); // unchanged
        assert_eq!(model.input.field, InputField::Uuid);
    }

    #[test]
    fn input_mode_esc_cancels() {
        let mut model = model_with_profiles(vec![]);
        model.overlay = Overlay::CreateProfile;
        model.input.field = InputField::Name;
        model.input.buffer = "text".to_string();
        model.input.draft = Some(Profile::new(
            String::new(),
            Protocol::Vless,
            String::new(),
            443,
            String::new(),
        ));

        let effects = handle_input_mode(&mut model, KeyEvent::from(KeyCode::Esc));
        assert_eq!(model.overlay, Overlay::None);
        assert!(model.input.buffer.is_empty());
        assert_eq!(model.input.field, InputField::None);
        assert!(model.input.draft.is_none());
        assert!(effects.is_empty());
    }

    #[test]
    fn input_mode_backspace_and_char() {
        let mut model = model_with_profiles(vec![]);
        model.overlay = Overlay::CreateProfile;
        model.input.field = InputField::Name;
        model.input.buffer.clear();

        let _ = handle_input_mode(&mut model, key('a'));
        let _ = handle_input_mode(&mut model, key('b'));
        assert_eq!(model.input.buffer, "ab");
        let _ = handle_input_mode(&mut model, KeyEvent::from(KeyCode::Backspace));
        assert_eq!(model.input.buffer, "a");
    }

    #[test]
    fn connected_mode_q_opens_confirm_quit() {
        let mut model = model_with_profiles(vec![]);
        model.connection = ConnectionState::Connected;
        model.overlay = Overlay::None;
        let effects = handle_key(&mut model, key('q'));
        assert!(effects.is_empty());
        assert_eq!(model.overlay, Overlay::ConfirmQuit);
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
            Profile::new(
                "A".to_string(),
                Protocol::Vless,
                "1.1.1.1".to_string(),
                443,
                "u1".to_string(),
            ),
            Profile::new(
                "B".to_string(),
                Protocol::Vless,
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
        assert_eq!(model.selected, 0);
    }

    #[test]
    fn connected_mode_enter_connects() {
        let mut model = model_with_profiles(vec![Profile::new(
            "A".to_string(),
            Protocol::Vless,
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        model.connection = ConnectionState::Connected;
        model.overlay = Overlay::None;
        let effects = handle_key(&mut model, KeyEvent::from(KeyCode::Enter));
        assert!(effects.is_empty());
        assert_eq!(model.connection, ConnectionState::Connecting);
    }

    #[test]
    fn connected_mode_r_reconnects() {
        let mut model = model_with_profiles(vec![Profile::new(
            "A".to_string(),
            Protocol::Vless,
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        model.connection = ConnectionState::Connected;
        model.overlay = Overlay::None;
        let effects = handle_key(&mut model, key('r'));
        assert!(effects.is_empty());
        assert_eq!(model.connection, ConnectionState::Connecting);
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
    fn connect_failed_sets_error_mode() {
        let mut model = Model::test_new(crate::config::profile::Config::default());
        let effects = update(&mut model, Msg::ConnectFailed("timeout".into()));
        assert_eq!(model.overlay, Overlay::Error);
        assert_eq!(model.connection, ConnectionState::Idle);
        assert!(effects.is_empty());
    }

    #[test]
    fn handle_tick_skips_connect_when_pending() {
        let mut model = model_with_profiles(vec![Profile::new(
            "A".to_string(),
            Protocol::Vless,
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
        let effects = update(&mut model, Msg::Connected { pid: 12345 });
        assert_eq!(model.connection, ConnectionState::Connected);
        assert_eq!(model.overlay, Overlay::None);
        assert_eq!(effects, vec![Effect::WriteState]);
    }

    #[test]
    fn connected_saves_last_profile() {
        let mut model = model_with_profiles(vec![Profile::new(
            "A".to_string(),
            Protocol::Vless,
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        model.connection = ConnectionState::ConnectPending;
        let effects = update(&mut model, Msg::Connected { pid: 12345 });
        assert_eq!(model.connection, ConnectionState::Connected);
        assert_eq!(
            model.config.settings.last_connected_profile,
            Some(model.config.profiles[0].id)
        );
        assert_eq!(effects, vec![Effect::WriteState, Effect::SaveConfig]);
    }

    #[test]
    fn toggle_auto_connect() {
        let mut model = model_with_profiles(vec![]);
        assert!(!model.config.settings.auto_connect);
        let effects = handle_main(&mut model, key('a'));
        assert!(model.config.settings.auto_connect);
        assert!(model.status.text().contains("enabled"));
        assert_eq!(effects, vec![Effect::SaveConfig]);

        let effects = handle_main(&mut model, key('a'));
        assert!(!model.config.settings.auto_connect);
        assert!(model.status.text().contains("disabled"));
        assert_eq!(effects, vec![Effect::SaveConfig]);
    }

    #[test]
    fn paste_duplicate_profile_shows_error() {
        let mut model = model_with_profiles(vec![]);
        let uri = "vless://671c62c7-6768-4b98-ac6b-572c9c707be0@203.0.113.42:443#Test";

        // First paste succeeds
        let effects = handle_clipboard_text(&mut model, uri);
        assert_eq!(model.config.profiles.len(), 1);
        assert_eq!(effects, vec![Effect::SaveConfig]);
        assert!(model.status.text().contains("Pasted profile"));

        // Second paste with same UUID fails
        let effects = handle_clipboard_text(&mut model, uri);
        assert_eq!(model.config.profiles.len(), 1);
        assert!(effects.is_empty());
        assert!(model.status.is_error());
        assert!(model.status.text().contains("already exists"));
    }

    #[test]
    fn log_line_truncates() {
        let mut model = Model::test_new(crate::config::profile::Config::default());
        for i in 0..1005 {
            let _ = update(&mut model, Msg::LogLine(format!("line {}", i)));
        }
        assert_eq!(model.logs.len(), 1000);
        assert_eq!(model.logs[0], "line 5");
        assert_eq!(model.logs[999], "line 1004");
        assert_eq!(model.log_scroll, 999);
    }
}
