use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;

use crate::config::profile::{Config, Profile, Protocol};
use crate::app::model::Model;

/// Convert a ratatui Buffer to a multi-line string for snapshot testing.
pub fn buffer_to_string(buffer: &Buffer) -> String {
    buffer
        .content
        .chunks(buffer.area.width as usize)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Ensure geo metadata exists with a fixed date so that StatusBar snapshots are deterministic.
pub fn ensure_fixed_geo() {
    crate::geo::set_test_last_updated(Some("2026-05-31 13:41".to_string()));
}

/// Generate a small set of sample profiles for unit tests.
pub fn sample_profiles() -> Vec<Profile> {
    vec![
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
        Profile::new(
            "C".to_string(),
            Protocol::Vless,
            "3.3.3.3".to_string(),
            443,
            "u3".to_string(),
        ),
    ]
}

/// Build a `Model` pre-filled with the given profiles for testing.
pub fn model_with_profiles(profiles: Vec<Profile>) -> Model {
    let config = Config {
        profiles,
        ..Default::default()
    };
    Model::test_new(config)
}

/// Create a simple `KeyEvent` from a character for testing input handlers.
pub fn key(c: char) -> KeyEvent {
    KeyEvent::from(KeyCode::Char(c))
}
