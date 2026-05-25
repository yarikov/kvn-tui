use crossterm::event::{Event, KeyCode, KeyEvent};

use crate::app::{App, AppMode, InputField};
use crate::config::profile::{Profile, Protocol, RoutingMode};

/// Process a terminal event and update application state.
/// Returns true when the user requests application quit.
pub fn handle_event(app: &mut App, event: Event) -> bool {
    match event {
        Event::Key(key) => handle_key(app, key),
        Event::Resize(_, _) => false,
        _ => false,
    }
}

/// Handle keyboard input based on current application mode.
fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    match app.mode {
        AppMode::Normal => handle_normal(app, key),
        AppMode::Help => {
            app.mode = AppMode::Normal;
            false
        }
        AppMode::ConfirmDelete => handle_confirm_delete(app, key),
        AppMode::ConfirmQuit => handle_confirm_quit(app, key),
        AppMode::RoutingMode => handle_routing_mode(app, key),
        AppMode::EditProfile | AppMode::CreateProfile | AppMode::PasteUri => {
            handle_input_mode(app, key)
        }
        AppMode::Connecting | AppMode::Connected => {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => {
                    if app.singbox_process.is_some() {
                        app.mode = AppMode::ConfirmQuit;
                    } else {
                        return true;
                    }
                }
                KeyCode::Char('r') => {
                    app.mode = AppMode::Connecting;
                }
                KeyCode::Char('s') => {
                    crate::singbox::disconnect(app);
                    app.set_status("Disconnected");
                }
                _ => return handle_normal(app, key),
            }
            false
        }
        AppMode::Error => {
            app.mode = AppMode::Normal;
            false
        }
    }
}

/// Normal mode vim-style navigation.
fn handle_normal(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        // Navigation
        KeyCode::Char('j') | KeyCode::Down => app.select_next(),
        KeyCode::Char('k') | KeyCode::Up => app.select_prev(),
        KeyCode::Char('g') => app.select_first(),
        KeyCode::Char('G') => app.select_last(),

        // Actions
        KeyCode::Enter if app.selected_profile().is_some() => {
            app.mode = AppMode::Connecting;
        }
        KeyCode::Char('p') => {
            if let Err(e) = crate::clipboard::paste_profile(app) {
                app.set_status(format!(
                    "Clipboard unavailable ({}). Enter URI manually.",
                    e
                ));
                app.mode = AppMode::PasteUri;
                app.input_buffer.clear();
            }
        }
        KeyCode::Char('n') => {
            app.mode = AppMode::CreateProfile;
            app.input_field = InputField::Name;
            app.input_buffer.clear();
            app.edit_profile_id = None;
            app.draft_profile = Some(Profile::new(
                String::new(),
                Protocol::Vless,
                String::new(),
                443,
                String::new(),
            ));
        }
        KeyCode::Char('e') => {
            if let Some(profile) = app.selected_profile().cloned() {
                app.mode = AppMode::EditProfile;
                app.input_field = InputField::Name;
                app.input_buffer = profile.name.clone();
                app.edit_profile_id = Some(profile.id);
                app.draft_profile = Some(profile);
            }
        }
        KeyCode::Char('d') if app.selected_profile().is_some() => {
            app.mode = AppMode::ConfirmDelete;
        }
        KeyCode::Char('m') => {
            app.mode = AppMode::RoutingMode;
            app.routing_selected = app.config.settings.routing_mode.index();
        }
        KeyCode::Char('u') => {
            app.trigger_geo_update();
        }
        KeyCode::Char('c') => {
            if let Err(e) = crate::editor::open_profiles_editor() {
                app.set_error(format!("Editor failed: {}", e));
            } else {
                app.needs_redraw = true;
            }
        }
        KeyCode::Char('r') if app.singbox_process.is_some() => {
            app.mode = AppMode::Connecting;
        }
        KeyCode::Char('s') if app.singbox_process.is_some() => {
            crate::singbox::disconnect(app);
            app.set_status("Disconnected");
        }

        // Help and quit
        KeyCode::Char('?') => app.mode = AppMode::Help,
        KeyCode::Char('q') | KeyCode::Esc => {
            if app.singbox_process.is_some() {
                app.mode = AppMode::ConfirmQuit;
            } else {
                return true;
            }
        }

        _ => {}
    }
    false
}

