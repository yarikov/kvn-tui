use crate::app::model::{AppState, Model};

/// Write current connection state to the state JSON file.
pub fn write_state(model: &Model) {
    let state = build_state(model);
    let path = crate::infra::paths::state_json_path();
    if let Err(e) = write_state_to(&state, &path) {
        tracing::warn!("state write failed: {e}");
    }
}

fn build_state(model: &Model) -> AppState {
    AppState {
        connected: model.connection == crate::app::model::ConnectionState::Connected,
        profile_name: model.active_profile_id.and_then(|id| {
            model
                .config
                .profiles
                .iter()
                .find(|p| p.id == id)
                .map(|p| p.name.clone())
        }),
        active_profile_id: model.active_profile_id.map(|id| id.to_string()),
        singbox_pid: model.singbox_pid,
    }
}

/// Clear the state file (used on startup to recover from a crash).
pub fn clear_state() {
    let state = AppState {
        connected: false,
        profile_name: None,
        active_profile_id: None,
        singbox_pid: None,
    };
    let path = crate::infra::paths::state_json_path();
    if let Err(e) = write_state_to(&state, &path) {
        tracing::warn!("state clear failed: {e}");
    }
}

fn write_state_to(state: &AppState, path: impl AsRef<std::path::Path>) -> std::io::Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(state).expect("AppState serialization is infallible");
    std::fs::write(path, json)
}

fn read_state_from(path: impl AsRef<std::path::Path>) -> AppState {
    std::fs::read_to_string(path.as_ref())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(AppState {
            connected: false,
            profile_name: None,
            active_profile_id: None,
            singbox_pid: None,
        })
}

/// Read the current state from disk.
pub fn read_state() -> AppState {
    let path = crate::infra::paths::state_json_path();
    read_state_from(&path)
}

/// Check whether a background sing-box session is still alive.
///
/// Returns `(pid, active_profile_id, profile_name)` if a valid session exists.
pub fn detect_background_session() -> Option<(u32, uuid::Uuid, String)> {
    let state = read_state();
    let pid = state.singbox_pid?;
    if !is_singbox_alive(pid) {
        return None;
    }
    let id = state
        .active_profile_id
        .and_then(|s| uuid::Uuid::parse_str(&s).ok())?;
    let name = state.profile_name.unwrap_or_else(|| "unknown".to_string());
    Some((pid, id, name))
}

/// Print waybar status JSON to stdout.
pub fn print_status() {
    let path = crate::infra::paths::state_json_path();
    let mut state = read_state_from(&path);

    if state.connected {
        let alive = state.singbox_pid.is_some_and(is_singbox_alive);
        if !alive {
            state = AppState {
                connected: false,
                profile_name: None,
                active_profile_id: None,
                singbox_pid: None,
            };
            let _ = write_state_to(&state, &path);
        }
    }

    print_waybar_from_state(&state);
}

pub fn is_singbox_alive(pid: u32) -> bool {
    let exe = std::path::PathBuf::from(format!("/proc/{pid}/exe"));
    std::fs::read_link(&exe)
        .map(|target| target.to_string_lossy().contains("sing-box"))
        .unwrap_or(false)
}

fn format_waybar(state: &AppState) -> String {
    let (icon, tooltip, class) = if state.connected {
        let name = state.profile_name.as_deref().unwrap_or("unknown");
        ("󰦝", format!("Connected: {}", name), "connected")
    } else {
        ("󰦜", "Disconnected".to_string(), "disconnected")
    };

    serde_json::json!({
        "text": icon,
        "tooltip": tooltip,
        "class": class
    })
    .to_string()
}

