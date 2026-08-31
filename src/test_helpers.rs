use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use std::convert::Infallible;
use std::ffi::{OsStr, OsString};
use std::sync::{Mutex, MutexGuard};

use crate::app::model::Model;
use crate::config::profile::{Config, Profile};

/// Mutex that recovers after a test panics while holding the lock.
///
/// A failed environment-sensitive test must not turn every later test into a
/// `PoisonError`. Returning an infallible `Result` preserves the familiar
/// `ENV_LOCK.lock().unwrap()` call sites while making them poison-tolerant.
pub struct RecoverableMutex(Mutex<()>);

impl RecoverableMutex {
    pub const fn new() -> Self {
        Self(Mutex::new(()))
    }

    pub fn lock(&self) -> Result<MutexGuard<'_, ()>, Infallible> {
        Ok(self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()))
    }
}

/// Global lock for tests that mutate environment variables.
/// Prevents race conditions when running tests in parallel.
pub static ENV_LOCK: RecoverableMutex = RecoverableMutex::new();

/// Restores an environment variable when an environment-sensitive test ends,
/// including during panic unwinding.
pub struct EnvVarGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvVarGuard {
    pub fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let previous = std::env::var_os(key);
        unsafe { std::env::set_var(key, value) };
        Self { key, previous }
    }

    pub fn remove(key: &'static str) -> Self {
        let previous = std::env::var_os(key);
        unsafe { std::env::remove_var(key) };
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        unsafe {
            match self.previous.take() {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

/// Convert a ratatui Buffer to a multi-line string for snapshot testing.
pub fn buffer_to_string(buffer: &Buffer) -> String {
    buffer
        .content
        .chunks(buffer.area.width as usize)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// A canonical valid UUID string for tests that need to pass
/// [`Config::validate`] without caring about the specific value.
pub const TEST_UUID: &str = "11111111-1111-1111-1111-111111111111";

/// Generate a small set of sample profiles for unit tests.
pub fn sample_profiles() -> Vec<Profile> {
    vec![
        Profile::new_vless(
            "A".to_string(),
            "1.1.1.1".to_string(),
            443,
            TEST_UUID.to_string(),
        ),
        Profile::new_vless(
            "B".to_string(),
            "2.2.2.2".to_string(),
            443,
            TEST_UUID.to_string(),
        ),
        Profile::new_vless(
            "C".to_string(),
            "3.3.3.3".to_string(),
            443,
            TEST_UUID.to_string(),
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