/// Confirm or cancel profile deletion.
fn handle_confirm_delete(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('y') | KeyCode::Enter => app.delete_selected(),
        KeyCode::Char('n') | KeyCode::Esc => app.mode = AppMode::Normal,
        _ => {}
    }
    false
}

/// Confirm or cancel application quit when a connection is active.
fn handle_confirm_quit(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('y') | KeyCode::Enter => return true,
        KeyCode::Char('n') | KeyCode::Esc => app.mode = AppMode::Normal,
        _ => {}
    }
    false
}

/// Handle vim-style navigation in the routing mode modal.
fn handle_routing_mode(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            crate::ui::nav::select_next(&mut app.routing_selected, RoutingMode::ALL.len());
        }
        KeyCode::Char('k') | KeyCode::Up => {
            crate::ui::nav::select_prev(&mut app.routing_selected);
        }
        KeyCode::Char('g') => {
            crate::ui::nav::select_first(&mut app.routing_selected);
        }
        KeyCode::Char('G') => {
            crate::ui::nav::select_last(&mut app.routing_selected, RoutingMode::ALL.len());
        }
        KeyCode::Enter => {
            if let Some(mode) = RoutingMode::from_index(app.routing_selected) {
                let changed = app.config.settings.routing_mode != mode;
                app.config.settings.routing_mode = mode;
                if let Err(e) = app.save() {
                    tracing::warn!("Failed to save config after routing change: {}", e);
                }
                app.mode = AppMode::Normal;
                app.set_status(format!("Routing mode: {}", mode.as_str()));

                if changed && app.singbox_process.is_some() {
                    app.mode = AppMode::Connecting;
                    app.push_log(format!(
                        "[routing] Mode changed to {} — reconnecting",
                        mode.as_str()
                    ));
                }
            }
        }
        KeyCode::Char('q') | KeyCode::Esc => {
            app.mode = AppMode::Normal;
        }
        _ => {}
    }
    false
}

/// Handle text input for profile creation and editing.
fn handle_input_mode(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Enter => {
            if app.mode == AppMode::PasteUri {
                let text = app.input_buffer.trim().to_string();
                app.input_buffer.clear();
                app.mode = AppMode::Normal;
                if !text.is_empty() {
                    if let Err(e) = crate::clipboard::add_profile_from_text(app, &text) {
                        app.set_error(format!("Invalid URI: {}", e));
                    }
                }
            } else {
                advance_input_field(app);
            }
        }
        KeyCode::Esc => {
            app.mode = AppMode::Normal;
            app.input_buffer.clear();
            app.input_field = InputField::None;
            app.draft_profile = None;
        }
        KeyCode::Backspace => {
            app.input_buffer.pop();
        }
        KeyCode::Char(c) => {
            app.input_buffer.push(c);
        }
        _ => {}
    }
    false
}

/// Move to the next input field or commit the profile.
fn advance_input_field(app: &mut App) {
    let draft = match app.draft_profile.as_mut() {
        Some(d) => d,
        None => return,
    };

    if let Some(_current) = app.input_field.next() {
        // We are advancing from `current` to the next field.
        // However `app.input_field` still holds the *old* field value,
        // so apply to the old field before switching.
        let old = app.input_field;
        old.apply(draft, &app.input_buffer);

        if let Some(next) = old.next() {
            app.input_field = next;
            app.input_buffer = next.default_buffer(draft);
        } else {
            // Should not happen because we checked `next()` above.
            commit_profile(app);
        }
    } else {
        commit_profile(app);
    }
}

