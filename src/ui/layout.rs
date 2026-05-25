use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, AppMode};
use crate::ui::styles::Theme;
use crate::ui::widgets::{ProfileList, StatusBar};

/// Render the full application UI into the terminal frame.
pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();

    // Main vertical layout: content + status bar
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);

    draw_main(frame, app, chunks[0]);
    draw_status_bar(frame, app, chunks[1]);

    match app.mode {
        AppMode::Help => draw_help(frame, area),
        AppMode::ConfirmDelete => draw_confirm_delete(frame, area),
        AppMode::ConfirmQuit => draw_confirm_quit(frame, area),
        AppMode::Error => draw_error(frame, area, app.status.text()),
        AppMode::CreateProfile => draw_input_modal(frame, app, area),
        AppMode::PasteUri => draw_paste_uri(frame, app, area),
        AppMode::RoutingMode => draw_routing_mode(frame, app, area),
        _ => {}
    }
}

/// Draw the main content area with the profile list and logs.
fn draw_main(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let profile_list = ProfileList::new(app);
    frame.render_widget(profile_list, chunks[0]);

    let log_block = Block::default()
        .title(" Logs ")
        .borders(Borders::ALL)
        .border_style(Theme::border());

    // Show the most recent log lines that fit in the available area.
    let available_height = chunks[1].height.saturating_sub(2) as usize;
    let start = app.logs.len().saturating_sub(available_height);
    let log_text: Vec<Line> = app
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
fn draw_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let status = StatusBar::new(app);
    frame.render_widget(status, area);
}

/// Draw the help popup overlay.
fn draw_help(frame: &mut Frame, area: Rect) {
    draw_modal(
        frame,
        area,
        " Help ",
        vec![
            Line::from(Span::styled("Key Bindings", Theme::accent())),
            Line::from(""),
            Line::from("j / Down    Move down"),
            Line::from("k / Up      Move up"),
            Line::from("g           Go to first"),
            Line::from("G           Go to last"),
            Line::from("Enter       Connect to selected"),
            Line::from("p           Paste from clipboard"),
            Line::from("n           New profile"),
            Line::from("d           Delete profile"),
            Line::from("m           Routing mode (popup list)"),
            Line::from("u           Update geoip/geosite databases"),
            Line::from("e           Open profiles.json in $EDITOR"),
            Line::from("r           Reconnect"),
            Line::from("s           Stop / disconnect"),
            Line::from("q / Esc     Quit"),
            Line::from("?           Show this help"),
        ],
    );
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

/// Draw the input modal for pasting a URI manually.
fn draw_paste_uri(frame: &mut Frame, app: &App, area: Rect) {
    draw_modal(
        frame,
        area,
        " Paste URI ",
        vec![
            Line::from(Span::styled("Paste VPN URI", Theme::accent())),
            Line::from(""),
            Line::from(app.input_buffer.as_str()),
            Line::from(""),
            Line::from("Enter to confirm, Esc to cancel"),
        ],
    );
}

/// Draw the input modal for creating or editing a profile.
fn draw_input_modal(frame: &mut Frame, app: &App, area: Rect) {
    let label = app.input_field.label();

    let title = if app.mode == AppMode::CreateProfile {
        " New Profile "
    } else {
        " Edit Profile "
    };

    draw_modal(
        frame,
        area,
        title,
        vec![
            Line::from(Span::styled(label, Theme::accent())),
            Line::from(""),
            Line::from(app.input_buffer.as_str()),
            Line::from(""),
            Line::from("Enter to confirm, Esc to cancel"),
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
fn draw_routing_mode(frame: &mut Frame, app: &App, area: Rect) {
    use crate::config::profile::RoutingMode;
    use ratatui::style::Modifier;
    use ratatui::text::Span;

    let modes = RoutingMode::ALL;
    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled("Select routing mode", Theme::accent())),
        Line::from(""),
    ];

    for (i, mode) in modes.iter().enumerate() {
        let marker = if i == app.routing_selected {
            "> "
        } else {
            "  "
        };
        let text = format!("{}{}", marker, mode.as_str());
        let style = if i == app.routing_selected {
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
}
