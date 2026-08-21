use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use crate::app::model::Model;
use crate::config::profile::GeoRegion;

/// Widget that renders the bottom status bar.
pub struct StatusBar<'a> {
    model: &'a Model,
}

impl<'a> StatusBar<'a> {
    pub fn new(model: &'a Model) -> Self {
        Self { model }
    }
}

/// Render a bytes-per-second rate as a short human-readable string.
/// Uses 1024-based units (KiB-style) to match typical bandwidth-monitor
/// conventions. Rates always use at least KB/s so the unit stays aligned.
/// Examples: `0 → "0.0 KB/s"`, `1024 → "1.0 KB/s"`,
/// `1_500_000 → "1.4 MB/s"`.
pub fn format_bps(bps: u64) -> String {
    format_quantity(bps, true)
}

fn format_quantity(bytes: u64, per_second: bool) -> String {
    const UNITS: [&str; 7] = ["B", "KB", "MB", "GB", "TB", "PB", "EB"];

    let (mut value, mut unit_index) = (bytes as f64 / 1024.0, 1);
    while value >= 999.5 && unit_index < UNITS.len() - 1 {
        value /= 1024.0;
        unit_index += 1;
    }

    let number = if value < 9.95 {
        format!("{value:.1}")
    } else {
        format!("{value:.0}")
    };
    let suffix = if per_second { "/s" } else { "" };
    format!("{number} {}{suffix}", UNITS[unit_index])
}

/// Render a cumulative byte count as a short human-readable string (no `/s`
/// suffix). Totals always use at least KB. Examples: `0 → "0.0 KB"`,
/// `1_500_000 → "1.4 MB"`.
pub fn format_bytes(bytes: u64) -> String {
    format_quantity(bytes, false)
}

const TRAFFIC_RATE_WIDTH: usize = 8;
const TRAFFIC_TOTAL_WIDTH: usize = 6;
const TRAFFIC_CONNECTIONS_COUNT_WIDTH: usize = 6;

fn fixed_width_field(value: String, width: usize, overflow: &str) -> String {
    let value = if value.len() > width {
        overflow
    } else {
        &value
    };
    format!("{value:>width$}")
}

/// Format a traffic rate into the fixed-width column used by the traffic panel.
pub fn format_bps_field(bps: u64) -> String {
    fixed_width_field(format_bps(bps), TRAFFIC_RATE_WIDTH, ">9 EB/s")
}

/// Format a cumulative traffic total into the fixed-width traffic panel column.
pub fn format_bytes_field(bytes: u64) -> String {
    fixed_width_field(format_bytes(bytes), TRAFFIC_TOTAL_WIDTH, ">9 EB")
}

/// Format the active connection count without allowing its column to grow.
pub fn format_connections_field(connections: usize) -> String {
    if connections > 99_999 {
        "99999+".to_string()
    } else {
        connections.to_string()
    }
}

pub fn format_connections_padding(connections: usize) -> String {
    " ".repeat(TRAFFIC_CONNECTIONS_COUNT_WIDTH - format_connections_field(connections).len())
}

/// Truncate a string to fit within a visual width, appending "..." if truncated.
fn truncate_to_width(s: &str, max_width: usize) -> String {
    if max_width <= 3 {
        return ".".repeat(max_width);
    }
    let mut result = String::with_capacity(max_width);
    for (len, ch) in s.chars().enumerate() {
        if len + 1 > max_width.saturating_sub(3) {
            result.push_str("...");
            return result;
        }
        result.push(ch);
    }
    result
}