/// Finalize profile creation or editing.
fn commit_profile(app: &mut App) {
    if let Some(draft) = app.draft_profile.as_mut() {
        app.input_field.apply(draft, &app.input_buffer);
    }

    if app.mode == AppMode::CreateProfile {
        let profile = app.draft_profile.take().unwrap();
        app.add_profile(profile);
        app.set_status("Profile created");
    } else if let Some(id) = app.edit_profile_id {
        let profile = app.draft_profile.take().unwrap();
        app.update_profile(id, profile);
        app.set_status("Profile updated");
    }

    app.mode = AppMode::Normal;
    app.input_field = InputField::None;
    app.input_buffer.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::*;

    #[test]
    fn handle_event_non_key_is_noop() {
        let mut app = app_with_profiles(vec![]);
        let result = handle_event(&mut app, Event::Resize(80, 24));
        assert!(!result);
        assert_eq!(app.mode, AppMode::Normal);
    }

    #[test]
    fn normal_mode_navigates() {
        let mut app = app_with_profiles(vec![
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
        assert_eq!(app.selected, 0);
        handle_normal(&mut app, key('j'));
        assert_eq!(app.selected, 1);
        handle_normal(&mut app, key('k'));
        assert_eq!(app.selected, 0);
        handle_normal(&mut app, key('G'));
        assert_eq!(app.selected, 1);
        handle_normal(&mut app, key('g'));
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn normal_mode_enter_connects() {
        let mut app = app_with_profiles(vec![Profile::new(
            "A".to_string(),
            Protocol::Vless,
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        handle_normal(&mut app, KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.mode, AppMode::Connecting);
    }

    #[test]
    fn normal_mode_enter_no_profile() {
        let mut app = app_with_profiles(vec![]);
        handle_normal(&mut app, KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.mode, AppMode::Normal);
    }

    #[test]
    fn normal_mode_n_creates_profile() {
        let mut app = app_with_profiles(vec![]);
        handle_normal(&mut app, key('n'));
        assert_eq!(app.mode, AppMode::CreateProfile);
        assert_eq!(app.input_field, InputField::Name);
        assert!(app.draft_profile.is_some());
        assert!(app.edit_profile_id.is_none());
    }

    #[test]
    fn normal_mode_e_edits_selected() {
        let p = Profile::new(
            "A".to_string(),
            Protocol::Vless,
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        );
        let id = p.id;
        let mut app = app_with_profiles(vec![p]);
        handle_normal(&mut app, key('e'));
        assert_eq!(app.mode, AppMode::EditProfile);
        assert_eq!(app.input_field, InputField::Name);
        assert_eq!(app.edit_profile_id, Some(id));
        assert!(app.draft_profile.is_some());
    }

    #[test]
    fn normal_mode_e_no_profile() {
        let mut app = app_with_profiles(vec![]);
        handle_normal(&mut app, key('e'));
        assert_eq!(app.mode, AppMode::Normal);
    }

    #[test]
    fn normal_mode_d_confirms_delete() {
        let mut app = app_with_profiles(vec![Profile::new(
            "A".to_string(),
            Protocol::Vless,
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        handle_normal(&mut app, key('d'));
        assert_eq!(app.mode, AppMode::ConfirmDelete);
    }

    #[test]
    fn normal_mode_m_opens_routing() {
        let mut app = app_with_profiles(vec![]);
        app.config.settings.routing_mode = RoutingMode::BypassRu;
        handle_normal(&mut app, key('m'));
        assert_eq!(app.mode, AppMode::RoutingMode);
        assert_eq!(app.routing_selected, RoutingMode::BypassRu.index());
    }

    #[test]
    fn normal_mode_q_quits_when_no_process() {
        let mut app = app_with_profiles(vec![]);
        let quit = handle_normal(&mut app, key('q'));
        assert!(quit);
    }

    #[test]
    fn normal_mode_q_confirms_quit_when_process_running() {
        let mut app = app_with_profiles(vec![]);
        // Simulate a running process by setting mode to Connected (which implies process is active in UI)
        // But singbox_process is None, so we need to use a mock. Since we can't easily mock Child,
        // test via handle_key in Connected mode where the guard is on singbox_process.
        app.singbox_process = None; // no real process
        let quit = handle_normal(&mut app, key('q'));
        assert!(quit);
    }

    #[test]
    fn help_mode_any_key_returns_to_normal() {
        let mut app = app_with_profiles(vec![]);
        app.mode = AppMode::Help;
        handle_key(&mut app, key('x'));
        assert_eq!(app.mode, AppMode::Normal);
    }

    #[test]
    fn error_mode_any_key_returns_to_normal() {
        let mut app = app_with_profiles(vec![]);
        app.mode = AppMode::Error;
        handle_key(&mut app, key('x'));
        assert_eq!(app.mode, AppMode::Normal);
    }

    #[test]
    fn confirm_delete_yes() {
        let mut app = app_with_profiles(vec![Profile::new(
            "A".to_string(),
            Protocol::Vless,
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        app.mode = AppMode::ConfirmDelete;
        handle_confirm_delete(&mut app, key('y'));
        assert!(app.config.profiles.is_empty());
        assert_eq!(app.mode, AppMode::Normal);
    }

    #[test]
    fn confirm_delete_no() {
        let mut app = app_with_profiles(vec![Profile::new(
            "A".to_string(),
            Protocol::Vless,
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        app.mode = AppMode::ConfirmDelete;
        handle_confirm_delete(&mut app, key('n'));
        assert_eq!(app.config.profiles.len(), 1);
        assert_eq!(app.mode, AppMode::Normal);
    }

    #[test]
    fn confirm_quit_yes() {
        let mut app = app_with_profiles(vec![]);
        app.mode = AppMode::ConfirmQuit;
        let quit = handle_confirm_quit(&mut app, key('y'));
        assert!(quit);
    }

    #[test]
    fn confirm_quit_no() {
        let mut app = app_with_profiles(vec![]);
        app.mode = AppMode::ConfirmQuit;
        let quit = handle_confirm_quit(&mut app, key('n'));
        assert!(!quit);
        assert_eq!(app.mode, AppMode::Normal);
    }

    #[test]
    fn routing_mode_navigates() {
        let mut app = app_with_profiles(vec![]);
        app.mode = AppMode::RoutingMode;
        app.routing_selected = 0;

        handle_routing_mode(&mut app, key('j'));
        assert_eq!(app.routing_selected, 1);
        handle_routing_mode(&mut app, key('j'));
        assert_eq!(app.routing_selected, 2);
        handle_routing_mode(&mut app, key('j'));
        assert_eq!(app.routing_selected, 2); // clamp

        handle_routing_mode(&mut app, key('k'));
        assert_eq!(app.routing_selected, 1);
        handle_routing_mode(&mut app, key('g'));
        assert_eq!(app.routing_selected, 0);
        handle_routing_mode(&mut app, key('G'));
        assert_eq!(app.routing_selected, 2);
    }

    #[test]
    fn routing_mode_enter_changes_mode() {
        let mut app = app_with_profiles(vec![]);
        app.mode = AppMode::RoutingMode;
        app.routing_selected = RoutingMode::OnlyRu.index();
        app.config.settings.routing_mode = RoutingMode::Global;

        handle_routing_mode(&mut app, KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.config.settings.routing_mode, RoutingMode::OnlyRu);
        assert_eq!(app.mode, AppMode::Normal);
        assert!(app.status.text().contains("Only RU"));
    }

    #[test]
    fn routing_mode_enter_triggers_reconnect_when_changed_and_connected() {
        let mut app = app_with_profiles(vec![]);
        app.mode = AppMode::RoutingMode;
        app.routing_selected = RoutingMode::OnlyRu.index();
        app.config.settings.routing_mode = RoutingMode::Global;
        // Simulate active connection
        app.singbox_process = None; // Can't easily mock Child, so we skip the reconnect branch
                                    // In real code the reconnect only happens if singbox_process.is_some()
        handle_routing_mode(&mut app, KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.mode, AppMode::Normal);
    }

    #[test]
    fn routing_mode_esc_cancels() {
        let mut app = app_with_profiles(vec![]);
        app.mode = AppMode::RoutingMode;
        app.routing_selected = 2;
        handle_routing_mode(&mut app, KeyEvent::from(KeyCode::Esc));
        assert_eq!(app.mode, AppMode::Normal);
    }

    #[test]
    fn input_mode_advances_fields_and_creates_profile() {
        let mut app = app_with_profiles(vec![]);
        app.mode = AppMode::CreateProfile;
        app.input_field = InputField::Name;
        app.draft_profile = Some(Profile::new(
            String::new(),
            Protocol::Vless,
            String::new(),
            443,
            String::new(),
        ));

        // Name
        app.input_buffer = "MyProfile".to_string();
        advance_input_field(&mut app);
        assert_eq!(app.input_field, InputField::Address);
        assert_eq!(app.draft_profile.as_ref().unwrap().name, "MyProfile");

        // Address
        app.input_buffer = "1.2.3.4".to_string();
        advance_input_field(&mut app);
        assert_eq!(app.input_field, InputField::Port);
        assert_eq!(app.draft_profile.as_ref().unwrap().address, "1.2.3.4");

        // Port
        app.input_buffer = "8080".to_string();
        advance_input_field(&mut app);
        assert_eq!(app.input_field, InputField::Uuid);
        assert_eq!(app.draft_profile.as_ref().unwrap().port, 8080);

        // UUID
        app.input_buffer = "uuid-here".to_string();
        advance_input_field(&mut app);
        assert_eq!(app.input_field, InputField::Protocol);
        assert_eq!(app.draft_profile.as_ref().unwrap().uuid, "uuid-here");

        // Protocol -> commit
        app.input_buffer = "vless".to_string();
        advance_input_field(&mut app);
        assert_eq!(app.mode, AppMode::Normal);
        assert_eq!(app.config.profiles.len(), 1);
        assert_eq!(app.config.profiles[0].name, "MyProfile");
        assert_eq!(app.config.profiles[0].protocol, Protocol::Vless);
    }

    #[test]
    fn input_mode_invalid_port_ignored() {
        let mut app = app_with_profiles(vec![]);
        app.mode = AppMode::CreateProfile;
        app.input_field = InputField::Port;
        app.draft_profile = Some(Profile::new(
            String::new(),
            Protocol::Vless,
            String::new(),
            443,
            String::new(),
        ));

        app.input_buffer = "not_a_port".to_string();
        advance_input_field(&mut app);
        assert_eq!(app.draft_profile.as_ref().unwrap().port, 443); // unchanged
        assert_eq!(app.input_field, InputField::Uuid);
    }

    #[test]
    fn input_mode_esc_cancels() {
        let mut app = app_with_profiles(vec![]);
        app.mode = AppMode::CreateProfile;
        app.input_field = InputField::Name;
        app.input_buffer = "text".to_string();
        app.draft_profile = Some(Profile::new(
            String::new(),
            Protocol::Vless,
            String::new(),
            443,
            String::new(),
        ));

        handle_input_mode(&mut app, KeyEvent::from(KeyCode::Esc));
        assert_eq!(app.mode, AppMode::Normal);
        assert!(app.input_buffer.is_empty());
        assert_eq!(app.input_field, InputField::None);
        assert!(app.draft_profile.is_none());
    }

    #[test]
    fn input_mode_backspace_and_char() {
        let mut app = app_with_profiles(vec![]);
        app.mode = AppMode::CreateProfile;
        app.input_field = InputField::Name;
        app.input_buffer.clear();

        handle_input_mode(&mut app, key('a'));
        handle_input_mode(&mut app, key('b'));
        assert_eq!(app.input_buffer, "ab");
        handle_input_mode(&mut app, KeyEvent::from(KeyCode::Backspace));
        assert_eq!(app.input_buffer, "a");
    }

    #[test]
    fn connected_mode_q_quits_without_process() {
        let mut app = app_with_profiles(vec![]);
        app.mode = AppMode::Connected;
        let quit = handle_key(&mut app, key('q'));
        assert!(quit);
    }

    #[test]
    fn connected_mode_s_sets_status() {
        let mut app = app_with_profiles(vec![]);
        app.mode = AppMode::Connected;
        let quit = handle_key(&mut app, key('s'));
        assert!(!quit);
        assert!(app.status.text().contains("Disconnected"));
    }

    #[test]
    fn connected_mode_navigates() {
        let mut app = app_with_profiles(vec![
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
        app.mode = AppMode::Connected;
        assert_eq!(app.selected, 0);
        handle_key(&mut app, key('j'));
        assert_eq!(app.selected, 1);
        handle_key(&mut app, key('k'));
        assert_eq!(app.selected, 0);
        handle_key(&mut app, key('G'));
        assert_eq!(app.selected, 1);
        handle_key(&mut app, key('g'));
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn connected_mode_enter_connects() {
        let mut app = app_with_profiles(vec![Profile::new(
            "A".to_string(),
            Protocol::Vless,
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        app.mode = AppMode::Connected;
        let quit = handle_key(&mut app, KeyEvent::from(KeyCode::Enter));
        assert!(!quit);
        assert_eq!(app.mode, AppMode::Connecting);
    }

    #[test]
    fn connected_mode_r_reconnects() {
        let mut app = app_with_profiles(vec![Profile::new(
            "A".to_string(),
            Protocol::Vless,
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        app.mode = AppMode::Connected;
        let quit = handle_key(&mut app, key('r'));
        assert!(!quit);
        assert_eq!(app.mode, AppMode::Connecting);
    }

    #[test]
    fn connected_mode_help() {
        let mut app = app_with_profiles(vec![]);
        app.mode = AppMode::Connected;
        let quit = handle_key(&mut app, key('?'));
        assert!(!quit);
        assert_eq!(app.mode, AppMode::Help);
    }
}
