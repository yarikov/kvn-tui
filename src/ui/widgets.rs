use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph, Widget, Wrap};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::model::{AppStatus, ConnectionState, Model};
use crate::config::profile::GeoRegion;

const RULE_SET_STALE_GRACE_MINUTES: u64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuleSetFreshness {
    Disabled,
    Updating,
    Never,
    Fresh(chrono::DateTime<chrono::Local>),
    Stale(chrono::DateTime<chrono::Local>),
    Manual(chrono::DateTime<chrono::Local>),
}

fn rule_set_freshness(model: &Model, now: chrono::DateTime<chrono::Local>) -> RuleSetFreshness {
    let routing = &model.config.settings.geo_routing;
    let has_region_rule_sets = routing
        .current_region
        .is_some_and(|region| region != GeoRegion::Global);
    let enabled_services = routing.enabled_services();

    if !has_region_rule_sets && enabled_services.is_empty() {
        return RuleSetFreshness::Disabled;
    }
    if model.geo_updating {
        return RuleSetFreshness::Updating;
    }

    let region_checked_at = has_region_rule_sets
        .then_some(model.geo_last_checked_at)
        .flatten();
    let latest = region_checked_at
        .into_iter()
        .chain(
            enabled_services
                .into_iter()
                .filter_map(|service| model.service_checked_at.get(&service).copied()),
        )
        .max();
    let Some(last_checked_at) = latest else {
        return RuleSetFreshness::Never;
    };

    let interval = routing.auto_update.interval_minutes();
    if interval == 0 {
        return RuleSetFreshness::Manual(last_checked_at);
    }
    let stale_after = interval.saturating_add(RULE_SET_STALE_GRACE_MINUTES);
    if now.signed_duration_since(last_checked_at).num_minutes() >= stale_after as i64 {
        RuleSetFreshness::Stale(last_checked_at)
    } else {
        RuleSetFreshness::Fresh(last_checked_at)
    }
}

fn rule_set_badge(
    model: &Model,
    now: chrono::DateTime<chrono::Local>,
) -> Option<(String, ratatui::style::Style)> {
    let (text, style) = match rule_set_freshness(model, now) {
        RuleSetFreshness::Disabled => return None,
        RuleSetFreshness::Updating => (
            "updating…".to_string(),
            model.theme.ruleset_updating_badge(),
        ),
        RuleSetFreshness::Never => ("never".to_string(), model.theme.ruleset_stale_badge()),
        RuleSetFreshness::Fresh(checked_at) => (
            checked_at.format("%d %b %H:%M").to_string(),
            model.theme.ruleset_fresh_badge(),
        ),
        RuleSetFreshness::Stale(checked_at) => (
            checked_at.format("%d %b %H:%M").to_string(),
            model.theme.ruleset_stale_badge(),
        ),
        RuleSetFreshness::Manual(checked_at) => (
            checked_at.format("%d %b %H:%M").to_string(),
            model.theme.ruleset_manual_badge(),
        ),
    };
    let schedule = model.config.settings.geo_routing.auto_update.label();
    Some((format!(" {schedule} {text} "), style))
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
    let mut width = 0;
    for ch in s.chars() {
        let ch_width = ch.width().unwrap_or(0);
        if width + ch_width > max_width.saturating_sub(3) {
            result.push_str("...");
            return result;
        }
        result.push(ch);
        width += ch_width;
    }
    result
}

