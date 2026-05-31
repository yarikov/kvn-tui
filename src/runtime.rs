use std::io;
use std::sync::mpsc::{channel, Sender};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use crossterm::event;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::effect::Effect;
use crate::model::Model;
use crate::msg::{GeoResult, Msg};
use crate::services::LogTailer;
use crate::update::update;

/// Run the TUI main loop until the user requests quit.
pub fn run(mut model: Model) -> Result<()> {
    let (tx, rx) = channel::<Msg>();

    spawn_event_reader(tx.clone());
    spawn_ticker(tx.clone());
    spawn_suspend_watcher(tx.clone());

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut log_tailer = LogTailer::new(crate::paths::singbox_log_path());

    let result = run_loop(&mut terminal, &mut model, rx, &tx, &mut log_tailer);

    // Ensure sing-box is stopped before exiting.
    crate::singbox::disconnect(&mut model);

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
            execute_effect(effect, tx, model, terminal, log_tailer)?;
        }

        if model.should_quit {
            break;
        }
    }
    Ok(())
}

fn execute_effect(
    effect: Effect,
    tx: &Sender<Msg>,
    model: &mut Model,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    log_tailer: &mut LogTailer,
) -> Result<()> {
    match effect {
        Effect::Connect(profile, settings) => {
            model.connection_pending = true;
            let tx = tx.clone();
            thread::spawn(move || {
                match crate::singbox::runner::start(&profile, &settings) {
                    Ok(child) => {
                        let _ = tx.send(Msg::Connected(child));
                    }
                    Err(e) => {
                        let _ = tx.send(Msg::ConnectFailed(e.to_string()));
                    }
                }
            });
        }
        Effect::Reconnect(profile, settings) => {
            // Kill existing process first
            crate::singbox::disconnect(model);
            model.connection_pending = true;
            let tx = tx.clone();
            thread::spawn(move || {
                match crate::singbox::runner::start(&profile, &settings) {
                    Ok(child) => {
                        let _ = tx.send(Msg::Connected(child));
                    }
                    Err(e) => {
                        let _ = tx.send(Msg::ConnectFailed(e.to_string()));
                    }
                }
            });
        }
        Effect::Disconnect => {
            crate::singbox::disconnect(model);
            model.connection_pending = false;
            model.active_profile_id = None;
            model.status = crate::model::AppStatus::Info("Disconnected".into());
            model.mode = crate::model::AppMode::Normal;
            crate::state_io::write_state(model);
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
            crate::state_io::write_state(model);
        }
        Effect::SaveConfig => {
            if let Err(e) = model.save() {
                tracing::warn!("Failed to save config: {}", e);
            }
        }
        Effect::OpenEditor(idx) => {
            disable_raw_mode()?;
            terminal.backend_mut().execute(LeaveAlternateScreen)?;
            let result = crate::editor::open_profiles_editor(idx).map_err(|e| e.to_string());
            enable_raw_mode()?;
            terminal.backend_mut().execute(EnterAlternateScreen)?;
            terminal.clear()?;
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

fn spawn_event_reader(tx: Sender<Msg>) {
    thread::spawn(move || {
        loop {
            match event::read() {
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
        crate::suspend::listen_blocking(tx);
    });
}
