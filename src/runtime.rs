use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use crossterm::ExecutableCommand;
use crossterm::event;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::app::effect::Effect;
use crate::app::model::Model;
use crate::app::msg::{GeoResult, Msg};
use crate::app::update::update;
use crate::process_handle::ProcessHandle;
use crate::services::LogTailer;

/// Run the TUI main loop until the user requests quit.
pub fn run(mut model: Model) -> Result<()> {
    let (tx, rx) = channel::<Msg>();
    let event_reading_enabled = Arc::new(AtomicBool::new(true));

    spawn_event_reader(tx.clone(), event_reading_enabled.clone());
    spawn_ticker(tx.clone());
    spawn_suspend_watcher(tx.clone());

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut log_tailer = LogTailer::new(crate::paths::singbox_log_path());

    let process_slot = Arc::new(Mutex::new(None));

    let result = run_loop(
        &mut terminal,
        &mut model,
        rx,
        &tx,
        &mut log_tailer,
        process_slot.clone(),
        event_reading_enabled,
    );

    // Ensure sing-box is stopped before exiting.
    if let Some(mut handle) = process_slot.lock().unwrap().take() {
        if let Err(e) = handle.kill_and_wait() {
            tracing::warn!("Failed to stop sing-box on exit: {}", e);
        }
    }

    disable_raw_mode()?;
    terminal.backend_mut().execute(LeaveAlternateScreen)?;

    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    model: &mut Model,
    rx: std::sync::mpsc::Receiver<Msg>,
    tx: &Sender<Msg>,
    log_tailer: &mut LogTailer,
    process_slot: Arc<Mutex<Option<ProcessHandle>>>,
    event_reading_enabled: Arc<AtomicBool>,
) -> Result<()> {
    loop {
        if model.needs_redraw {
            terminal.clear()?;
            model.needs_redraw = false;
        }
        terminal.draw(|f| crate::ui::draw(f, model))?;

        let msg = rx.recv()?;
        let effects = update(model, msg);

        for effect in effects {
            execute_effect(
                effect,
                &rx,
                tx,
                model,
                terminal,
                log_tailer,
                &process_slot,
                &event_reading_enabled,
            )?;
        }

        if model.should_quit {
            break;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn execute_effect(
    effect: Effect,
    rx: &std::sync::mpsc::Receiver<Msg>,
    tx: &Sender<Msg>,
    model: &mut Model,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    log_tailer: &mut LogTailer,
    process_slot: &Arc<Mutex<Option<ProcessHandle>>>,
    event_reading_enabled: &Arc<AtomicBool>,
) -> Result<()> {
    match effect {
        Effect::Connect { profile, settings } => {
            if let Some(mut handle) = process_slot.lock().unwrap().take() {
                if let Err(e) = handle.kill_and_wait() {
                    tracing::warn!("Failed to stop sing-box process: {}", e);
                }
            }
            model.connection = crate::app::model::ConnectionState::ConnectPending;
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
            }
            model.connection = crate::app::model::ConnectionState::Idle;
            model.active_profile_id = None;
            model.singbox_pid = None;
            model.status = crate::app::model::AppStatus::Info("Disconnected".into());
            model.overlay = crate::app::model::Overlay::None;
            crate::services::state_io::write_state(model);
        }
        Effect::DownloadGeo => {
            model.geo_updating = true;
            let tx = tx.clone();
            thread::spawn(move || {
                let result = match crate::geo::GeoManager::new() {
                    Ok(gm) => match gm.update_if_needed() {
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
            crate::services::state_io::write_state(model);
        }
        Effect::SaveConfig => {
            if let Err(e) = model.save() {
                tracing::warn!("Failed to save config: {}", e);
            }
        }
        Effect::OpenEditor(idx) => {
            event_reading_enabled.store(false, Ordering::Relaxed);
            disable_raw_mode()?;
            terminal.backend_mut().execute(LeaveAlternateScreen)?;
            let result = crate::editor::open_profiles_editor(idx).map_err(|e| e.to_string());
            enable_raw_mode()?;
            terminal.backend_mut().execute(EnterAlternateScreen)?;
            terminal.clear()?;
            event_reading_enabled.store(true, Ordering::Relaxed);
            // Drain leaked input events but preserve state messages.
            let mut to_requeue = Vec::new();
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    Msg::Key(_) | Msg::Resize => {}
                    other => to_requeue.push(other),
                }
            }
            for msg in to_requeue {
                let _ = tx.send(msg);
            }
            let _ = tx.send(Msg::EditorClosed(result));
        }
        Effect::PasteClipboard => {
            let tx = tx.clone();
            thread::spawn(move || {
                let result = crate::clipboard::read_clipboard_text().map_err(|e| e.to_string());
                let _ = tx.send(Msg::ClipboardRead(result));
            });
        }
        Effect::Quit => {
            model.should_quit = true;
        }
    }
    Ok(())
}

fn spawn_event_reader(tx: Sender<Msg>, reading_enabled: Arc<AtomicBool>) {
    thread::spawn(move || {
        loop {
            if !reading_enabled.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(50));
                continue;
            }
            match event::poll(Duration::from_millis(100)) {
                Ok(true) => match event::read() {
                    Ok(event::Event::Key(key)) => {
                        if tx.send(Msg::Key(key)).is_err() {
                            break;
                        }
                    }
                    Ok(event::Event::Resize(_, _)) => {
                        if tx.send(Msg::Resize).is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => break,
                },
                Ok(false) => {}
                Err(_) => break,
            }
        }
    });
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
