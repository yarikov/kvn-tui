pub mod runner;

use crate::model::Model;

/// Stop the running sing-box process and reset state.
pub fn disconnect(model: &mut Model) {
    if let Some(mut child) = model.singbox_process.take() {
        if let Err(e) = runner::stop(&mut child) {
            tracing::warn!("Failed to stop sing-box process: {}", e);
        }
    }
}
