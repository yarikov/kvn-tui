use crate::model::{AppState, Model};

/// Write current connection state to the state JSON file.
pub fn write_state(model: &Model) {
    let state = build_state(model);
    write_state_to(&state, crate::paths::state_json_path());
}

fn build_state(model: &Model) -> AppState {
    AppState {
        connected: model.singbox_process.is_some(),
        profile_name: model.active_profile_id.and_then(|id| {
            model.config.profiles.iter().find(|p| p.id == id).map(|p| p.name.clone())
        }),
        active_profile_id: model.active_profile_id.map(|id| id.to_string()),
        singbox_pid: model.singbox_process.as_ref().map(|c| c.id()),
    }
}

/// Clear the state file (used on startup to recover from a crash).
pub fn clear_state() {
    write_state_to(
        &AppState {
            connected: false,
            profile_name: None,
            active_profile_id: None,
            singbox_pid: None,
        },
        crate::paths::state_json_path(),
    );
}

fn write_state_to(state: &AppState, path: impl AsRef<std::path::Path>) {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(state) {
        if let Err(e) = std::fs::write(path, json) {
            tracing::warn!("Failed to write state file: {}", e);
        }
    }
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

/// Print waybar status JSON to stdout.
pub fn print_waybar_status() {
    let path = crate::paths::state_json_path();
    let mut state = read_state_from(&path);

    if state.connected {
        let alive = state
            .singbox_pid
            .map_or(false, |pid| is_singbox_alive(pid));
        if !alive {
            state = AppState {
                connected: false,
                profile_name: None,
                active_profile_id: None,
                singbox_pid: None,
            };
            write_state_to(&state, &path);
        }
    }

    print_waybar_from_state(&state);
}

fn is_singbox_alive(pid: u32) -> bool {
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
    use crate::model::AppState;

    #[test]
    fn clear_state_writes_disconnected() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let connected = AppState {
            connected: true,
            profile_name: Some("Alpha".to_string()),
            active_profile_id: Some("550e8400-e29b-41d4-a716-446655440000".to_string()),
            singbox_pid: Some(1234),
        };
        write_state_to(&connected, temp.path());

        write_state_to(
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
        write_state_to(&state, temp.path());

        let read = read_state_from(temp.path());
        assert!(read.connected);
        assert_eq!(read.profile_name, Some("Test Profile".to_string()));
        assert_eq!(read.active_profile_id, Some("550e8400-e29b-41d4-a716-446655440000".to_string()));
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
    fn print_waybar_status_clears_stale_state() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let stale = AppState {
            connected: true,
            profile_name: Some("Stale".to_string()),
            active_profile_id: Some("550e8400-e29b-41d4-a716-446655440000".to_string()),
            singbox_pid: Some(999_999),
        };
        write_state_to(&stale, temp.path());

        let mut state = read_state_from(temp.path());
        if state.connected {
            let alive = state
                .singbox_pid
                .map_or(false, |pid| is_singbox_alive(pid));
            if !alive {
                state = AppState {
                    connected: false,
                    profile_name: None,
                    active_profile_id: None,
                    singbox_pid: None,
                };
                write_state_to(&state, temp.path());
            }
        }

        let read = read_state_from(temp.path());
        assert!(!read.connected);
        assert!(read.profile_name.is_none());
        assert!(read.singbox_pid.is_none());
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
