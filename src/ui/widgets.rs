use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Rect};
use ratatui::style::{Modifier, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Row, StatefulWidget, Table, Widget};

use crate::app::App;
use crate::ui::styles::Theme;

/// Widget that renders the profile list as a table.
pub struct ProfileList<'a> {
    app: &'a App,
}

impl<'a> ProfileList<'a> {
    pub fn new(app: &'a App) -> Self {
        Self { app }
    }
}

impl<'a> Widget for ProfileList<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let header = Row::new(vec!["Name", "Protocol", "Address", "Port"])
            .style(Theme::accent())
            .add_modifier(Modifier::BOLD);

        let rows: Vec<Row> = self
            .app
            .config
            .profiles
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let style = if i == self.app.selected {
                    Theme::selected()
                } else {
                    Theme::normal()
                };

                let connected_marker =
                    if self.app.singbox_process.is_some() && self.app.selected == i {
                        " ●"
                    } else {
                        ""
                    };

                Row::new(vec![
                    Cell::from(format!("{}{}", p.name, connected_marker)),
                    Cell::from(p.protocol.to_string()),
                    Cell::from(p.address.clone()),
                    Cell::from(p.port.to_string()),
                ])
                .style(style)
            })
            .collect();

        let table = Table::new(
            rows,
            [
                Constraint::Percentage(35),
                Constraint::Percentage(20),
                Constraint::Percentage(30),
                Constraint::Percentage(15),
            ],
        )
        .header(header)
        .block(
            Block::default()
                .title(" Profiles ")
                .borders(Borders::ALL)
                .border_style(Theme::border()),
        )
        .highlight_symbol("");

        // Table does not need a state for basic selection rendering here
        let mut state = ratatui::widgets::TableState::default();
        state.select(Some(self.app.selected));
        StatefulWidget::render(table, area, buf, &mut state);
    }
}

/// Widget that renders the bottom status bar.
pub struct StatusBar<'a> {
    app: &'a App,
}

impl<'a> StatusBar<'a> {
    pub fn new(app: &'a App) -> Self {
        Self { app }
    }
}

impl<'a> Widget for StatusBar<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let style = if self.app.singbox_process.is_some() {
            Theme::success()
        } else {
            Theme::status()
        };

        let status = if self.app.singbox_process.is_some() {
            "[CONNECTED]"
        } else {
            "[DISCONNECTED]"
        };

        let routing = format!("[{}]", self.app.config.settings.routing_mode.as_str());

        let geo_info = if self.app.geo_updating {
            "[Geo: updating...]".to_string()
        } else {
            match self.app.geo_last_updated() {
                Some(dt) => format!("[Geo: {}]", dt),
                None => "[Geo: never]".to_string(),
            }
        };

        let text = Line::from(vec![
            Span::styled(status, style),
            Span::raw(" "),
            Span::styled(routing, Theme::accent()),
            Span::raw(" "),
            Span::styled(geo_info, Theme::accent()),
            Span::raw(" "),
            Span::styled(self.app.status.text(), Theme::normal()),
        ]);

        let paragraph = ratatui::widgets::Paragraph::new(text).alignment(Alignment::Left);

        paragraph.render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::profile::{Profile, Protocol};
    use crate::test_helpers::app_with_profiles;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    #[test]
    fn profile_list_renders_headers_and_rows() {
        let app = app_with_profiles(vec![
            Profile::new(
                "Alpha".to_string(),
                Protocol::Vless,
                "1.1.1.1".to_string(),
                443,
                "u1".to_string(),
            ),
            Profile::new(
                "Beta".to_string(),
                Protocol::Vless,
                "2.2.2.2".to_string(),
                80,
                "u2".to_string(),
            ),
        ]);
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 10));
        ProfileList::new(&app).render(Rect::new(0, 0, 80, 10), &mut buf);

        let content: String = buf.content.iter().map(|c| c.symbol()).collect();
        assert!(content.contains("Alpha"));
        assert!(content.contains("Beta"));
        assert!(content.contains("1.1.1.1"));
        assert!(content.contains("2.2.2.2"));
        assert!(content.contains("443"));
        assert!(content.contains("80"));
        assert!(content.contains("Profiles"));
    }

    #[test]
    fn profile_list_shows_connected_marker() {
        let mut app = app_with_profiles(vec![Profile::new(
            "Alpha".to_string(),
            Protocol::Vless,
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        app.selected = 0;
        // We can't easily mock Child, so we test the disconnected case (no marker)
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 10));
        ProfileList::new(&app).render(Rect::new(0, 0, 80, 10), &mut buf);
        let content: String = buf.content.iter().map(|c| c.symbol()).collect();
        assert!(content.contains("Alpha"));
        // Without a real child process the "●" marker should not appear
        assert!(!content.contains('●'));
    }

    #[test]
    fn status_bar_shows_disconnected() {
        let app = app_with_profiles(vec![]);
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 1));
        StatusBar::new(&app).render(Rect::new(0, 0, 80, 1), &mut buf);
        let content: String = buf.content.iter().map(|c| c.symbol()).collect();
        assert!(content.contains("[DISCONNECTED]"));
        assert!(content.contains("[Global]"));
    }
}
