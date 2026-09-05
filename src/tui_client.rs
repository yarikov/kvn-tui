mod clipboard;
mod editor;
mod input;
pub(crate) mod theme_watch;

use std::io::{self, IsTerminal, Write};
use std::sync::Arc;
use std::sync::mpsc::{Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::ExecutableCommand;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::app::model::{AppStatus, ConnectionState, Model, TrafficStats};
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
pub(crate) fn osc_color(slot: u8, color: Color) -> String {
    let (r, g, b) = to_rgb(color);
    format!("\x1b]{slot};#{r:02x}{g:02x}{b:02x}\x1b\\")
}

/// Reset terminal foreground and background to their configured defaults.
pub(crate) const OSC_RESET_COLORS: &str = "\x1b]110\x1b\\\x1b]111\x1b\\";
/// Show a pointing hand over clickable rows; an empty shape list restores the
/// terminal's contextual default (usually an I-beam over terminal text).
pub(crate) const OSC_POINTER_INTERACTIVE: &str = "\x1b]22;pointer\x1b\\";
pub(crate) const OSC_POINTER_TEXT: &str = "\x1b]22;text\x1b\\";
pub(crate) const OSC_POINTER_DEFAULT: &str = "\x1b]22;\x1b\\";
const DOUBLE_CLICK_INTERVAL: Duration = Duration::from_millis(300);
const TOAST_INFO_DURATION: Duration = Duration::from_secs(3);
const TOAST_ERROR_DURATION: Duration = Duration::from_secs(7);

/// Presentation-only lifetime for daemon status events. Keeping the deadline
/// here avoids leaking wall-clock concerns into the shared TEA model.
struct ToastState {
    last_revision: u64,
    status: Option<AppStatus>,
    expires_at: Option<Instant>,
    show_over_overlay: bool,
}

impl ToastState {
    fn new(last_revision: u64) -> Self {
        Self {
            last_revision,
            status: None,
            expires_at: None,
            show_over_overlay: false,
        }
    }

    fn show_initial_error(&mut self, status: AppStatus, now: Instant) {
        if matches!(status, AppStatus::Error(_)) {
            self.status = Some(status);
            self.expires_at = Some(now + TOAST_ERROR_DURATION);
            self.show_over_overlay = true;
        }
    }

    fn observe(&mut self, revision: u64, status: AppStatus, now: Instant) {
        if revision == self.last_revision {
            return;
        }
        self.last_revision = revision;
        if status.text().is_empty() || status.text() == "Press ? for help" {
            self.status = None;
            self.expires_at = None;
            self.show_over_overlay = false;
            return;
        }
        let duration = if matches!(status, AppStatus::Error(_)) {
            TOAST_ERROR_DURATION
        } else {
            TOAST_INFO_DURATION
        };
        self.status = Some(status);
        self.expires_at = Some(now + duration);
        self.show_over_overlay = false;
    }

    fn expire(&mut self, now: Instant) {
        if self.expires_at.is_some_and(|deadline| now >= deadline) {
            self.status = None;
            self.expires_at = None;
            self.show_over_overlay = false;
        }
    }

    fn current(&self) -> Option<&AppStatus> {
        self.status.as_ref()
    }

    fn show_over_overlay(&self) -> bool {
        self.show_over_overlay
    }
}

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
        reset_terminal_colors();
    }
}

/// Write OSC 11 to stdout (no-op when stdout isn't a TTY — pipes, CI,
/// captured output). Errors are swallowed: a terminal that doesn't
/// recognise the sequence is not a failure mode worth surfacing.
fn apply_terminal_colors(foreground: Color, background: Color) {
    let mut stdout = io::stdout();
    if !stdout.is_terminal() {
        return;
    }
    let _ = stdout.write_all(osc_color(10, foreground).as_bytes());
    let _ = stdout.write_all(osc_color(11, background).as_bytes());
    let _ = stdout.flush();
}

/// Counterpart to [`apply_terminal_colors`]: restore terminal defaults.
fn reset_terminal_colors() {
    let mut stdout = io::stdout();
    if !stdout.is_terminal() {
        return;
    }
    let _ = stdout.write_all(OSC_RESET_COLORS.as_bytes());
    let _ = stdout.flush();
}

