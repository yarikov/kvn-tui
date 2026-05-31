use crossterm::event::{KeyCode, KeyEvent};

use crate::config::profile::{Config, Profile, Protocol};
use crate::model::Model;

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
    let mut config = Config::default();
    config.profiles = profiles;
    Model::test_new(config)
}

/// Create a simple `KeyEvent` from a character for testing input handlers.
pub fn key(c: char) -> KeyEvent {
    KeyEvent::from(KeyCode::Char(c))
}