impl<'a> Widget for StatusBar<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }

        Paragraph::new("")
            .style(self.model.theme.status_bar())
            .render(area, buf);

        let (status, style) = match self.model.connection {
            ConnectionState::Connected => (" CONNECTED ", self.model.theme.connected_badge()),
            ConnectionState::Connecting | ConnectionState::ConnectPending => {
                (" CONNECTING ", self.model.theme.connecting_badge())
            }
            ConnectionState::Idle => (" DISCONNECTED ", self.model.theme.offline_badge()),
        };

        let dns = &self.model.config.settings.dns;
        let dns_label = if dns.fakeip_enabled {
            "fakeip".to_string()
        } else {
            dns.final_server_entry()
                .map(|s| s.kind_label().to_string())
                .unwrap_or_else(|| "?".to_string())
        };

        let mut right = Vec::new();
        if self.model.config.settings.kill_switch {
            right.push(("KS".to_string(), self.model.theme.accent(), 4));
        }
        if self.model.config.settings.auto_connect {
            right.push(("Auto".to_string(), self.model.theme.accent(), 0));
        }
        right.push((dns_label, self.model.theme.accent(), 1_u8));
        right.push((
            self.model.config.settings.geo_routing.mode().to_string(),
            self.model.theme.accent(),
            3,
        ));
        if let Some((label, style)) = rule_set_badge(self.model, chrono::Local::now()) {
            right.push((label, style, 2));
        }

        let right_width = |parts: &[(String, ratatui::style::Style, u8)]| {
            parts.iter().map(|(text, _, _)| text.width()).sum::<usize>()
                + parts.len().saturating_sub(1) * 2
        };
        let available = usize::from(area.width);
        let status_width = status.width();
        while status_width + 1 + right_width(&right) > available {
            let Some((index, _)) = right
                .iter()
                .enumerate()
                .min_by_key(|(_, (_, _, priority))| *priority)
            else {
                break;
            };
            right.remove(index);
        }

        let right_size = right_width(&right);
        let profile = (self.model.connection == ConnectionState::Connected)
            .then(|| {
                self.model.active_profile_id.and_then(|id| {
                    self.model
                        .config
                        .profiles
                        .iter()
                        .find(|profile| profile.id == id)
                        .map(|profile| profile.name.as_str())
                })
            })
            .flatten();
        let left_limit = available.saturating_sub(right_size + usize::from(!right.is_empty()));
        let profile_limit = left_limit.saturating_sub(status_width + 1);
        let profile = profile
            .filter(|_| profile_limit >= 4)
            .map(|name| truncate_to_width(name, profile_limit));

        let mut left = vec![Span::styled(status, style)];
        if let Some(profile) = profile {
            left.push(Span::raw(" "));
            left.push(Span::styled(profile, self.model.theme.normal()));
        }
        Paragraph::new(Line::from(left))
            .style(self.model.theme.status_bar())
            .alignment(Alignment::Left)
            .render(area, buf);

        if !right.is_empty() {
            let mut spans = Vec::new();
            for (index, (text, style, _)) in right.into_iter().enumerate() {
                if index > 0 {
                    spans.push(Span::raw("  "));
                }
                spans.push(Span::styled(text, style));
            }
            let right_area = Rect::new(
                area.right().saturating_sub(right_size as u16),
                area.y,
                right_size as u16,
                1,
            );
            Paragraph::new(Line::from(spans))
                .style(self.model.theme.status_bar())
                .alignment(Alignment::Right)
                .render(right_area, buf);
        }
    }
}

/// Temporary status notification rendered above the application rather than
/// consuming space in the persistent bottom bar.
pub struct Toast<'a> {
    status: &'a AppStatus,
    theme: &'a crate::ui::styles::Theme,
}

impl<'a> Toast<'a> {
    pub fn new(status: &'a AppStatus, theme: &'a crate::ui::styles::Theme) -> Self {
        Self { status, theme }
    }

    pub fn area(&self, container: Rect) -> Option<Rect> {
        if container.width < 12 || container.height < 5 || self.status.text().is_empty() {
            return None;
        }
        let max_width = container.width.saturating_sub(2).min(50);
        let title_width: u16 = if matches!(self.status, AppStatus::Error(_)) {
            9
        } else {
            10
        };
        let text_width = self.status.text().width().min(46) as u16;
        let width = text_width.saturating_add(4).max(title_width).min(max_width);
        let inner_width = usize::from(width.saturating_sub(4)).max(1);
        let height = if self.status.text().width() > inner_width {
            4
        } else {
            3
        };
        Some(Rect::new(
            container.right().saturating_sub(width + 1),
            container.y + 1,
            width,
            height,
        ))
    }
}

