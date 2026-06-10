use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
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

use crate::app::model::Model;
use crate::app::msg::{IpcCommand, Msg};
use crate::ipc::IpcClient;
use crate::services::LogTailer;

/// Run the TUI client: connects to daemon, renders UI, forwards input.
pub fn run() -> Result<()> {
    let mut client = IpcClient::connect()?;
    client.send(&IpcCommand::Attach)?;

    let config = crate::config::load_config().unwrap_or_default();
    let mut model = Model::from_config(config.clone());

    let (tx, rx) = channel::<Msg>();
    let event_reading_enabled = Arc::new(AtomicBool::new(true));
    spawn_event_reader(tx.clone(), event_reading_enabled.clone());
    spawn_ticker(tx.clone());
    client.spawn_reader(tx);

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut log_tailer = LogTailer::new(vec![
        (crate::infra::paths::app_log_path(), "[app]"),
        (crate::infra::paths::singbox_log_path(), "[sb]"),
    ]);

    let result = run_loop(
        &mut terminal,
        &mut model,
        rx,
        &mut client,
        &mut log_tailer,
        event_reading_enabled,
    );

    disable_raw_mode()?;
    terminal.backend_mut().execute(LeaveAlternateScreen)?;

    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    model: &mut Model,
    rx: std::sync::mpsc::Receiver<Msg>,
    client: &mut IpcClient,
    log_tailer: &mut LogTailer,
    event_reading_enabled: Arc<AtomicBool>,
) -> Result<()> {
    // Initial draw
    terminal.draw(|f| crate::ui::draw(f, model))?;

    loop {
        let msg = rx.recv()?;
        let mut needs_redraw = false;

        match msg {
            Msg::Key(key) => {
                use crossterm::event::{KeyCode, KeyModifiers};
                let mut forward_key = || {
                    let (code, ch) = match key.code {
                        KeyCode::Char(c) => ("Char".to_string(), Some(c)),
                        other => (format!("{:?}", other), None),
                    };
                    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                    client.send(&IpcCommand::Key {
                        code,
                        char: ch,
                        ctrl,
                    })
                };
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => {
                        if model.overlay == crate::app::model::Overlay::None {
                            client.send(&IpcCommand::Detach)?;
                            break;
                        }
                        forward_key()?;
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        let _ = client.send(&IpcCommand::Quit);
                        std::thread::sleep(Duration::from_millis(300));
                        break;
                    }
                    KeyCode::Char('p') => {
                        if let Ok(text) = crate::infra::clipboard::read_clipboard_text() {
                            client.send(&IpcCommand::Paste { text })?;
                        }
                    }
                    KeyCode::Char('e') => {
                        event_reading_enabled.store(false, Ordering::Relaxed);
                        disable_raw_mode()?;
                        terminal.backend_mut().execute(LeaveAlternateScreen)?;
                        let result = crate::infra::editor::open_profiles_editor(model.selected);
                        enable_raw_mode()?;
                        terminal.backend_mut().execute(EnterAlternateScreen)?;
                        terminal.clear()?;
                        event_reading_enabled.store(true, Ordering::Relaxed);
                        if result.is_ok() {
                            if let Ok(config) = crate::config::load_config() {
                                model.selected = config.resolve_selected();
                                model.config = config;
                            }
                            client.send(&IpcCommand::ReloadConfig)?;
                        }
                        needs_redraw = true;
                    }
                    _ => {
                        forward_key()?;
                    }
                }
            }
            Msg::StateUpdate(snapshot) => {
                apply_snapshot(model, snapshot);
                needs_redraw = true;
            }
            Msg::Tick => {
                for line in log_tailer.tail() {
                    model.push_log(line);
                }
                needs_redraw = true;
            }
            Msg::Resize => {
                needs_redraw = true;
            }
            _ => {}
        }

        if needs_redraw {
            terminal.draw(|f| crate::ui::draw(f, model))?;
        }
    }
    Ok(())
}

fn apply_snapshot(model: &mut Model, snapshot: crate::app::msg::StateSnapshot) {
    model.connection = snapshot.connection;
    model.status = if snapshot.status_is_error {
        crate::app::model::AppStatus::Error(snapshot.status)
    } else {
        crate::app::model::AppStatus::Info(snapshot.status)
    };
    model.singbox_pid = snapshot.singbox_pid;
    model.active_profile_id = snapshot
        .active_profile_id
        .and_then(|s| uuid::Uuid::parse_str(&s).ok());
    model.selected = snapshot.selected;
    model.routing_selected = snapshot.routing_selected;
    model.geo_region_selected = snapshot.geo_region_selected;
    model.geo_updating = snapshot.geo_updating;
    model.overlay = snapshot.overlay;
    model.config.profiles = snapshot.profiles;
    model.config.settings = snapshot.settings;
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