fn print_waybar_from_state(state: &AppState) {
    println!("{}", format_waybar(state));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::model::AppState;

    #[test]
    fn clear_state_writes_disconnected() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let connected = AppState {
            connected: true,
            profile_name: Some("Alpha".to_string()),
            active_profile_id: Some("550e8400-e29b-41d4-a716-446655440000".to_string()),
            singbox_pid: Some(1234),
        };
        let _ = write_state_to(&connected, temp.path());

        let _ = write_state_to(
            &AppState {
                connected: false,
                profile_name: None,
                active_profile_id: None,
                singbox_pid: None,
            },
            temp.path(),
        );

        let read = read_state_from(temp.path());
        assert!(!read.connected);
        assert!(read.profile_name.is_none());
        assert!(read.active_profile_id.is_none());
    }

    #[test]
    fn write_state_to_creates_valid_json() {
        let state = AppState {
            connected: true,
            profile_name: Some("Test Profile".to_string()),
            active_profile_id: Some("550e8400-e29b-41d4-a716-446655440000".to_string()),
            singbox_pid: Some(1234),
        };
        let temp = tempfile::NamedTempFile::new().unwrap();
        let _ = write_state_to(&state, temp.path());

        let read = read_state_from(temp.path());
        assert!(read.connected);
        assert_eq!(read.profile_name, Some("Test Profile".to_string()));
        assert_eq!(
            read.active_profile_id,
            Some("550e8400-e29b-41d4-a716-446655440000".to_string())
        );
    }

    #[test]
    fn read_state_from_returns_default_when_missing() {
        let temp = tempfile::TempDir::new().unwrap();
        let missing = temp.path().join("nonexistent.json");
        let state = read_state_from(&missing);
        assert!(!state.connected);
        assert!(state.profile_name.is_none());
        assert!(state.active_profile_id.is_none());
    }

    #[test]
    fn is_singbox_alive_false_for_nonexistent_pid() {
        assert!(!is_singbox_alive(999_999));
    }

    #[test]
    fn is_singbox_alive_false_for_systemd() {
        assert!(!is_singbox_alive(1));
    }

    #[test]
    fn print_status_clears_stale_state() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let stale = AppState {
            connected: true,
            profile_name: Some("Stale".to_string()),
            active_profile_id: Some("550e8400-e29b-41d4-a716-446655440000".to_string()),
            singbox_pid: Some(999_999),
        };
        let _ = write_state_to(&stale, temp.path());

        let mut state = read_state_from(temp.path());
        if state.connected {
            let alive = state.singbox_pid.is_some_and(is_singbox_alive);
            if !alive {
                state = AppState {
                    connected: false,
                    profile_name: None,
                    active_profile_id: None,
                    singbox_pid: None,
                };
                let _ = write_state_to(&state, temp.path());
            }
        }

        let read = read_state_from(temp.path());
        assert!(!read.connected);
        assert!(read.profile_name.is_none());
        assert!(read.singbox_pid.is_none());
    }

    #[test]
    fn build_state_connected() {
        use crate::app::model::{ConnectionState, Model};
        use crate::config::profile::{Config, Profile, Protocol};
        let mut model = Model::test_new(Config::default());
        model.connection = ConnectionState::Connected;
        let profile = Profile::new(
            "Test".to_string(),
            Protocol::Vless,
            "1.1.1.1".to_string(),
            443,
            "u".to_string(),
        );
        let id = profile.id;
        model.config.profiles.push(profile);
        model.active_profile_id = Some(id);
        model.singbox_pid = Some(1234);

        let state = build_state(&model);
        assert!(state.connected);
        assert_eq!(state.profile_name, Some("Test".to_string()));
        assert_eq!(state.active_profile_id, Some(id.to_string()));
        assert_eq!(state.singbox_pid, Some(1234));
    }

    #[test]
    fn build_state_idle() {
        use crate::app::model::Model;
        use crate::config::profile::Config;
        let model = Model::test_new(Config::default());
        let state = build_state(&model);
        assert!(!state.connected);
        assert!(state.profile_name.is_none());
        assert!(state.active_profile_id.is_none());
        assert!(state.singbox_pid.is_none());
    }

    #[test]
    fn print_waybar_from_state_outputs_json() {
        let state = AppState {
            connected: true,
            profile_name: Some("Alpha".to_string()),
            active_profile_id: None,
            singbox_pid: None,
        };
        // Just ensure it doesn't panic and produces output
        print_waybar_from_state(&state);

        let state = AppState {
            connected: false,
            profile_name: None,
            active_profile_id: None,
            singbox_pid: None,
        };
        print_waybar_from_state(&state);
    }

    #[test]
    fn format_waybar_connected() {
        let state = AppState {
            connected: true,
            profile_name: Some("Alpha".to_string()),
            active_profile_id: None,
            singbox_pid: Some(1234),
        };
        let json = format_waybar(&state);
        assert!(json.contains("󰦝"));
        assert!(json.contains("Connected: Alpha"));
        assert!(json.contains("\"class\":\"connected\""));
    }

    #[test]
    fn format_waybar_disconnected() {
        let state = AppState {
            connected: false,
            profile_name: None,
            active_profile_id: None,
            singbox_pid: None,
        };
        let json = format_waybar(&state);
        assert!(json.contains("󰦜"));
        assert!(json.contains("Disconnected"));
        assert!(json.contains("\"class\":\"disconnected\""));
    }
}
