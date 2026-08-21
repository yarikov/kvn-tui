mod clipboard;
mod editor;
mod input;
pub(crate) mod theme_watch;

use std::io::{self, IsTerminal, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use crossterm::ExecutableCommand;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::app::model::Model;
use crate::app::msg::{IpcCommand, Msg};
use crate::ipc::IpcClient;
use crate::services::LogTailer;
use crate::ui::palette::to_rgb;
use ratatui::style::Color;

/// Format the OSC 11 escape sequence that asks the terminal emulator to
/// repaint its own background (the pixel padding around the character
/// grid that no TUI widget can reach). Most modern emulators
/// (Alacritty, Foot, Kitty, Ghostty, Konsole, xterm, WezTerm…) honor it;
/// the rest silently ignore the unknown OSC and stay as-is.
pub(crate) fn osc11(color: Color) -> String {
    let (r, g, b) = to_rgb(color);
    format!("\x1b]11;#{r:02x}{g:02x}{b:02x}\x1b\\")
}

/// OSC 111: reset terminal background to its default. Emitted on exit so
/// we don't leave the user's terminal stuck on our palette color.
pub(crate) const OSC_RESET_BG: &str = "\x1b]111\x1b\\";

/// Owns the terminal modes enabled by the TUI and restores them on every
/// return path (including an error from the render loop).
struct TerminalSession;

impl TerminalSession {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = stdout.execute(EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(error.into());
        }
        if let Err(error) = input::enable_keyboard_protocol(&mut stdout) {
            let _ = stdout.execute(LeaveAlternateScreen);
            let _ = disable_raw_mode();
            return Err(error.into());
        }
        Ok(Self)
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let mut stdout = io::stdout();
        let _ = input::disable_keyboard_protocol(&mut stdout);
        let _ = disable_raw_mode();
        let _ = stdout.execute(LeaveAlternateScreen);
        reset_terminal_bg();
    }
}

/// Write OSC 11 to stdout (no-op when stdout isn't a TTY — pipes, CI,
/// captured output). Errors are swallowed: a terminal that doesn't
/// recognise the sequence is not a failure mode worth surfacing.
fn apply_terminal_bg(color: Color) {
    let mut stdout = io::stdout();
    if !stdout.is_terminal() {
        return;
    }
    let _ = stdout.write_all(osc11(color).as_bytes());
    let _ = stdout.flush();
}

/// Counterpart to [`apply_terminal_bg`]: restore terminal default.
fn reset_terminal_bg() {
    let mut stdout = io::stdout();
    if !stdout.is_terminal() {
        return;
    }
    let _ = stdout.write_all(OSC_RESET_BG.as_bytes());
    let _ = stdout.flush();
}

