use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::Result;

use crate::app::effect::Effect;
use crate::app::model::{AppStatus, ConnectionState, Model, Overlay};
use crate::app::msg::{GeoResult, Msg, StateSnapshot};
use crate::app::update::update;
use crate::infra::process_handle::ProcessHandle;
use crate::ipc::{IpcServer, cleanup_socket};
use crate::services::LogTailer;

/// Run the daemon main loop.
pub fn run(mut model: Model) -> Result<()> {
    let (tx, rx) = channel::<Msg>();
    let ipc_server = IpcServer::bind(tx.clone())?;

    spawn_ticker(tx.clone());
    spawn_suspend_watcher(tx.clone());

    let mut log_tailer = LogTailer::new(crate::infra::paths::singbox_log_path());

    let process_slot = Arc::new(Mutex::new(None));

    let result = run_loop(
        &mut model,
        rx,
        &tx,
        &mut log_tailer,
        process_slot.clone(),
        &ipc_server,
    );

    // Cleanup
    if let Some(mut handle) = process_slot.lock().unwrap().take() {
        if let Err(e) = handle.kill_and_wait() {
            tracing::warn!("Failed to stop sing-box on exit: {}", e);
        }
    }
    cleanup_socket();

    result
}

fn run_loop(
    model: &mut Model,
    rx: std::sync::mpsc::Receiver<Msg>,
    tx: &Sender<Msg>,
    log_tailer: &mut LogTailer,
    process_slot: Arc<Mutex<Option<ProcessHandle>>>,
    ipc_server: &IpcServer,
) -> Result<()> {
    loop {
        let msg = rx.recv()?;
        let effects = update(model, msg);
        let mut should_broadcast = false;

        for effect in &effects {
            if matches!(
                effect,
                Effect::Connect { .. }
                    | Effect::Disconnect
                    | Effect::DownloadGeo
                    | Effect::WriteState
                    | Effect::SaveConfig
                    | Effect::PasteClipboard
                    | Effect::BroadcastState
            ) {
                should_broadcast = true;
            }
        }

        for effect in effects {
            execute_daemon_effect(effect, tx, model, log_tailer, &process_slot)?;
        }

        if model.should_quit {
            break;
        }

        if should_broadcast {
            ipc_server.broadcast(&build_snapshot(model));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn execute_daemon_effect(
    effect: Effect,
    tx: &Sender<Msg>,
    model: &mut Model,
    log_tailer: &mut LogTailer,
    process_slot: &Arc<Mutex<Option<ProcessHandle>>>,
) -> Result<()> {
    match effect {
        Effect::Connect { profile, settings } => {
            if let Some(mut handle) = process_slot.lock().unwrap().take() {
                if let Err(e) = handle.kill_and_wait() {
                    tracing::warn!("Failed to stop sing-box process: {}", e);
                }
            } else if let Some(pid) = model.singbox_pid {
                unsafe {
                    let _ = libc::kill(pid as i32, libc::SIGTERM);
                }
            }
            model.connection = ConnectionState::ConnectPending;
            let tx = tx.clone();
            let slot = process_slot.clone();
            thread::spawn(
                move || match crate::singbox::runner::start(&profile, &settings) {
                    Ok(handle) => {
                        let pid = handle.pid;
                        *slot.lock().unwrap() = Some(handle);
                        let _ = tx.send(Msg::Connected { pid });
                    }
                    Err(e) => {
                        let _ = tx.send(Msg::ConnectFailed(e.to_string()));
                    }
                },
            );
        }
        Effect::Disconnect => {
            if let Some(mut handle) = process_slot.lock().unwrap().take() {
                if let Err(e) = handle.kill_and_wait() {
                    tracing::warn!("Failed to stop sing-box process: {}", e);
                }
            } else if let Some(pid) = model.singbox_pid {
                unsafe {
                    let _ = libc::kill(pid as i32, libc::SIGTERM);
                }
            }
            model.connection = ConnectionState::Idle;
            model.active_profile_id = None;
            model.singbox_pid = None;
            model.set_status_and_log(AppStatus::Info("Disconnected".into()));
            model.overlay = Overlay::None;
            crate::services::waybar::write_state(model);
        }
        Effect::DownloadGeo => {
            model.geo_updating = true;
            let tx = tx.clone();
            let region = model
                .config
                .settings
                .geo_region
                .unwrap_or(crate::config::profile::GeoRegion::Global);
            thread::spawn(move || {
                let result = match crate::infra::geo::GeoManager::new() {
                    Ok(gm) => match gm.update_if_needed(region) {
                        Ok(geo_result) => geo_result,
                        Err(e) => GeoResult::Error(e.to_string()),
                    },
                    Err(e) => GeoResult::Error(e.to_string()),
                };
                let _ = tx.send(Msg::GeoUpdated(result));
            });
        }
        Effect::TailLogs => {
            for line in log_tailer.tail() {
                let _ = tx.send(Msg::LogLine(line));
            }
        }
        Effect::WriteState => {
            crate::services::waybar::write_state(model);
        }
        Effect::SaveConfig => {
            if let Err(e) = model.save() {
                model.set_status_and_log(AppStatus::Error(format!("Failed to save config: {}", e)));
            }
        }
        Effect::PasteClipboard => {
            // In daemon mode paste is handled via IpcCommand::Paste directly;
            // this variant should not normally reach the daemon.
        }
        Effect::BroadcastState => {}
        Effect::Quit => {
            model.should_quit = true;
        }
        Effect::OpenEditor(_) => {
            // Editor is a TUI-local operation; daemon ignores it.
        }
    }
    Ok(())
}

fn build_snapshot(model: &Model) -> StateSnapshot {
    StateSnapshot {
        connection: model.connection,
        status: model.status.text().to_string(),
        status_is_error: matches!(model.status, AppStatus::Error(_)),
        singbox_pid: model.singbox_pid,
        active_profile_id: model.active_profile_id.map(|id| id.to_string()),
        selected: model.selected,
        routing_selected: model.routing_selected,
        geo_region_selected: model.geo_region_selected,
        geo_updating: model.geo_updating,
        overlay: model.overlay,
        profiles: model.config.profiles.clone(),
        settings: model.config.settings.clone(),
    }
}

fn spawn_ticker(tx: Sender<Msg>) {
    thread::spawn(move || {
        loop {
            thread::sleep(Duration::from_millis(250));
            if tx.send(Msg::Tick).is_err() {
                break;
            }
        }
    });
}

fn spawn_suspend_watcher(tx: Sender<Msg>) {
    thread::spawn(move || {
        crate::services::suspend::listen_blocking(tx);
    });
}