/// Render a fixed, side-effect-free application state for documentation captures.
pub fn run_docs_preview(theme_slug: &str) -> Result<()> {
    use crate::config::profile::{
        Config, GeoAutoUpdate, GeoRegion, Hysteria2Config, ProtocolConfig, RoutingMode,
        Subscription, SubscriptionAutoUpdate, TrojanConfig, TuicConfig, VlessConfig, VmessConfig,
    };
    use crossterm::event::{self, Event, KeyCode, KeyModifiers};
    use uuid::Uuid;

    let Some(palette) = crate::ui::palette::Palette::lookup(theme_slug) else {
        anyhow::bail!("unknown bundled theme {theme_slug:?}");
    };

    let finland_subscription_id = Uuid::from_u128(0x22222222222222222222222222222222);
    let france_subscription_id = Uuid::from_u128(0x33333333333333333333333333333333);
    let profiles = vec![
        preview_profile(
            1,
            "🇳🇱 Netherlands",
            "nl.demo.example",
            ProtocolConfig::Vless(VlessConfig {
                uuid: preview_uuid(),
                ..Default::default()
            }),
            None,
        ),
        preview_profile(
            2,
            "🇨🇳 China",
            "cn.demo.example",
            ProtocolConfig::Vmess(VmessConfig {
                uuid: preview_uuid(),
                ..Default::default()
            }),
            None,
        ),
        preview_profile(
            3,
            "🇺🇸 United States",
            "us.demo.example",
            ProtocolConfig::Trojan(TrojanConfig {
                password: "demo".into(),
                ..Default::default()
            }),
            None,
        ),
        preview_profile(
            4,
            "🇨🇦 Canada",
            "ca.demo.example",
            ProtocolConfig::Hysteria2(Hysteria2Config {
                password: "demo".into(),
                ..Default::default()
            }),
            None,
        ),
        preview_profile(
            5,
            "🇩🇪 Germany",
            "de.demo.example",
            ProtocolConfig::Tuic(TuicConfig {
                uuid: preview_uuid(),
                password: "demo".into(),
                ..Default::default()
            }),
            None,
        ),
        preview_profile(
            6,
            "🇫🇮 Helsinki",
            "fi-1.demo.example",
            ProtocolConfig::Vless(VlessConfig {
                uuid: preview_uuid(),
                ..Default::default()
            }),
            Some(finland_subscription_id),
        ),
        preview_profile(
            7,
            "🇫🇮 Tampere",
            "fi-2.demo.example",
            ProtocolConfig::Vless(VlessConfig {
                uuid: preview_uuid(),
                ..Default::default()
            }),
            Some(finland_subscription_id),
        ),
        preview_profile(
            8,
            "🇫🇷 Paris",
            "fr-1.demo.example",
            ProtocolConfig::Hysteria2(Hysteria2Config {
                password: "demo".into(),
                ..Default::default()
            }),
            Some(france_subscription_id),
        ),
        preview_profile(
            9,
            "🇫🇷 Lyon",
            "fr-2.demo.example",
            ProtocolConfig::Hysteria2(Hysteria2Config {
                password: "demo".into(),
                ..Default::default()
            }),
            Some(france_subscription_id),
        ),
        preview_profile(
            10,
            "🇫🇷 Marseille",
            "fr-3.demo.example",
            ProtocolConfig::Hysteria2(Hysteria2Config {
                password: "demo".into(),
                ..Default::default()
            }),
            Some(france_subscription_id),
        ),
    ];
    let active_id = profiles[2].id;
    let mut config = Config {
        profiles,
        ..Default::default()
    };
    config.subscriptions.push(Subscription {
        id: finland_subscription_id,
        name: "🇫🇮 Finland VLESS".into(),
        url: "https://finland-subscription.demo.example/list".into(),
        auto_update: SubscriptionAutoUpdate::Every1d,
        last_updated: None,
        next_auto_update: None,
        retry_state: None,
        send_hwid: false,
        hwid: None,
    });
    config.subscriptions.push(Subscription {
        id: france_subscription_id,
        name: "🇫🇷 France Hysteria2".into(),
        url: "https://france-subscription.demo.example/list".into(),
        auto_update: SubscriptionAutoUpdate::Every7d,
        last_updated: None,
        next_auto_update: None,
        retry_state: None,
        send_hwid: false,
        hwid: None,
    });
    config.settings.theme = theme_slug.into();
    config.settings.auto_connect = true;
    config.settings.kill_switch = true;
    config.settings.geo_routing.set_region(GeoRegion::Ru);
    config.settings.geo_routing.set_mode(RoutingMode::Global);
    config.settings.geo_routing.auto_update = GeoAutoUpdate::Every3d;

    let mut model = Model::in_memory(config);
    model.theme = crate::ui::styles::Theme::from_palette(palette);
    model.connection = ConnectionState::Connected;
    model.active_profile_id = Some(active_id);
    model.selected = 2;
    model.main_pane_focus = crate::app::model::MainPaneFocus::Sources;
    model.status = AppStatus::Info("Connected to 🇺🇸 United States".into());
    model.geo_last_checked_at = Some(chrono::Local::now());
    model.traffic = TrafficStats {
        up_rate_bps: 731 * 1024,
        down_rate_bps: 5_033_165,
        up_total: 105 * 1024 * 1024,
        down_total: 822 * 1024 * 1024,
        conn_count: 25,
    };
    for line in [
        "[app] Starting connection to 🇺🇸 United States",
        "[sb] 17:43:01 INFO network: updated default interface wlp1s0, index 2",
        "[sb] 17:43:01 INFO inbound/tun[tun-in]: started at kvn0",
        "[sb] 17:43:02 INFO outbound/trojan[proxy]: connected to us.demo.example:443",
        "[app] Connection established; traffic statistics are live",
        "[sb] 17:43:03 INFO inbound/tun[tun-in]: inbound packet connection to 10.222.0.2:53",
        "[sb] 17:43:03 INFO dns: exchanged A docs.example. 291 IN A 192.0.2.10",
        "[sb] 17:43:04 INFO outbound/trojan[proxy]: outbound connection to 192.0.2.10:443",
        "[sb] 17:43:05 INFO inbound/tun[tun-in]: inbound packet connection to 198.51.100.20:443",
        "[sb] 17:43:05 INFO outbound/trojan[proxy]: outbound connection to 198.51.100.20:443",
        "[sb] 17:43:06 INFO dns: exchanged AAAA api.demo.example. 300 IN AAAA 2001:db8::20",
        "[sb] 17:43:07 INFO inbound/tun[tun-in]: inbound packet connection to [2001:db8::20]:443",
        "[sb] 17:43:07 INFO outbound/trojan[proxy]: outbound connection to [2001:db8::20]:443",
        "[sb] 17:43:08 INFO dns: exchanged A cdn.demo.example. 180 IN A 203.0.113.40",
        "[sb] 17:43:09 INFO inbound/tun[tun-in]: inbound packet connection to 203.0.113.40:443",
        "[sb] 17:43:09 INFO outbound/trojan[proxy]: outbound connection to 203.0.113.40:443",
        "[sb] 17:43:10 INFO dns: exchanged A updates.demo.example. 240 IN A 192.0.2.55",
        "[sb] 17:43:11 INFO outbound/trojan[proxy]: outbound connection to 192.0.2.55:443",
        "[sb] 17:43:12 INFO inbound/tun[tun-in]: inbound packet connection to 198.51.100.72:443",
        "[sb] 17:43:12 INFO outbound/trojan[proxy]: outbound connection to 198.51.100.72:443",
        "[sb] 17:43:13 INFO dns: exchanged A status.demo.example. 120 IN A 203.0.113.80",
        "[sb] 17:43:14 INFO outbound/trojan[proxy]: outbound connection to 203.0.113.80:443",
    ] {
        model.push_log(line.into());
    }

    let _terminal_session = TerminalSession::enter()?;
    apply_terminal_colors(
        model.theme.palette_foreground(),
        model.theme.palette_background(),
    );
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    loop {
        terminal.draw(|frame| crate::ui::draw(frame, &model))?;
        if event::poll(Duration::from_millis(250))?
            && let Event::Key(key) = event::read()?
            && (matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
                || (key.code == KeyCode::Char('c')
                    && key.modifiers.contains(KeyModifiers::CONTROL)))
        {
            return Ok(());
        }
    }
}