impl<'a> Widget for StatusBar<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let (status, style) = match self.model.connection {
            crate::app::model::ConnectionState::Connected => {
                ("[CONNECTED]", self.model.theme.success())
            }
            crate::app::model::ConnectionState::ConnectPending => {
                ("[CONNECTING]", self.model.theme.status())
            }
            _ => ("[DISCONNECTED]", self.model.theme.error()),
        };

        let routing = format!("[{}]", self.model.config.settings.geo_routing.mode());

        let is_global =
            self.model.config.settings.geo_routing.current_region == Some(GeoRegion::Global);

        let mut spans = vec![Span::styled(status, style)];

        if self.model.config.settings.auto_connect {
            spans.push(Span::raw(" "));
            spans.push(Span::styled("[AUTO]", self.model.theme.accent()));
        }

        if self.model.config.settings.kill_switch {
            spans.push(Span::raw(" "));
            spans.push(Span::styled("[KS]", self.model.theme.accent()));
        }

        let dns = &self.model.config.settings.dns;
        let dns_label = if dns.fakeip_enabled {
            "fakeip".to_string()
        } else {
            dns.final_server_entry()
                .map(|s| s.kind_label().to_string())
                .unwrap_or_else(|| "?".to_string())
        };
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("[DNS: {}]", dns_label),
            self.model.theme.accent(),
        ));

        spans.push(Span::raw(" "));
        spans.push(Span::styled(routing, self.model.theme.accent()));

        if !is_global {
            let geo_info = if self.model.geo_updating {
                "[Geo: updating...]".to_string()
            } else {
                match self.model.geo_last_updated {
                    Some(ref dt) => format!("[Geo: {}]", dt),
                    None => "[Geo: never]".to_string(),
                }
            };
            spans.push(Span::raw(" "));
            spans.push(Span::styled(geo_info, self.model.theme.accent()));
        }

        spans.push(Span::raw(" "));

        let fixed_width: usize = spans.iter().map(|s| s.content.len()).sum();
        let available = area.width as usize;
        let status_text = self.model.status.text();
        let max_status = available.saturating_sub(fixed_width);
        let truncated = truncate_to_width(status_text, max_status);

        spans.push(Span::styled(truncated, self.model.theme.normal()));

        let text = Line::from(spans);

        let paragraph = Paragraph::new(text).alignment(Alignment::Left);

        paragraph.render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::model::ConnectionState;
    use crate::config::profile::Profile;
    use crate::test_helpers::{buffer_to_string, model_with_profiles};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    #[test]
    fn status_bar_shows_disconnected() {
        let model = model_with_profiles(vec![]);
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 1));
        StatusBar::new(&model).render(Rect::new(0, 0, 80, 1), &mut buf);
        let content: String = buf.content.iter().map(|c| c.symbol()).collect();
        assert!(content.contains("[DISCONNECTED]"));
        assert!(content.contains("[Global]"));
        // Verify red color for disconnected
        let idx = content.find('[').unwrap();
        assert_eq!(
            buf.content[idx].style().fg,
            Some(ratatui::style::Color::Red)
        );
    }

    #[test]
    fn status_bar_connect_pending_shows_connecting() {
        use crate::app::model::ConnectionState;
        let mut model = model_with_profiles(vec![]);
        model.connection = ConnectionState::ConnectPending;
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 1));
        StatusBar::new(&model).render(Rect::new(0, 0, 80, 1), &mut buf);
        let content: String = buf.content.iter().map(|c| c.symbol()).collect();
        assert!(content.contains("[CONNECTING]"));
        // Verify yellow color for connecting
        let idx = content.find('[').unwrap();
        assert_eq!(
            buf.content[idx].style().fg,
            Some(ratatui::style::Color::Yellow)
        );
    }

    #[test]
    fn status_bar_connected_snapshot() {
        let mut model = model_with_profiles(vec![Profile::new_vless(
            "Alpha".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        model.connection = ConnectionState::Connected;
        model.geo_last_updated = Some("2026-05-31 13:41".to_string());
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 1));
        StatusBar::new(&model).render(Rect::new(0, 0, 80, 1), &mut buf);
        insta::assert_snapshot!(buffer_to_string(&buf));
    }

    #[test]
    fn status_bar_connected_auto_snapshot() {
        let mut model = model_with_profiles(vec![Profile::new_vless(
            "Alpha".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        model.connection = ConnectionState::Connected;
        model.config.settings.auto_connect = true;
        model.geo_last_updated = Some("2026-05-31 13:41".to_string());
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 1));
        StatusBar::new(&model).render(Rect::new(0, 0, 80, 1), &mut buf);
        insta::assert_snapshot!(buffer_to_string(&buf));
    }

    #[test]
    fn status_bar_geo_updating_snapshot() {
        let mut model = model_with_profiles(vec![]);
        model.geo_updating = true;
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 1));
        StatusBar::new(&model).render(Rect::new(0, 0, 80, 1), &mut buf);
        insta::assert_snapshot!(buffer_to_string(&buf));
    }

    #[test]
    fn status_bar_global_region_hides_geo_info() {
        let mut model = model_with_profiles(vec![]);
        model
            .config
            .settings
            .geo_routing
            .set_region(GeoRegion::Global);
        model.geo_last_updated = Some("2026-05-31 13:41".to_string());
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 1));
        StatusBar::new(&model).render(Rect::new(0, 0, 80, 1), &mut buf);
        let content: String = buf.content.iter().map(|c| c.symbol()).collect();
        assert!(content.contains("[DISCONNECTED]"));
        assert!(content.contains("[Global]"));
        assert!(!content.contains("[Geo:"));
    }

    #[test]
    fn format_bps_boundaries() {
        assert_eq!(format_bps(0), "0.0 KB/s");
        assert_eq!(format_bps(850), "0.8 KB/s");
        assert_eq!(format_bps(999), "1.0 KB/s");
        assert_eq!(format_bps(1000), "1.0 KB/s");
        assert_eq!(format_bps(1023), "1.0 KB/s");
        assert_eq!(format_bps(1024), "1.0 KB/s");
        assert_eq!(format_bps(1500), "1.5 KB/s");
        assert_eq!(format_bps(1_500_000), "1.4 MB/s");
        assert_eq!(format_bps(10 * 1024), "10 KB/s");
        assert_eq!(format_bps(100 * 1024), "100 KB/s");
        assert_eq!(format_bps(999 * 1024), "999 KB/s");
        assert_eq!(format_bps(1000 * 1024), "1.0 MB/s");
        assert_eq!(format_bps(2 * 1024 * 1024 * 1024), "2.0 GB/s");
        assert_eq!(format_bps(1000 * 1024 * 1024 * 1024), "1.0 TB/s");
    }

    #[test]
    fn format_bytes_boundaries() {
        assert_eq!(format_bytes(0), "0.0 KB");
        assert_eq!(format_bytes(850), "0.8 KB");
        assert_eq!(format_bytes(999), "1.0 KB");
        assert_eq!(format_bytes(1000), "1.0 KB");
        assert_eq!(format_bytes(1023), "1.0 KB");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1_500_000), "1.4 MB");
        assert_eq!(format_bytes(10 * 1024), "10 KB");
        assert_eq!(format_bytes(100 * 1024), "100 KB");
        assert_eq!(format_bytes(999 * 1024), "999 KB");
        assert_eq!(format_bytes(1000 * 1024), "1.0 MB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
        assert_eq!(format_bytes(1000 * 1024 * 1024 * 1024), "1.0 TB");
    }

    #[test]
    fn traffic_fields_have_fixed_widths() {
        for bps in [0, 1023, 1024, 1_500_000, 2 * 1024 * 1024 * 1024] {
            assert_eq!(format_bps_field(bps).len(), TRAFFIC_RATE_WIDTH);
        }
        for bytes in [0, 1023, 1024, 1_500_000, 3 * 1024 * 1024 * 1024] {
            assert_eq!(format_bytes_field(bytes).len(), TRAFFIC_TOTAL_WIDTH);
        }
        assert_eq!(format_bps_field(0), "0.0 KB/s");
        assert_eq!(format_bps_field(12 * 1024), " 12 KB/s");
        assert_eq!(format_bytes_field(0), "0.0 KB");
    }

    #[test]
    fn traffic_fields_handle_overflow_without_growing() {
        assert_eq!(format_bps_field(u64::MAX), " 16 EB/s");
        assert_eq!(format_bytes_field(u64::MAX), " 16 EB");
        assert_eq!(format_connections_field(18), "18");
        assert_eq!(format_connections_field(99_999), "99999");
        assert_eq!(format_connections_field(100_000), "99999+");
        assert_eq!(
            format_connections_field(18).len() + format_connections_padding(18).len(),
            TRAFFIC_CONNECTIONS_COUNT_WIDTH
        );
    }

    #[test]
    fn status_bar_long_message_truncated_snapshot() {
        let mut model = model_with_profiles(vec![]);
        model.geo_last_updated = Some("2026-05-31 13:41".to_string());
        model.status = crate::app::model::AppStatus::Error(
            "Connection failed: sing-box exited immediately (code: Some(1)). stderr: FATAL[0000] create service: parse outbound[0].server_settings.address: lookup example.com: no such host".to_string(),
        );
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 1));
        StatusBar::new(&model).render(Rect::new(0, 0, 80, 1), &mut buf);
        insta::assert_snapshot!(buffer_to_string(&buf));
    }

    /// `Connecting` falls through the status-bar match to `[DISCONNECTED]`
    /// (only `Connected` and `ConnectPending` get explicit labels). Pin this
    /// so the fall-through stays intentional — if a future patch wants to
    /// show `[CONNECTING]` here, this snapshot will fail and force review.
    #[test]
    fn status_bar_connecting_snapshot() {
        let mut model = model_with_profiles(vec![Profile::new_vless(
            "Alpha".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        model.connection = ConnectionState::Connecting;
        model.geo_last_updated = Some("2026-05-31 13:41".to_string());
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 1));
        StatusBar::new(&model).render(Rect::new(0, 0, 80, 1), &mut buf);
        insta::assert_snapshot!(buffer_to_string(&buf));
    }

    /// `ConnectPending` is the only state that renders `[CONNECTING]`.
    #[test]
    fn status_bar_connect_pending_snapshot() {
        let mut model = model_with_profiles(vec![Profile::new_vless(
            "Alpha".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        model.connection = ConnectionState::ConnectPending;
        model.geo_last_updated = Some("2026-05-31 13:41".to_string());
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 1));
        StatusBar::new(&model).render(Rect::new(0, 0, 80, 1), &mut buf);
        insta::assert_snapshot!(buffer_to_string(&buf));
    }
}
