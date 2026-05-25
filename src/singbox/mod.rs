pub mod runner;

use anyhow::Result;

use crate::app::{App, AppMode};
use crate::config::profile::Profile;

/// Start sing-box for the given profile and update app state.
pub fn connect(app: &mut App, profile: &Profile) -> Result<()> {
    // Stop any existing process first.
    disconnect(app);

    let child = runner::start(profile, &app.config.settings)?;
    app.singbox_process = Some(child);
    app.status = crate::app::AppStatus::Info(format!("Connected to {}", profile.name));
    app.mode = AppMode::Connected;
    app.push_log(format!("sing-box started for profile: {}", profile.name));

    // Update log path so the UI can tail sing-box logs.
    app.set_singbox_log_path(crate::paths::singbox_log_path());

    Ok(())
}

/// Stop the running sing-box process and reset state.
pub fn disconnect(app: &mut App) {
    if let Some(mut child) = app.singbox_process.take() {
        if let Err(e) = runner::stop(&mut child) {
            tracing::warn!("Failed to stop sing-box process: {}", e);
        }
        app.push_log("sing-box stopped".to_string());
    }
    app.mode = AppMode::Normal;
    app.connecting = false;
}
