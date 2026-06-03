use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Rect};
use ratatui::style::{Modifier, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Row, StatefulWidget, Table, Widget};

use crate::app::model::Model;
use crate::ui::styles::Theme;

/// Widget that renders the profile list as a table.
pub struct ProfileList<'a> {
    model: &'a Model,
}

impl<'a> ProfileList<'a> {
    pub fn new(model: &'a Model) -> Self {
        Self { model }
    }
}

impl<'a> Widget for ProfileList<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let header = Row::new(vec!["Name", "Protocol", "Address", "Port"])
            .style(Theme::accent())
            .add_modifier(Modifier::BOLD);

        let rows: Vec<Row> = self
            .model
            .config
            .profiles
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let style = if i == self.model.selected {
                    Theme::selected()
                } else {
                    Theme::normal()
                };

                let connected_marker = if self.model.active_profile_id == Some(p.id) {
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
        state.select(Some(self.model.selected));
        StatefulWidget::render(table, area, buf, &mut state);
    }
}

/// Widget that renders the bottom status bar.
pub struct StatusBar<'a> {
    model: &'a Model,
}

impl<'a> StatusBar<'a> {
    pub fn new(model: &'a Model) -> Self {
        Self { model }
    }
}

impl<'a> Widget for StatusBar<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let style = if self.model.connection == crate::app::model::ConnectionState::Connected {
            Theme::success()
        } else {
            Theme::status()
        };

        let status = if self.model.connection == crate::app::model::ConnectionState::Connected {
            "[CONNECTED]"
        } else {
            "[DISCONNECTED]"
        };

        let routing = format!("[{}]", self.model.config.settings.routing_mode.as_str());

        let geo_info = if self.model.geo_updating {
            "[Geo: updating...]".to_string()
        } else {
            match crate::geo::GeoManager::new()
                .ok()
                .and_then(|g| g.last_updated())
            {
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
            Span::styled(self.model.status.text(), Theme::normal()),
        ]);

        let paragraph = ratatui::widgets::Paragraph::new(text).alignment(Alignment::Left);

        paragraph.render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::model::ConnectionState;
    use crate::config::profile::{Profile, Protocol};
    use crate::test_helpers::{buffer_to_string, ensure_fixed_geo, model_with_profiles};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    #[test]
    fn profile_list_renders_headers_and_rows() {
        let model = model_with_profiles(vec![
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
        ProfileList::new(&model).render(Rect::new(0, 0, 80, 10), &mut buf);

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
        let mut model = model_with_profiles(vec![Profile::new(
            "Alpha".to_string(),
            Protocol::Vless,
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        model.selected = 0;

        // No active profile — marker should not appear
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 10));
        ProfileList::new(&model).render(Rect::new(0, 0, 80, 10), &mut buf);
        let content: String = buf.content.iter().map(|c| c.symbol()).collect();
        assert!(content.contains("Alpha"));
        assert!(!content.contains('●'));

        // Set active profile — marker should appear
        let id = model.config.profiles[0].id;
        model.active_profile_id = Some(id);
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 10));
        ProfileList::new(&model).render(Rect::new(0, 0, 80, 10), &mut buf);
        let content: String = buf.content.iter().map(|c| c.symbol()).collect();
        assert!(content.contains('●'));
    }

    #[test]
    fn profile_list_connected_marker_stays_on_active_profile() {
        let mut model = model_with_profiles(vec![
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
        // Active profile is Alpha, but selection is on Beta
        model.active_profile_id = Some(model.config.profiles[0].id);
        model.selected = 1;

        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 10));
        ProfileList::new(&model).render(Rect::new(0, 0, 80, 10), &mut buf);
        let content: String = buf.content.iter().map(|c| c.symbol()).collect();

        // "●" should appear next to Alpha, not Beta
        let alpha_pos = content.find("Alpha").unwrap();
        let beta_pos = content.find("Beta").unwrap();
        let bullet_pos = content.find('●').unwrap();

        assert!(
            bullet_pos > alpha_pos && bullet_pos < beta_pos,
            "bullet should be on Alpha, not Beta"
        );
    }

    #[test]
    fn status_bar_shows_disconnected() {
        let model = model_with_profiles(vec![]);
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 1));
        StatusBar::new(&model).render(Rect::new(0, 0, 80, 1), &mut buf);
        let content: String = buf.content.iter().map(|c| c.symbol()).collect();
        assert!(content.contains("[DISCONNECTED]"));
        assert!(content.contains("[Global]"));
    }

    #[test]
    fn status_bar_connected_snapshot() {
        ensure_fixed_geo();
        let mut model = model_with_profiles(vec![Profile::new(
            "Alpha".to_string(),
            Protocol::Vless,
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        model.connection = ConnectionState::Connected;
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 1));
        StatusBar::new(&model).render(Rect::new(0, 0, 80, 1), &mut buf);
        insta::assert_snapshot!(buffer_to_string(&buf));
    }

    #[test]
    fn status_bar_geo_updating_snapshot() {
        ensure_fixed_geo();
        let mut model = model_with_profiles(vec![]);
        model.geo_updating = true;
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 1));
        StatusBar::new(&model).render(Rect::new(0, 0, 80, 1), &mut buf);
        insta::assert_snapshot!(buffer_to_string(&buf));
    }

    #[test]
    fn profile_list_snapshot() {
        ensure_fixed_geo();
        let model = model_with_profiles(vec![
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
        ProfileList::new(&model).render(Rect::new(0, 0, 80, 10), &mut buf);
        insta::assert_snapshot!(buffer_to_string(&buf));
    }
}
