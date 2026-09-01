mod clipboard;
mod editor;
mod input;
pub(crate) mod theme_watch;

use std::io::{self, IsTerminal, Write};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

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
/// Show a pointing hand over clickable rows; an empty shape list restores the
/// terminal's contextual default (usually an I-beam over terminal text).
pub(crate) const OSC_POINTER_INTERACTIVE: &str = "\x1b]22;pointer\x1b\\";
pub(crate) const OSC_POINTER_TEXT: &str = "\x1b]22;text\x1b\\";
pub(crate) const OSC_POINTER_DEFAULT: &str = "\x1b]22;\x1b\\";
const DOUBLE_CLICK_INTERVAL: Duration = Duration::from_millis(300);

#[derive(Default)]
struct ClickTracker(Option<(uuid::Uuid, Instant)>);

#[derive(Debug, Default)]
struct GoFirstSequence {
    pending: bool,
}

impl GoFirstSequence {
    /// Consume one key and report whether it completes a consecutive `gg`.
    /// Any non-g key cancels a pending prefix before continuing normally.
    fn feed(&mut self, code: &crossterm::event::KeyCode) -> bool {
        if *code != crossterm::event::KeyCode::Char('g') {
            self.pending = false;
            return false;
        }
        if self.pending {
            self.pending = false;
            true
        } else {
            self.pending = true;
            false
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum PointerShape {
    #[default]
    Default,
    Source,
    Logs,
}

impl ClickTracker {
    fn profile_pressed(&mut self, profile_id: uuid::Uuid, now: Instant) -> bool {
        let double = self.0.is_some_and(|(previous, at)| {
            previous == profile_id && now.saturating_duration_since(at) <= DOUBLE_CLICK_INTERVAL
        });
        self.0 = (!double).then_some((profile_id, now));
        double
    }

    fn reset(&mut self) {
        self.0 = None;
    }
}

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
        if let Err(error) = input::enable_mouse_capture(&mut stdout) {
            let _ = input::disable_mouse_capture(&mut stdout);
            let _ = input::disable_keyboard_protocol(&mut stdout);
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
        let _ = stdout.write_all(OSC_POINTER_DEFAULT.as_bytes());
        let _ = input::disable_mouse_capture(&mut stdout);
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
    let event_reader_control = Arc::new(input::EventReaderControl::new());
    spawn_ticker(tx.clone());
    theme_watch::spawn_theme_watcher(tx.clone());
    client.spawn_reader(tx.clone())?;

    let mut log_tailer = LogTailer::new(vec![
        (crate::paths::app_log_path(), "[app]"),
        (crate::paths::singbox_log_path(), "[sb]"),
    ]);
    receive_initial_state(&rx, &mut model, &mut log_tailer)?;

    let _terminal_session = TerminalSession::enter()?;
    apply_terminal_bg(model.theme.palette_background());
    let stdout = io::stdout();
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    input::spawn_event_reader(tx, event_reader_control.clone());

    run_loop(
        &mut terminal,
        &mut model,
        rx,
        &mut client,
        &mut log_tailer,
        event_reader_control,
    )
}

fn receive_initial_state(
    rx: &Receiver<Msg>,
    model: &mut Model,
    log_tailer: &mut LogTailer,
) -> Result<()> {
    loop {
        match rx.recv()? {
            Msg::StateUpdate(snapshot) => {
                if let Some(offsets) = snapshot.log_session_offsets {
                    for line in log_tailer.load_history(
                        &[offsets.app, offsets.singbox],
                        crate::app::model::MAX_LOG_LINES,
                    ) {
                        model.push_log(line);
                    }
                }
                apply_snapshot(model, *snapshot);
                return Ok(());
            }
            Msg::ThemeChanged(theme)
                if model.config.settings.theme == theme_watch::OMARCHY_SENTINEL =>
            {
                model.theme = theme;
            }
            _ => {}
        }
    }
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    model: &mut Model,
    rx: std::sync::mpsc::Receiver<Msg>,
    client: &mut IpcClient,
    log_tailer: &mut LogTailer,
    event_reader_control: Arc<input::EventReaderControl>,
) -> Result<()> {
    use crate::app::model::MainPaneFocus;
    use crate::ui::layout::LogNavigation;

    let mut pane_focus = model.main_pane_focus;
    let mut log_navigation = LogNavigation::default();
    let mut go_first_sequence = GoFirstSequence::default();
    // Initial draw
    terminal.draw(|f| crate::ui::draw(f, model))?;
    let mut pointer_shape = PointerShape::Default;
    let mut mouse_position: Option<(u16, u16)> = None;
    let mut click_tracker = ClickTracker::default();
    let mut log_selection: Option<crate::ui::layout::LogSelection> = None;
    let mut log_dragging = false;

    loop {
        let msg = rx.recv()?;
        let mut needs_redraw = false;

        match msg {
            Msg::Mouse(mouse) => {
                use crossterm::event::{MouseButton, MouseEventKind};
                mouse_position = Some((mouse.column, mouse.row));
                let hit =
                    update_pointer_shape(terminal, model, mouse_position, &mut pointer_shape)?;
                match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        log_selection = None;
                        log_dragging = false;
                        let area: ratatui::layout::Rect = terminal.size()?.into();
                        if let Some(selection) = crate::ui::layout::log_viewport_with_navigation(
                            model,
                            area,
                            Some(&log_navigation),
                        )
                        .and_then(|viewport| {
                            crate::ui::layout::LogSelection::start(
                                viewport,
                                mouse.column,
                                mouse.row,
                            )
                        }) {
                            pane_focus = MainPaneFocus::Logs;
                            client.send(&IpcCommand::SetMainPaneFocus {
                                focus: MainPaneFocus::Logs,
                            })?;
                            log_navigation.clear();
                            click_tracker.reset();
                            log_selection = Some(selection);
                            log_dragging = true;
                        } else if let Some(index) = hit {
                            pane_focus = MainPaneFocus::Sources;
                            client.send(&IpcCommand::SetMainPaneFocus {
                                focus: MainPaneFocus::Sources,
                            })?;
                            client.send(&IpcCommand::SelectSource { index })?;
                            model.selected = index;
                            let profile_id = match model.source_rows()[index] {
                                crate::app::model::SourceRow::StandaloneProfile(profile_idx)
                                | crate::app::model::SourceRow::SubscriptionProfile {
                                    profile_idx,
                                    ..
                                } => Some(model.config.profiles[profile_idx].id),
                                crate::app::model::SourceRow::SubscriptionHeader(_) => None,
                            };
                            if let Some(profile_id) = profile_id
                                && click_tracker.profile_pressed(profile_id, Instant::now())
                            {
                                client.send(&IpcCommand::ConnectProfile { profile_id })?;
                            } else if profile_id.is_none() {
                                click_tracker.reset();
                            }
                        } else if pointer_shape == PointerShape::Logs {
                            pane_focus = MainPaneFocus::Logs;
                            client.send(&IpcCommand::SetMainPaneFocus {
                                focus: MainPaneFocus::Logs,
                            })?;
                            click_tracker.reset();
                        } else {
                            click_tracker.reset();
                        }
                        needs_redraw = true;
                    }
                    MouseEventKind::Moved => {
                        if log_dragging && let Some(selection) = &mut log_selection {
                            selection.update(mouse.column, mouse.row);
                            needs_redraw = true;
                        }
                    }
                    MouseEventKind::Up(MouseButton::Left) => {
                        if log_dragging && let Some(selection) = &mut log_selection {
                            log_dragging = false;
                            selection.update(mouse.column, mouse.row);
                            let copy_result = (!selection.is_empty())
                                .then(|| self::clipboard::write_clipboard_text(&selection.text()));
                            log_selection = None;
                            if let Some(result) = copy_result {
                                match result {
                                    Ok(()) => client.send(&IpcCommand::Copied {
                                        name: "log".into(),
                                        count: 1,
                                    })?,
                                    Err(error) => client.send(&IpcCommand::ClientError {
                                        message: format!("Failed to copy log text: {error:#}"),
                                    })?,
                                }
                            }
                            needs_redraw = true;
                        }
                    }
                    _ => {}
                }
            }
            Msg::Key(key) => {
                use crossterm::event::{KeyCode, KeyModifiers};
                let completes_gg = go_first_sequence.feed(&key.code);
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
                    KeyCode::Char('g') => {
                        if completes_gg {
                            if model.overlay == crate::app::model::Overlay::None
                                && pane_focus == MainPaneFocus::Logs
                            {
                                log_navigation.select_buffer_edge(
                                    model.logs.len(),
                                    true,
                                    Instant::now(),
                                );
                            } else {
                                client.send(&IpcCommand::GoFirst)?;
                            }
                        }
                        needs_redraw = true;
                    }
                    KeyCode::Char('q') | KeyCode::Esc => {
                        if key.code == KeyCode::Esc
                            && model.overlay == crate::app::model::Overlay::None
                            && pane_focus == MainPaneFocus::Logs
                            && log_navigation.is_visual()
                        {
                            log_navigation.cancel_visual();
                            needs_redraw = true;
                        } else if model.overlay == crate::app::model::Overlay::None {
                            client.send(&IpcCommand::Detach)?;
                            break;
                        } else {
                            forward_key()?;
                        }
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        let _ = client.send(&IpcCommand::Quit);
                        std::thread::sleep(Duration::from_millis(300));
                        break;
                    }
                    KeyCode::Char('h') | KeyCode::Left
                        if model.overlay == crate::app::model::Overlay::None =>
                    {
                        pane_focus = MainPaneFocus::Sources;
                        client.send(&IpcCommand::SetMainPaneFocus {
                            focus: MainPaneFocus::Sources,
                        })?;
                        needs_redraw = true;
                    }
                    KeyCode::Char('l') | KeyCode::Right
                        if model.overlay == crate::app::model::Overlay::None =>
                    {
                        pane_focus = MainPaneFocus::Logs;
                        client.send(&IpcCommand::SetMainPaneFocus {
                            focus: MainPaneFocus::Logs,
                        })?;
                        needs_redraw = true;
                    }
                    KeyCode::Char('j') | KeyCode::Down
                        if model.overlay == crate::app::model::Overlay::None
                            && pane_focus == MainPaneFocus::Logs =>
                    {
                        let area: ratatui::layout::Rect = terminal.size()?.into();
                        let now = Instant::now();
                        if log_navigation.cursor().is_none() {
                            if let Some(viewport) = crate::ui::layout::log_viewport_with_navigation(
                                model,
                                area,
                                Some(&log_navigation),
                            ) {
                                log_navigation.select_edge(&viewport, true, now);
                            }
                        } else {
                            crate::ui::layout::sync_log_scroll(model, area, &mut log_navigation);
                            log_navigation.move_by(1, model.logs.len(), now);
                        }
                        needs_redraw = true;
                    }
                    KeyCode::Char('k') | KeyCode::Up
                        if model.overlay == crate::app::model::Overlay::None
                            && pane_focus == MainPaneFocus::Logs =>
                    {
                        let area: ratatui::layout::Rect = terminal.size()?.into();
                        let now = Instant::now();
                        if log_navigation.cursor().is_none() {
                            if let Some(viewport) = crate::ui::layout::log_viewport_with_navigation(
                                model,
                                area,
                                Some(&log_navigation),
                            ) {
                                log_navigation.select_edge(&viewport, false, now);
                            }
                        } else {
                            crate::ui::layout::sync_log_scroll(model, area, &mut log_navigation);
                            log_navigation.move_by(-1, model.logs.len(), now);
                        }
                        needs_redraw = true;
                    }
                    KeyCode::Char('G')
                        if model.overlay == crate::app::model::Overlay::None
                            && pane_focus == MainPaneFocus::Logs =>
                    {
                        log_navigation.select_buffer_edge(model.logs.len(), false, Instant::now());
                        needs_redraw = true;
                    }
                    KeyCode::Char('V')
                        if model.overlay == crate::app::model::Overlay::None
                            && pane_focus == MainPaneFocus::Logs =>
                    {
                        let now = Instant::now();
                        if log_navigation.cursor().is_none() {
                            let area: ratatui::layout::Rect = terminal.size()?.into();
                            if let Some(viewport) = crate::ui::layout::log_viewport_with_navigation(
                                model,
                                area,
                                Some(&log_navigation),
                            ) {
                                log_navigation.select_edge(&viewport, true, now);
                            }
                        }
                        log_navigation.enter_visual(now);
                        needs_redraw = true;
                    }
                    KeyCode::Char('y')
                        if model.overlay == crate::app::model::Overlay::None
                            && pane_focus == MainPaneFocus::Logs =>
                    {
                        if let Some((text, count)) = log_navigation.selected_text(model) {
                            match self::clipboard::write_clipboard_text(&text) {
                                Ok(()) => {
                                    log_navigation.copied(Instant::now());
                                    client.send(&IpcCommand::Copied {
                                        name: "log".into(),
                                        count,
                                    })?;
                                }
                                Err(error) => client.send(&IpcCommand::ClientError {
                                    message: format!("Failed to copy log text: {error:#}"),
                                })?,
                            }
                        }
                        needs_redraw = true;
                    }
                    KeyCode::Char('p')
                        if model.overlay == crate::app::model::Overlay::None
                            && pane_focus == MainPaneFocus::Sources =>
                    {
                        if let Ok(text) = self::clipboard::read_clipboard_text() {
                            client.send(&IpcCommand::Paste { text })?;
                        }
                    }
                    KeyCode::Char('y')
                        if model.overlay == crate::app::model::Overlay::None
                            && pane_focus == MainPaneFocus::Sources =>
                    {
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
                    KeyCode::Char('e')
                        if model.overlay == crate::app::model::Overlay::None
                            && pane_focus == MainPaneFocus::Sources =>
                    {
                        log_selection = None;
                        log_dragging = false;
                        anyhow::ensure!(
                            event_reader_control.pause(),
                            "Timed out while pausing terminal input for external editor"
                        );
                        terminal
                            .backend_mut()
                            .write_all(OSC_POINTER_DEFAULT.as_bytes())?;
                        input::disable_mouse_capture(terminal.backend_mut())?;
                        input::disable_keyboard_protocol(terminal.backend_mut())?;
                        disable_raw_mode()?;
                        terminal.backend_mut().execute(LeaveAlternateScreen)?;
                        let target = model.selected_row().map(self::editor::EditorTarget::from);
                        let result = self::editor::open_profiles_editor(target);
                        enable_raw_mode()?;
                        terminal.backend_mut().execute(EnterAlternateScreen)?;
                        input::enable_keyboard_protocol(terminal.backend_mut())?;
                        input::enable_mouse_capture(terminal.backend_mut())?;
                        pointer_shape = PointerShape::Default;
                        terminal.clear()?;
                        input::discard_pending_input();
                        event_reader_control.resume();
                        update_pointer_shape(terminal, model, mouse_position, &mut pointer_shape)?;
                        match result {
                            Ok(_) => {
                                if let Ok(config) = crate::config::load_config() {
                                    model.replace_config_preserving_selection(config);
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
                pane_focus = snapshot.main_pane_focus;
                apply_snapshot(model, *snapshot);
                if model.overlay != crate::app::model::Overlay::None {
                    log_selection = None;
                    log_dragging = false;
                }
                update_pointer_shape(terminal, model, mouse_position, &mut pointer_shape)?;
                needs_redraw = true;
            }
            Msg::Tick => {
                log_navigation.expire_if_idle(Instant::now());
                let new_lines = log_tailer.tail();
                if !new_lines.is_empty() && !log_dragging {
                    log_selection = None;
                }
                for line in new_lines {
                    if model.logs.len() == crate::app::model::MAX_LOG_LINES {
                        log_navigation.oldest_log_evicted();
                    }
                    model.push_log(line);
                }
                needs_redraw = true;
            }
            Msg::Resize => {
                log_selection = None;
                log_dragging = false;
                update_pointer_shape(terminal, model, mouse_position, &mut pointer_shape)?;
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
            terminal.draw(|f| {
                crate::ui::layout::draw_with_interaction(
                    f,
                    model,
                    pane_focus,
                    Some(&log_navigation),
                    log_selection.as_ref(),
                )
            })?;
        }
    }
    Ok(())
}

fn update_pointer_shape(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    model: &Model,
    position: Option<(u16, u16)>,
    pointer_shape: &mut PointerShape,
) -> Result<Option<usize>> {
    let area: ratatui::layout::Rect = terminal.size()?.into();
    let hit = if let Some((column, row)) = position {
        crate::ui::layout::source_hit_test(model, area, column, row)
    } else {
        None
    };
    let next_shape = if hit.is_some() {
        PointerShape::Source
    } else if position.is_some_and(|(column, row)| {
        crate::ui::layout::log_viewport(model, area)
            .is_some_and(|viewport| viewport.contains(column, row))
    }) {
        PointerShape::Logs
    } else {
        PointerShape::Default
    };
    if next_shape != *pointer_shape {
        let sequence = match next_shape {
            PointerShape::Default => OSC_POINTER_DEFAULT,
            PointerShape::Source => OSC_POINTER_INTERACTIVE,
            PointerShape::Logs => OSC_POINTER_TEXT,
        };
        terminal.backend_mut().write_all(sequence.as_bytes())?;
        terminal.backend_mut().flush()?;
        *pointer_shape = next_shape;
    }
    Ok(hit)
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
    model.main_pane_focus = snapshot.main_pane_focus;
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

    #[test]
    fn click_tracker_requires_same_profile_within_300ms() {
        let first = uuid::Uuid::new_v4();
        let other = uuid::Uuid::new_v4();
        let start = Instant::now();

        let mut tracker = ClickTracker::default();
        assert!(!tracker.profile_pressed(first, start));
        assert!(tracker.profile_pressed(first, start + Duration::from_millis(300)));

        assert!(!tracker.profile_pressed(first, start));
        assert!(!tracker.profile_pressed(other, start + Duration::from_millis(100)));
        assert!(!tracker.profile_pressed(other, start + Duration::from_millis(401)));
    }

    #[test]
    fn go_first_sequence_requires_two_consecutive_g_keys() {
        use crossterm::event::KeyCode;

        let mut sequence = GoFirstSequence::default();
        assert!(!sequence.feed(&KeyCode::Char('g')));
        assert!(sequence.feed(&KeyCode::Char('g')));
        assert!(!sequence.feed(&KeyCode::Char('g')));
    }

    #[test]
    fn go_first_sequence_is_cancelled_by_another_key() {
        use crossterm::event::KeyCode;

        let mut sequence = GoFirstSequence::default();
        assert!(!sequence.feed(&KeyCode::Char('g')));
        assert!(!sequence.feed(&KeyCode::Char('j')));
        assert!(!sequence.feed(&KeyCode::Char('g')));
        assert!(sequence.feed(&KeyCode::Char('g')));
    }

    #[test]
    fn pointer_shape_sequences_use_osc_22() {
        assert_eq!(OSC_POINTER_INTERACTIVE, "\x1b]22;pointer\x1b\\");
        assert_eq!(OSC_POINTER_TEXT, "\x1b]22;text\x1b\\");
        assert_eq!(OSC_POINTER_DEFAULT, "\x1b]22;\x1b\\");
    }
}
