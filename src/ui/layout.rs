use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Row, Table, Wrap};

use crate::app::model::{Model, Overlay};
use crate::ui::styles::Theme;
use crate::ui::widgets::{ProfileList, StatusBar};

/// Render the full application UI into the terminal frame.
pub fn draw(frame: &mut Frame, model: &Model) {
    let area = frame.area();

    // Main vertical layout: content + status bar
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);

    draw_main(frame, model, chunks[0]);
    draw_status_bar(frame, model, chunks[1]);

    match model.overlay {
        Overlay::Help => draw_help(frame, area),
        Overlay::ConfirmDelete => draw_confirm_delete(frame, area),
        Overlay::ConfirmQuit => draw_confirm_quit(frame, area),
        Overlay::Error => draw_error(frame, area, model.status.text()),
        Overlay::RoutingMode => draw_routing_mode(frame, model, area),
        Overlay::None => {}
    }
}

/// Draw the main content area with the profile list and logs.
fn draw_main(frame: &mut Frame, model: &Model, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let profile_list = ProfileList::new(model);
    frame.render_widget(profile_list, chunks[0]);

    let log_block = Block::default()
        .title(" Logs ")
        .borders(Borders::ALL)
        .border_style(Theme::border());

    // Show the most recent log lines that fit in the available area.
    let available_height = chunks[1].height.saturating_sub(2) as usize;
    let start = model.logs.len().saturating_sub(available_height);
    let log_text: Vec<Line> = model
        .logs
        .iter()
        .skip(start)
        .map(|l| Line::from(Span::styled(l.as_str(), Theme::normal())))
        .collect();

    let logs = Paragraph::new(log_text)
        .block(log_block)
        .wrap(Wrap { trim: true });
    frame.render_widget(logs, chunks[1]);
}

/// Draw the bottom status bar.
fn draw_status_bar(frame: &mut Frame, model: &Model, area: Rect) {
    let status = StatusBar::new(model);
    frame.render_widget(status, area);
}

/// Draw the help popup overlay.
fn draw_help(frame: &mut Frame, area: Rect) {
    let popup_area = centered_rect(POPUP_WIDTH_PERCENT, POPUP_HEIGHT_PERCENT, area);

    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .title(" Help ")
        .borders(Borders::ALL)
        .border_style(Theme::border())
        .style(Theme::popup_bg());

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let header =
        Row::new(vec!["Key", "Action"]).style(Theme::accent().add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = vec![
        Row::new(vec!["j / Down", "Move down"]),
        Row::new(vec!["k / Up", "Move up"]),
        Row::new(vec!["g", "Go to first"]),
        Row::new(vec!["G", "Go to last"]),
        Row::new(vec!["Enter", "Connect to selected"]),
        Row::new(vec!["p", "Paste from clipboard"]),
        Row::new(vec!["d", "Delete profile"]),
        Row::new(vec!["m", "Routing mode (popup list)"]),
        Row::new(vec!["u", "Update geoip/geosite databases"]),
        Row::new(vec!["e", "Open profiles.json in $EDITOR"]),
        Row::new(vec!["a", "Toggle auto-connect"]),
        Row::new(vec!["r", "Reconnect"]),
        Row::new(vec!["s", "Stop / disconnect"]),
        Row::new(vec!["q / Esc", "Quit"]),
        Row::new(vec!["?", "Show this help"]),
    ];

    let table = Table::new(rows, [Constraint::Length(12), Constraint::Min(1)]).header(header);

    frame.render_widget(table, inner);
}

/// Draw the delete confirmation dialog.
fn draw_confirm_delete(frame: &mut Frame, area: Rect) {
    draw_modal(
        frame,
        area,
        " Confirm ",
        vec![
            Line::from(Span::styled("Delete selected profile?", Theme::error())),
            Line::from(""),
            Line::from("Press y to confirm, n to cancel"),
        ],
    );
}

/// Draw the quit confirmation dialog when a VPN connection is active.
fn draw_confirm_quit(frame: &mut Frame, area: Rect) {
    draw_modal(
        frame,
        area,
        " Confirm Quit ",
        vec![
            Line::from(Span::styled(
                "A VPN connection is still active.",
                Theme::error(),
            )),
            Line::from("Are you sure you want to quit?"),
            Line::from(""),
            Line::from("Press y to confirm, n to cancel"),
        ],
    );
}

/// Draw an error message popup.
fn draw_error(frame: &mut Frame, area: Rect, message: &str) {
    draw_modal(
        frame,
        area,
        " Error ",
        vec![
            Line::from(Span::styled("Error", Theme::error())),
            Line::from(""),
            Line::from(message),
            Line::from(""),
            Line::from("Press any key to dismiss"),
        ],
    );
}

const POPUP_WIDTH_PERCENT: u16 = 60;
const POPUP_HEIGHT_PERCENT: u16 = 50;

/// Helper to render a centered popup with a border and text.
fn draw_modal(frame: &mut Frame, area: Rect, title: &str, lines: Vec<Line>) {
    let popup_area = centered_rect(POPUP_WIDTH_PERCENT, POPUP_HEIGHT_PERCENT, area);

    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Theme::border())
        .style(Theme::popup_bg());

    let paragraph = Paragraph::new(lines)
        .block(block)
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true });

    frame.render_widget(paragraph, popup_area);
}