/// Run the TUI client: connects to daemon, renders UI, forwards input.
pub fn run() -> Result<()> {
    let mut client = IpcClient::connect()?;
    client.send(&IpcCommand::Attach)?;

    let config = crate::config::load_config().unwrap_or_default();
    let mut model = Model::from_config(config.clone());
    model.theme = theme_watch::resolve_active(&model.config.settings.theme);

    let (tx, rx) = channel::<Msg>();
    let event_reading_enabled = Arc::new(AtomicBool::new(true));
    spawn_ticker(tx.clone());
    theme_watch::spawn_theme_watcher(tx.clone());
    client.spawn_reader(tx.clone())?;

    let _terminal_session = TerminalSession::enter()?;
    apply_terminal_bg(model.theme.palette_background());
    let stdout = io::stdout();
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    input::spawn_event_reader(tx, event_reading_enabled.clone());

    let mut log_tailer = LogTailer::new(vec![
        (crate::paths::app_log_path(), "[app]"),
        (crate::paths::singbox_log_path(), "[sb]"),
    ]);

    run_loop(
        &mut terminal,
        &mut model,
        rx,
        &mut client,
        &mut log_tailer,
        event_reading_enabled,
    )
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
                        if let Ok(text) = self::clipboard::read_clipboard_text() {
                            client.send(&IpcCommand::Paste { text })?;
                        }
                    }
                    KeyCode::Char('y') if model.overlay == crate::app::model::Overlay::None => {
                        if let Some(profile) = model.selected_profile() {
                            if let Ok(link) = crate::config::profile::encode_share_link(profile)
                                && self::clipboard::write_clipboard_text(&link).is_ok()
                            {
                                client.send(&IpcCommand::Copied {
                                    name: profile.name.clone(),
                                    count: 1,
                                })?;
                            }
                        } else if let Some(sub) = model.selected_subscription()
                            && self::clipboard::write_clipboard_text(&sub.url).is_ok()
                        {
                            client.send(&IpcCommand::Copied {
                                name: sub.name.clone(),
                                count: 1,
                            })?;
                        }
                    }
                    KeyCode::Char('e') => {
                        event_reading_enabled.store(false, Ordering::Relaxed);
                        disable_raw_mode()?;
                        terminal.backend_mut().execute(LeaveAlternateScreen)?;
                        let result = self::editor::open_profiles_editor(
                            model.selected_profile_index().unwrap_or(0),
                        );
                        enable_raw_mode()?;
                        terminal.backend_mut().execute(EnterAlternateScreen)?;
                        terminal.clear()?;
                        input::discard_pending_input();
                        event_reading_enabled.store(true, Ordering::Relaxed);
                        match result {
                            Ok(_) => {
                                if let Ok(config) = crate::config::load_config() {
                                    model.selected = crate::app::model::row_for_profile(
                                        &config,
                                        config.resolve_selected(),
                                    );
                                    model.config = config;
                                }
                                client.send(&IpcCommand::ReloadConfig)?;
                            }
                            Err(e) => {
                                // ConfigBackup restored the original file, so
                                // profiles.json is intact — but the user's
                                // edits are gone. Route the message through
                                // the daemon: it updates its own model's
                                // status (surviving apply_snapshot on the
                                // client) and appends to app.log, which the
                                // client's LogTailer picks up on the next
                                // tick and shows in the log panel.
                                let message = format!("Edit rejected: {e:#}");
                                client.send(&IpcCommand::ClientError { message })?;
                            }
                        }
                        needs_redraw = true;
                    }
                    _ => {
                        forward_key()?;
                    }
                }
            }
            Msg::StateUpdate(snapshot) => {
                apply_snapshot(model, *snapshot);
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
            Msg::ThemeChanged(theme)
                if model.config.settings.theme == theme_watch::OMARCHY_SENTINEL =>
            {
                let prev_bg = model.theme.palette_background();
                model.theme = theme;
                if model.theme.palette_background() != prev_bg {
                    apply_terminal_bg(model.theme.palette_background());
                }
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
    model.dns_selected = snapshot.dns_selected;
    model.dns_strategy_draft = snapshot.dns_strategy_draft;
    model.dns_fakeip_draft = snapshot.dns_fakeip_draft;
    model.theme_selected = snapshot.theme_selected;
    model.theme_draft = snapshot.theme_draft.clone();
    model.service_routing_selected = snapshot.service_routing_selected;
    model.service_routing_draft = snapshot.service_routing_draft;
    model.geo_updating = snapshot.geo_updating;
    model.geo_last_updated = snapshot.geo_last_updated;
    model.overlay = snapshot.overlay;
    model.config.profiles = snapshot.profiles;
    model.config.subscriptions = snapshot.subscriptions;
    model.config.settings = snapshot.settings;
    model.traffic = snapshot.traffic;
    model.profile_latencies = snapshot
        .profile_latencies
        .into_iter()
        .filter_map(|(s, ms)| uuid::Uuid::parse_str(&s).ok().map(|id| (id, ms)))
        .collect();

    model.testing_profiles = snapshot
        .testing_profiles
        .into_iter()
        .filter_map(|s| uuid::Uuid::parse_str(&s).ok())
        .collect();
    // Resolve the effective theme: live-preview draft wins while the
    // picker is open, otherwise honor the committed `Settings.theme`.
    let effective_slug = model
        .theme_draft
        .as_deref()
        .unwrap_or(&model.config.settings.theme);
    let prev_bg = model.theme.palette_background();
    model.theme = theme_watch::resolve_active(effective_slug);
    if model.theme.palette_background() != prev_bg {
        apply_terminal_bg(model.theme.palette_background());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc11_formats_rgb_color_as_hex_triplet() {
        // Tokyo Night accent — typical RGB value.
        assert_eq!(osc11(Color::Rgb(0x7a, 0xa2, 0xf7)), "\x1b]11;#7aa2f7\x1b\\");
    }

    #[test]
    fn osc11_formats_named_ansi_colors_via_to_rgb_table() {
        // Color::Black maps to (0, 0, 0) in palette::to_rgb.
        assert_eq!(osc11(Color::Black), "\x1b]11;#000000\x1b\\");
    }

    #[test]
    fn osc_reset_bg_is_osc_111() {
        assert_eq!(OSC_RESET_BG, "\x1b]111\x1b\\");
    }
}