fn preview_profile(
    id: u128,
    name: &str,
    address: &str,
    config: crate::config::profile::ProtocolConfig,
    subscription_id: Option<uuid::Uuid>,
) -> crate::config::profile::Profile {
    crate::config::profile::Profile {
        id: uuid::Uuid::from_u128(id),
        name: name.into(),
        address: address.into(),
        port: 443,
        config,
        tags: Vec::new(),
        subscription_id,
    }
}

fn preview_uuid() -> String {
    "11111111-1111-1111-1111-111111111111".to_string()
}

/// Run the TUI client: connects to daemon, renders UI, forwards input.
pub fn run() -> Result<()> {
    let (mut client, initial_snapshot) = connect_to_current_daemon()?;

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
    apply_initial_snapshot(&mut model, initial_snapshot, &mut log_tailer);

    let _terminal_session = TerminalSession::enter()?;
    apply_terminal_colors(
        model.theme.palette_foreground(),
        model.theme.palette_background(),
    );
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

/// Attach to a daemon built from the same package as this client. Upgrades can
/// leave the previous daemon alive, so inspect the first response as generic
/// JSON before attempting to decode the full (possibly changed) snapshot.
fn connect_to_current_daemon() -> Result<(IpcClient, crate::app::msg::StateSnapshot)> {
    let mut reconnect_profile = None;

    for attempt in 0..=1 {
        let mut client = IpcClient::connect().context("Failed to connect to kvn-tui daemon")?;
        client.send(&IpcCommand::Attach)?;
        let value = client
            .read_snapshot_value(Duration::from_secs(2))
            .context("Daemon did not provide its initial state")?;

        let daemon_version = value
            .get("daemon_version")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let ipc_version = value
            .get("ipc_version")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default();
        let compatible = snapshot_is_compatible(&value);

        if compatible {
            let mut snapshot: crate::app::msg::StateSnapshot = serde_json::from_value(value)
                .context("Malformed state snapshot from the daemon")?;
            if snapshot.connection == ConnectionState::Idle
                && let Some(profile_id) = reconnect_profile
            {
                client.send(&IpcCommand::ConnectProfile { profile_id })?;
                snapshot = client
                    .read_snapshot(Duration::from_secs(2))
                    .context("Restarted daemon did not acknowledge reconnect")?;
            }
            return Ok((client, snapshot));
        }

        anyhow::ensure!(
            attempt == 0,
            "daemon is still incompatible after restart (daemon version {:?}, IPC {}; client version {:?}, IPC {})",
            daemon_version,
            ipc_version,
            env!("CARGO_PKG_VERSION"),
            crate::ipc::IPC_VERSION
        );

        reconnect_profile = reconnect_profile_after_restart(&value);

        if io::stderr().is_terminal() {
            eprintln!(
                "kvn-tui: restarting outdated daemon (version {})…",
                if daemon_version.is_empty() {
                    "unknown"
                } else {
                    daemon_version
                }
            );
        }
        client
            .send(&IpcCommand::Quit)
            .context("Failed to ask outdated daemon to stop")?;
        drop(client);
        anyhow::ensure!(
            crate::ipc::wait_for_daemon_exit(Duration::from_secs(5)),
            "outdated daemon did not stop within 5s"
        );
        crate::start_current_daemon().context("Failed to start updated daemon")?;
        anyhow::ensure!(
            crate::ipc::wait_for_daemon(Duration::from_secs(5)),
            "updated daemon did not start within 5s"
        );
    }

    unreachable!("daemon compatibility loop always returns or errors")
}

fn snapshot_is_compatible(value: &serde_json::Value) -> bool {
    value
        .get("daemon_version")
        .and_then(serde_json::Value::as_str)
        == Some(env!("CARGO_PKG_VERSION"))
        && value.get("ipc_version").and_then(serde_json::Value::as_u64)
            == Some(u64::from(crate::ipc::IPC_VERSION))
}

fn reconnect_profile_after_restart(value: &serde_json::Value) -> Option<uuid::Uuid> {
    if value.get("connection").and_then(serde_json::Value::as_str) != Some("Connected") {
        return None;
    }
    value
        .get("active_profile_id")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            value
                .pointer("/settings/last_connected_profile")
                .and_then(serde_json::Value::as_str)
        })
        .and_then(|id| uuid::Uuid::parse_str(id).ok())
}