impl Widget for Toast<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 3 || area.height < 3 {
            return;
        }
        Clear.render(area, buf);
        let (title, border_style) = match self.status {
            AppStatus::Info(_) => (" Status ", self.theme.toast_info()),
            AppStatus::Error(_) => (" Error ", self.theme.toast_error()),
        };
        let inner_width = usize::from(area.width.saturating_sub(4));
        let text = truncate_to_width(self.status.text(), inner_width * 2);
        Paragraph::new(text)
            .style(self.theme.popup_bg())
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .padding(Padding::horizontal(1))
                    .title(title)
                    .border_style(border_style)
                    .style(self.theme.popup_bg()),
            )
            .render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::model::ConnectionState;
    use crate::config::profile::{GeoAutoUpdate, GeoRegion, Profile, RoutedService, ServiceRoute};
    use crate::test_helpers::{buffer_to_string, model_with_profiles};
    use chrono::{Duration, TimeZone};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    fn local_time(day: u32, hour: u32) -> chrono::DateTime<chrono::Local> {
        chrono::Local
            .with_ymd_and_hms(2026, 8, day, hour, 20, 0)
            .single()
            .unwrap()
    }

    fn model_with_region_rule_sets() -> Model {
        let mut model = model_with_profiles(vec![]);
        model.config.settings.geo_routing.set_region(GeoRegion::Ru);
        model
    }

    #[test]
    fn rule_set_freshness_is_disabled_without_active_sets() {
        let model = model_with_profiles(vec![]);
        assert_eq!(
            rule_set_freshness(&model, local_time(14, 12)),
            RuleSetFreshness::Disabled
        );
    }

    #[test]
    fn rule_set_freshness_reports_updating_and_never() {
        let mut model = model_with_region_rule_sets();
        assert_eq!(
            rule_set_freshness(&model, local_time(14, 12)),
            RuleSetFreshness::Never
        );

        model.geo_updating = true;
        assert_eq!(
            rule_set_freshness(&model, local_time(14, 12)),
            RuleSetFreshness::Updating
        );
    }

    #[test]
    fn rule_set_freshness_uses_latest_active_check() {
        let mut model = model_with_region_rule_sets();
        model.config.settings.geo_routing.auto_update = GeoAutoUpdate::Every1d;
        model.geo_last_checked_at = Some(local_time(11, 12));
        model
            .config
            .settings
            .geo_routing
            .service_routes
            .insert(RoutedService::Telegram, ServiceRoute::Proxy);
        let service_checked_at = local_time(14, 11);
        model
            .service_checked_at
            .insert(RoutedService::Telegram, service_checked_at);

        assert_eq!(
            rule_set_freshness(&model, local_time(14, 12)),
            RuleSetFreshness::Fresh(service_checked_at)
        );
    }

    #[test]
    fn rule_set_freshness_supports_service_sets_in_global_region() {
        let mut model = model_with_profiles(vec![]);
        model
            .config
            .settings
            .geo_routing
            .service_routes
            .insert(RoutedService::Steam, ServiceRoute::Direct);
        model.config.settings.geo_routing.auto_update = GeoAutoUpdate::Every7d;
        let checked_at = local_time(14, 10);
        model
            .service_checked_at
            .insert(RoutedService::Steam, checked_at);

        assert_eq!(
            rule_set_freshness(&model, local_time(14, 12)),
            RuleSetFreshness::Fresh(checked_at)
        );
    }

    #[test]
    fn rule_set_freshness_becomes_stale_one_hour_after_configured_interval() {
        let mut model = model_with_region_rule_sets();
        model.config.settings.geo_routing.auto_update = GeoAutoUpdate::Every3d;
        let checked_at = local_time(11, 12);
        model.geo_last_checked_at = Some(checked_at);

        assert_eq!(
            rule_set_freshness(&model, checked_at + Duration::days(3)),
            RuleSetFreshness::Fresh(checked_at)
        );
        assert_eq!(
            rule_set_freshness(
                &model,
                checked_at + Duration::days(3) + Duration::minutes(59)
            ),
            RuleSetFreshness::Fresh(checked_at)
        );
        assert_eq!(
            rule_set_freshness(&model, checked_at + Duration::days(3) + Duration::hours(1)),
            RuleSetFreshness::Stale(checked_at)
        );
    }

    #[test]
    fn rule_set_freshness_is_neutral_when_auto_update_is_off() {
        let mut model = model_with_region_rule_sets();
        let checked_at = local_time(1, 8);
        model.geo_last_checked_at = Some(checked_at);

        assert_eq!(
            rule_set_freshness(&model, local_time(14, 12)),
            RuleSetFreshness::Manual(checked_at)
        );
    }

    #[test]
    fn rule_set_badge_has_omarchy_icon_padding_and_state_style() {
        let mut model = model_with_region_rule_sets();
        model.config.settings.geo_routing.auto_update = GeoAutoUpdate::Every1d;
        model.geo_last_checked_at = Some(local_time(14, 10));

        let (label, style) = rule_set_badge(&model, local_time(14, 12)).unwrap();
        assert_eq!(label, "  (1d) 14 Aug 10:20 ");
        assert_eq!(style, model.theme.ruleset_fresh_badge());

        model.geo_updating = true;
        let (label, style) = rule_set_badge(&model, local_time(14, 12)).unwrap();
        assert_eq!(label, "  (1d) updating… ");
        assert_eq!(style, model.theme.ruleset_updating_badge());

        model.geo_updating = false;
        model.geo_last_checked_at = None;
        model.config.settings.geo_routing.auto_update = GeoAutoUpdate::Every3d;
        let (label, style) = rule_set_badge(&model, local_time(14, 12)).unwrap();
        assert_eq!(label, "  (3d) never ");
        assert_eq!(style, model.theme.ruleset_stale_badge());

        model.geo_last_checked_at = Some(local_time(14, 10));
        model.config.settings.geo_routing.auto_update = GeoAutoUpdate::Off;
        let (label, style) = rule_set_badge(&model, local_time(14, 12)).unwrap();
        assert_eq!(label, "  (off) 14 Aug 10:20 ");
        assert_eq!(style, model.theme.ruleset_manual_badge());
    }

    #[test]
    fn status_bar_shows_disconnected() {
        let model = model_with_profiles(vec![]);
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 1));
        StatusBar::new(&model).render(Rect::new(0, 0, 80, 1), &mut buf);
        let content: String = buf.content.iter().map(|c| c.symbol()).collect();
        assert!(content.starts_with(" DISCONNECTED "));
        assert!(content.contains("Global"));
        let idx = content.find("DISCONNECTED").unwrap();
        assert_eq!(buf.content[idx].style().fg, model.theme.offline_badge().fg);
        assert_eq!(buf.content[idx].style().bg, model.theme.offline_badge().bg);
        assert!(
            buf.content[idx]
                .style()
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
        );
        assert_eq!(buf.content[40].style().bg, model.theme.status_bar().bg);
    }

    #[test]
    fn status_bar_connect_pending_shows_connecting() {
        use crate::app::model::ConnectionState;
        let mut model = model_with_profiles(vec![]);
        model.connection = ConnectionState::ConnectPending;
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 1));
        StatusBar::new(&model).render(Rect::new(0, 0, 80, 1), &mut buf);
        let content: String = buf.content.iter().map(|c| c.symbol()).collect();
        assert!(content.starts_with(" CONNECTING "));
        let idx = content.find("CONNECTING").unwrap();
        assert_eq!(
            buf.content[idx].style().fg,
            model.theme.connecting_badge().fg
        );
        assert_eq!(
            buf.content[idx].style().bg,
            model.theme.connecting_badge().bg
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
        model.active_profile_id = Some(model.config.profiles[0].id);
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
        model.active_profile_id = Some(model.config.profiles[0].id);
        model.config.settings.auto_connect = true;
        model.geo_last_updated = Some("2026-05-31 13:41".to_string());
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 1));
        StatusBar::new(&model).render(Rect::new(0, 0, 80, 1), &mut buf);
        insta::assert_snapshot!(buffer_to_string(&buf));
    }

    #[test]
    fn status_bar_does_not_include_transient_geo_state() {
        let mut model = model_with_profiles(vec![]);
        model.geo_updating = true;
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 1));
        StatusBar::new(&model).render(Rect::new(0, 0, 80, 1), &mut buf);
        assert!(!buffer_to_string(&buf).contains("Geo"));
    }

    #[test]
    fn status_bar_does_not_include_geo_timestamp() {
        let mut model = model_with_profiles(vec![]);
        model.geo_last_updated = Some("2026-05-31 13:41".to_string());
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 1));
        StatusBar::new(&model).render(Rect::new(0, 0, 80, 1), &mut buf);
        let content: String = buf.content.iter().map(|c| c.symbol()).collect();
        assert!(content.contains("DISCONNECTED"));
        assert!(content.contains("Global"));
        assert!(!content.contains("Geo"));
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
    fn status_bar_does_not_include_status_message() {
        let mut model = model_with_profiles(vec![]);
        model.geo_last_updated = Some("2026-05-31 13:41".to_string());
        model.status = crate::app::model::AppStatus::Error(
            "Connection failed: sing-box exited immediately (code: Some(1)). stderr: FATAL[0000] create service: parse outbound[0].server_settings.address: lookup example.com: no such host".to_string(),
        );
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 1));
        StatusBar::new(&model).render(Rect::new(0, 0, 80, 1), &mut buf);
        let content = buffer_to_string(&buf);
        assert!(!content.contains("Connection failed"));
        assert!(content.contains("DISCONNECTED"));
    }

    /// Both phases of connection establishment use the same stable badge.
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

    /// ConnectPending uses the same badge while the worker is starting.
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

    #[test]
    fn narrow_status_bar_keeps_kill_switch_before_lower_priority_segments() {
        let mut model = model_with_profiles(vec![]);
        model.config.settings.kill_switch = true;
        model.config.settings.auto_connect = true;
        model.config.settings.geo_routing.set_region(GeoRegion::Ru);
        model.geo_last_checked_at = Some(chrono::Local::now());
        let area = Rect::new(0, 0, 18, 1);
        let mut buf = Buffer::empty(area);
        StatusBar::new(&model).render(area, &mut buf);
        let content = buffer_to_string(&buf);
        assert!(content.contains("DISCONNECTED"));
        assert!(content.contains("KS"));
        assert!(!content.contains("DNS"));
        assert!(!content.contains("Global"));
        assert!(!content.contains("Auto"));
        assert!(!content.contains(''));
    }

    #[test]
    fn status_bar_uses_full_panel_width_and_uniform_setting_styles() {
        let mut model = model_with_profiles(vec![]);
        model.config.settings.kill_switch = true;
        model.config.settings.auto_connect = true;
        model.config.settings.geo_routing.set_region(GeoRegion::Ru);
        model.geo_last_checked_at = Some(chrono::Local::now());
        model.config.settings.geo_routing.auto_update = GeoAutoUpdate::Every1d;
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);
        StatusBar::new(&model).render(area, &mut buf);
        let content: String = buf.content.iter().map(|cell| cell.symbol()).collect();

        assert!(content.starts_with(" DISCONNECTED "));
        assert!(content.ends_with(' '));
        assert!(!content.contains('│'));
        assert!(!content.contains("DNS"));
        assert!(!content.contains('·'));

        let ks = content.find("KS").unwrap();
        let auto = content.find("Auto").unwrap();
        let dns = content.find("DoH").unwrap();
        let routing = content.find("Global").unwrap();
        let rules = content.find('').unwrap();
        assert!(ks < auto && auto < dns && dns < routing && routing < rules);

        assert_eq!(buf.content[dns].style().fg, buf.content[ks].style().fg);
        assert_eq!(buf.content[dns].style().bg, buf.content[ks].style().bg);
        assert_eq!(
            buf.content[dns].style().add_modifier,
            buf.content[ks].style().add_modifier
        );
    }

    #[test]
    fn info_toast_snapshot() {
        let status = AppStatus::Info("Connected to Alpha".into());
        let theme = crate::ui::styles::Theme::legacy();
        let container = Rect::new(0, 0, 60, 10);
        let toast = Toast::new(&status, &theme);
        let area = toast.area(container).unwrap();
        let mut buf = Buffer::empty(container);
        toast.render(area, &mut buf);
        let rendered = buffer_to_string(&buf);
        let trimmed = rendered
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n");
        insta::assert_snapshot!(trimmed);
    }

    #[test]
    fn error_toast_wraps_to_two_lines_snapshot() {
        let status = AppStatus::Error(
            "Connection failed: handshake timed out while contacting the selected server".into(),
        );
        let theme = crate::ui::styles::Theme::legacy();
        let container = Rect::new(0, 0, 42, 10);
        let toast = Toast::new(&status, &theme);
        let area = toast.area(container).unwrap();
        let mut buf = Buffer::empty(container);
        toast.render(area, &mut buf);
        let rendered = buffer_to_string(&buf);
        let trimmed = rendered
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n");
        insta::assert_snapshot!(trimmed);
    }

    #[test]
    fn toast_skips_terminals_too_small_for_a_popup() {
        let status = AppStatus::Info("Saved".into());
        let theme = crate::ui::styles::Theme::legacy();
        assert!(
            Toast::new(&status, &theme)
                .area(Rect::new(0, 0, 10, 4))
                .is_none()
        );
    }
}