/// Draw the routing mode selection modal.
fn draw_routing_mode(frame: &mut Frame, model: &Model, area: Rect) {
    use crate::config::profile::RoutingMode;
    use ratatui::style::Modifier;
    use ratatui::text::Span;

    let modes = RoutingMode::ALL;
    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled("Select routing mode", Theme::accent())),
        Line::from(""),
    ];

    for (i, mode) in modes.iter().enumerate() {
        let marker = if i == model.routing_selected {
            "> "
        } else {
            "  "
        };
        let text = format!("{}{}", marker, mode.as_str());
        let style = if i == model.routing_selected {
            Theme::accent().add_modifier(Modifier::BOLD)
        } else {
            Theme::normal()
        };
        lines.push(Line::from(Span::styled(text, style)));
    }

    lines.push(Line::from(""));
    lines.push(Line::from("j/k navigate, Enter confirm, Esc cancel"));

    draw_modal(frame, area, " Routing Mode ", lines);
}

/// Compute a centered rectangle with given percentage sizes.
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::model::{ConnectionState, Overlay};
    use crate::config::profile::{Profile, Protocol};
    use crate::test_helpers::{buffer_to_string, ensure_fixed_geo, model_with_profiles};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn snapshot_terminal(model: &Model, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let frame = terminal.draw(|f| draw(f, model)).unwrap();
        buffer_to_string(frame.buffer)
    }

    #[test]
    fn centered_rect_60_50_in_100_100() {
        let area = Rect::new(0, 0, 100, 100);
        let popup = centered_rect(POPUP_WIDTH_PERCENT, POPUP_HEIGHT_PERCENT, area);
        assert_eq!(popup.x, 20);
        assert_eq!(popup.y, 25);
        assert_eq!(popup.width, 60);
        assert_eq!(popup.height, 50);
    }

    #[test]
    fn centered_rect_100_100_fills_area() {
        let area = Rect::new(10, 20, 80, 40);
        let popup = centered_rect(100, 100, area);
        assert_eq!(popup.x, 10);
        assert_eq!(popup.y, 20);
        assert_eq!(popup.width, 80);
        assert_eq!(popup.height, 40);
    }

    #[test]
    fn centered_rect_zero_area() {
        let area = Rect::new(0, 0, 0, 0);
        let popup = centered_rect(50, 50, area);
        assert_eq!(popup.width, 0);
        assert_eq!(popup.height, 0);
    }

    #[test]
    fn help_renders_commands() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(80, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let frame = terminal
            .draw(|frame| {
                let area = frame.area();
                draw_help(frame, area);
            })
            .unwrap();

        let content: String = frame.buffer.content.iter().map(|c| c.symbol()).collect();
        let expected = [
            ("j / Down", "Move down"),
            ("k / Up", "Move up"),
            ("g", "Go to first"),
            ("G", "Go to last"),
            ("Enter", "Connect to selected"),
            ("p", "Paste from clipboard"),
            ("d", "Delete profile"),
            ("m", "Routing mode (popup list)"),
            ("u", "Update geoip/geosite databases"),
            ("e", "Open profiles.json in $EDITOR"),
            ("r", "Reconnect"),
            ("s", "Stop / disconnect"),
            ("q / Esc", "Quit"),
            ("?", "Show this help"),
        ];
        for (key, action) in expected {
            assert!(content.contains(key), "help should contain key: {}", key);
            assert!(
                content.contains(action),
                "help should contain action: {}",
                action
            );
        }
        assert!(content.contains("Help"), "should contain Help title");
    }

    #[test]
    fn draw_main_snapshot() {
        ensure_fixed_geo();
        let mut model = model_with_profiles(vec![Profile::new(
            "Alpha".to_string(),
            Protocol::Vless,
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        model.logs.push_back("log line 1".to_string());
        model.logs.push_back("log line 2".to_string());
        model.connection = ConnectionState::Connected;
        model.active_profile_id = Some(model.config.profiles[0].id);
        insta::assert_snapshot!(snapshot_terminal(&model, 80, 20));
    }

    #[test]
    fn draw_help_overlay_snapshot() {
        ensure_fixed_geo();
        let mut model = model_with_profiles(vec![]);
        model.overlay = Overlay::Help;
        insta::assert_snapshot!(snapshot_terminal(&model, 80, 40));
    }

    #[test]
    fn draw_confirm_delete_overlay_snapshot() {
        ensure_fixed_geo();
        let mut model = model_with_profiles(vec![Profile::new(
            "Alpha".to_string(),
            Protocol::Vless,
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        model.overlay = Overlay::ConfirmDelete;
        insta::assert_snapshot!(snapshot_terminal(&model, 80, 20));
    }

    #[test]
    fn draw_confirm_quit_overlay_snapshot() {
        ensure_fixed_geo();
        let mut model = model_with_profiles(vec![]);
        model.connection = ConnectionState::Connected;
        model.overlay = Overlay::ConfirmQuit;
        insta::assert_snapshot!(snapshot_terminal(&model, 80, 20));
    }

    #[test]
    fn draw_error_overlay_snapshot() {
        ensure_fixed_geo();
        let mut model = model_with_profiles(vec![]);
        model.overlay = Overlay::Error;
        model.status = crate::app::model::AppStatus::Error("something went wrong".to_string());
        insta::assert_snapshot!(snapshot_terminal(&model, 80, 20));
    }

    #[test]
    fn draw_routing_mode_overlay_snapshot() {
        ensure_fixed_geo();
        let mut model = model_with_profiles(vec![]);
        model.overlay = Overlay::RoutingMode;
        model.routing_selected = 2;
        insta::assert_snapshot!(snapshot_terminal(&model, 80, 20));
    }
}