fn apply_initial_snapshot(
    model: &mut Model,
    snapshot: crate::app::msg::StateSnapshot,
    log_tailer: &mut LogTailer,
) {
    if let Some(offsets) = snapshot.log_session_offsets {
        for line in log_tailer.load_history(
            &[offsets.app, offsets.singbox],
            crate::app::model::MAX_LOG_LINES,
        ) {
            model.push_log(line);
        }
    }
    apply_snapshot(model, snapshot);
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
    let mut toast = ToastState::new(model.status_revision);
    toast.show_initial_error(model.status.clone(), Instant::now());
    // Initial draw
    terminal.draw(|f| {
        crate::ui::layout::draw_with_toast(
            f,
            model,
            pane_focus,
            Some(&log_navigation),
            None,
            toast.current(),
            toast.show_over_overlay(),
        )
    })?;
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
                        let result =
                            self::editor::open_profiles_editor(target, model.config.clone());
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
                            Ok(edit) => {
                                client.send(&IpcCommand::ApplyEditedConfig {
                                    base: Box::new(edit.base),
                                    edited: Box::new(edit.edited),
                                })?;
                            }
                            Err(e) => {
                                // The live config was never opened by the
                                // editor. Route the snapshot error through
                                // the daemon so it survives the next state
                                // broadcast and appears in app.log.
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
                let toast_status = if snapshot.status_is_error {
                    AppStatus::Error(snapshot.status.clone())
                } else {
                    AppStatus::Info(snapshot.status.clone())
                };
                toast.observe(snapshot.status_revision, toast_status, Instant::now());
                apply_snapshot(model, *snapshot);
                if model.overlay != crate::app::model::Overlay::None {
                    log_selection = None;
                    log_dragging = false;
                }
                update_pointer_shape(terminal, model, mouse_position, &mut pointer_shape)?;
                needs_redraw = true;
            }
            Msg::IpcReadFailed(message) => anyhow::bail!(message),
            Msg::Tick => {
                let now = Instant::now();
                log_navigation.expire_if_idle(now);
                toast.expire(now);
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
                let previous = (
                    model.theme.palette_foreground(),
                    model.theme.palette_background(),
                );
                model.theme = theme;
                let current = (
                    model.theme.palette_foreground(),
                    model.theme.palette_background(),
                );
                if current != previous {
                    apply_terminal_colors(current.0, current.1);
                }
                needs_redraw = true;
            }
            _ => {}
        }

        if needs_redraw {
            terminal.draw(|f| {
                crate::ui::layout::draw_with_toast(
                    f,
                    model,
                    pane_focus,
                    Some(&log_navigation),
                    log_selection.as_ref(),
                    toast.current(),
                    toast.show_over_overlay(),
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
    model.status_revision = snapshot.status_revision;
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
    model.geo_last_checked_at = snapshot.geo_last_checked_at;
    model.service_checked_at = snapshot.service_checked_at;
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
    let previous = (
        model.theme.palette_foreground(),
        model.theme.palette_background(),
    );
    model.theme = theme_watch::resolve_active(effective_slug);
    let current = (
        model.theme.palette_foreground(),
        model.theme.palette_background(),
    );
    if current != previous {
        apply_terminal_colors(current.0, current.1);
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
    fn snapshot_compatibility_requires_matching_binary_and_ipc_versions() {
        let current = serde_json::json!({
            "daemon_version": env!("CARGO_PKG_VERSION"),
            "ipc_version": crate::ipc::IPC_VERSION,
        });
        assert!(snapshot_is_compatible(&current));
        assert!(!snapshot_is_compatible(&serde_json::json!({})));
        assert!(!snapshot_is_compatible(&serde_json::json!({
            "daemon_version": "0.0.0",
            "ipc_version": crate::ipc::IPC_VERSION,
        })));
        assert!(!snapshot_is_compatible(&serde_json::json!({
            "daemon_version": env!("CARGO_PKG_VERSION"),
            "ipc_version": crate::ipc::IPC_VERSION + 1,
        })));
    }

    #[test]
    fn restart_reconnects_only_the_profile_from_a_connected_snapshot() {
        let id = uuid::Uuid::new_v4();
        let connected = serde_json::json!({
            "connection": "Connected",
            "active_profile_id": id.to_string(),
        });
        assert_eq!(reconnect_profile_after_restart(&connected), Some(id));

        let idle = serde_json::json!({
            "connection": "Idle",
            "active_profile_id": id.to_string(),
        });
        assert_eq!(reconnect_profile_after_restart(&idle), None);
    }

    #[test]
    fn restart_reconnect_profile_falls_back_to_persisted_last_profile() {
        let id = uuid::Uuid::new_v4();
        let snapshot = serde_json::json!({
            "connection": "Connected",
            "active_profile_id": null,
            "settings": { "last_connected_profile": id.to_string() },
        });
        assert_eq!(reconnect_profile_after_restart(&snapshot), Some(id));
    }

    #[test]
    fn osc_color_formats_rgb_color_as_hex_triplet() {
        // Tokyo Night accent — typical RGB value.
        assert_eq!(
            osc_color(11, Color::Rgb(0x7a, 0xa2, 0xf7)),
            "\x1b]11;#7aa2f7\x1b\\"
        );
    }

    #[test]
    fn osc_color_formats_named_ansi_colors_via_to_rgb_table() {
        // Color::Black maps to (0, 0, 0) in palette::to_rgb.
        assert_eq!(osc_color(10, Color::Black), "\x1b]10;#000000\x1b\\");
    }

    #[test]
    fn osc_reset_colors_resets_foreground_and_background() {
        assert_eq!(OSC_RESET_COLORS, "\x1b]110\x1b\\\x1b]111\x1b\\");
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

    #[test]
    fn toast_state_treats_repeated_text_as_a_new_revision() {
        let start = Instant::now();
        let mut toast = ToastState::new(4);
        toast.observe(5, AppStatus::Info("Saved".into()), start);
        let first_deadline = toast.expires_at.unwrap();
        toast.observe(
            6,
            AppStatus::Info("Saved".into()),
            start + Duration::from_secs(1),
        );
        assert_eq!(toast.current().map(AppStatus::text), Some("Saved"));
        assert!(toast.expires_at.unwrap() > first_deadline);
    }

    #[test]
    fn toast_state_ignores_duplicate_snapshots_and_expires() {
        let start = Instant::now();
        let mut toast = ToastState::new(2);
        toast.observe(3, AppStatus::Error("Failed".into()), start);
        let deadline = toast.expires_at.unwrap();
        toast.observe(
            3,
            AppStatus::Info("stale".into()),
            start + Duration::from_secs(1),
        );
        assert_eq!(toast.current().map(AppStatus::text), Some("Failed"));
        toast.expire(deadline);
        assert!(toast.current().is_none());
    }

    #[test]
    fn toast_state_suppresses_empty_and_help_messages() {
        let start = Instant::now();
        let mut toast = ToastState::new(0);
        toast.observe(1, AppStatus::Info(String::new()), start);
        assert!(toast.current().is_none());
        toast.observe(2, AppStatus::Info("Press ? for help".into()), start);
        assert!(toast.current().is_none());
    }

    #[test]
    fn toast_state_shows_only_initial_errors_over_an_overlay() {
        let start = Instant::now();
        let mut toast = ToastState::new(2);

        toast.show_initial_error(AppStatus::Info("Connected".into()), start);
        assert!(toast.current().is_none());
        assert!(!toast.show_over_overlay());

        toast.show_initial_error(AppStatus::Error("Startup failed".into()), start);
        assert_eq!(toast.current().map(AppStatus::text), Some("Startup failed"));
        assert!(toast.show_over_overlay());

        toast.observe(3, AppStatus::Info("Recovered".into()), start);
        assert_eq!(toast.current().map(AppStatus::text), Some("Recovered"));
        assert!(!toast.show_over_overlay());
    }
}
