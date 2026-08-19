use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Row, Table, Wrap};
use unicode_width::UnicodeWidthChar;

use crate::app::model::{ConnectionState, Model, Overlay, SourceRow};
use crate::ui::styles::Theme;
use crate::ui::widgets::{StatusBar, format_bps, format_bytes};

/// Height (including borders) of the full-width traffic header rendered at
/// the very top of the UI when the VPN is connected. One content row plus
/// top/bottom borders = 3 lines.
const TRAFFIC_PANEL_HEIGHT: u16 = 3;

/// Render the full application UI into the terminal frame.
pub fn draw(frame: &mut Frame, model: &Model) {
    let area = frame.area();

    // Paint the palette's background across the whole frame first so that
    // every widget below — most of which set only `fg` — inherits a
    // theme-consistent background instead of the terminal default. Popups
    // override with their own `popup_bg()` (currently the same color).
    frame.render_widget(Block::default().style(model.theme.background()), area);

    // Top-level vertical layout: optional traffic header, main content, status bar.
    let show_traffic =
        model.connection == ConnectionState::Connected && area.height > TRAFFIC_PANEL_HEIGHT + 5;
    let (traffic_area, main_area, status_area) = if show_traffic {
        let split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(TRAFFIC_PANEL_HEIGHT),
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .split(area);
        (Some(split[0]), split[1], split[2])
    } else {
        let split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .split(area);
        (None, split[0], split[1])
    };

    if let Some(area) = traffic_area {
        draw_traffic_panel(frame, model, area);
    }
    draw_main(frame, model, main_area);
    draw_status_bar(frame, model, status_area);

    let theme = &model.theme;
    match model.overlay {
        Overlay::Help => draw_help(frame, theme, area),
        Overlay::ConfirmDelete => draw_confirm_delete(frame, model, area),
        Overlay::RoutingMode => draw_routing_mode(frame, model, area),
        Overlay::GeoRegions => draw_geo_region(frame, model, area),
        Overlay::DnsSettings => draw_dns_settings(frame, model, area),
        Overlay::ThemeSettings => draw_theme_settings(frame, model, area),
        Overlay::ServiceRouting => draw_service_routing(frame, model, area),
        Overlay::None => {}
    }
}

/// Draw the main content area with the Sources list and logs.
fn draw_main(frame: &mut Frame, model: &Model, area: Rect) {
    let theme = &model.theme;
    let content_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    draw_sources(frame, model, content_chunks[0]);

    let log_block = Block::default()
        .title(" Logs ")
        .borders(Borders::ALL)
        .border_style(theme.border());

    // Show the most recent log lines that fit in the available area.
    let available_height = content_chunks[1].height.saturating_sub(2) as usize;
    let start = model.logs.len().saturating_sub(available_height);
    let log_text: Vec<Line> = model
        .logs
        .iter()
        .skip(start)
        .map(|l| {
            let style = if l.starts_with("[error]") {
                theme.error()
            } else {
                theme.normal()
            };
            Line::from(Span::styled(l.as_str(), style))
        })
        .collect();

    let logs = Paragraph::new(log_text)
        .block(log_block)
        .wrap(Wrap { trim: true });
    frame.render_widget(logs, content_chunks[1]);
}

/// Render the full-width traffic header: instantaneous ↑/↓ rate, cumulative
/// totals, and active connection count laid out on a single content row
/// inside a bordered block. Driven by `model.traffic`, updated ~1 Hz by the
/// daemon.
fn draw_traffic_panel(frame: &mut Frame, model: &Model, area: Rect) {
    let theme = &model.theme;
    let t = &model.traffic;
    let line = Line::from(vec![
        Span::styled("↑ ", theme.success()),
        Span::styled(format_bps(t.up_rate_bps), theme.normal()),
        Span::raw("   "),
        Span::styled("↓ ", theme.accent()),
        Span::styled(format_bps(t.down_rate_bps), theme.normal()),
        Span::raw("     "),
        Span::styled("Total ", theme.border()),
        Span::styled("↑ ", theme.success()),
        Span::styled(format_bytes(t.up_total), theme.normal()),
        Span::raw("   "),
        Span::styled("↓ ", theme.accent()),
        Span::styled(format_bytes(t.down_total), theme.normal()),
        Span::raw("     "),
        Span::styled(format!("{}", t.conn_count), theme.accent()),
        Span::styled(" connections", theme.normal()),
    ]);
    let block = Block::default()
        .title(" Traffic ")
        .borders(Borders::ALL)
        .border_style(theme.border());
    let paragraph = Paragraph::new(line).block(block);
    frame.render_widget(paragraph, area);
}

/// Draw the bottom status bar.
fn draw_status_bar(frame: &mut Frame, model: &Model, area: Rect) {
    let status = StatusBar::new(model);
    frame.render_widget(status, area);
}

/// Draw the help popup overlay.
fn draw_help(frame: &mut Frame, theme: &Theme, area: Rect) {
    let header = Row::new(vec!["Key", "Action"]).style(theme.accent().add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = vec![
        Row::new(vec!["j / Down", "Move down"]),
        Row::new(vec!["k / Up", "Move up"]),
        Row::new(vec!["g", "Go to first"]),
        Row::new(vec!["G", "Go to last"]),
        Row::new(vec!["Enter", "Connect to selected profile"]),
        Row::new(vec!["p", "Paste from clipboard"]),
        Row::new(vec!["y", "Yank share link to clipboard"]),
        Row::new(vec!["d", "Delete selected source"]),
        Row::new(vec!["m", "Routing mode (popup list)"]),
        Row::new(vec!["o", "Geo region"]),
        Row::new(vec!["u", "Update subscription or geo"]),
        Row::new(vec!["i", "Cycle subscription auto-update"]),
        Row::new(vec!["I", "Cycle geo auto-update"]),
        Row::new(vec!["e", "Open profiles.json in $EDITOR"]),
        Row::new(vec!["a", "Toggle auto-connect"]),
        Row::new(vec!["K", "Toggle kill switch"]),
        Row::new(vec!["D", "DNS settings"]),
        Row::new(vec!["S", "Service routing"]),
        Row::new(vec!["C", "Theme picker"]),
        Row::new(vec!["t", "Test selected profile latency"]),
        Row::new(vec!["T", "Test all profiles (batch)"]),
        Row::new(vec!["r", "Reconnect"]),
        Row::new(vec!["s", "Stop / disconnect"]),
        Row::new(vec!["q / Esc", "Detach TUI"]),
        Row::new(vec!["Ctrl+C", "Quit"]),
        Row::new(vec!["?", "Show this help"]),
    ];

    let needed = rows.len() as u16 + 1 + 2 + 1; // data rows + header + borders + padding
    let percent = ((needed * 100) / area.height).clamp(50, 90);
    let popup_area = centered_rect(POPUP_WIDTH_PERCENT, percent, area);

    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .title(" Help ")
        .borders(Borders::ALL)
        .border_style(theme.border())
        .style(theme.popup_bg());

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let table = Table::new(rows, [Constraint::Length(12), Constraint::Min(1)]).header(header);

    frame.render_widget(table, inner);
}

/// Draw the delete confirmation dialog.
fn draw_confirm_delete(frame: &mut Frame, model: &Model, area: Rect) {
    use crate::app::model::SourceRow;
    let theme = &model.theme;
    let message = match model.selected_row() {
        Some(SourceRow::SubscriptionHeader(_)) => "Delete selected subscription and its profiles?",
        _ => "Delete selected profile?",
    };
    draw_modal(
        frame,
        theme,
        area,
        " Confirm ",
        vec![
            Line::from(Span::styled(message, theme.error())),
            Line::from(""),
            Line::from("Press y to confirm, n to cancel"),
        ],
        POPUP_HEIGHT_PERCENT,
    );
}

const POPUP_WIDTH_PERCENT: u16 = 60;
const POPUP_HEIGHT_PERCENT: u16 = 50;
/// Taller variant for overlays whose list grows past ~6 items (e.g. the
/// theme picker with 19+ entries). Keeps text inside the visible region
/// on standard 24-row terminals.
const POPUP_HEIGHT_PERCENT_TALL: u16 = 90;

/// Helper to render a centered popup with a border and text.
fn draw_modal(
    frame: &mut Frame,
    theme: &Theme,
    area: Rect,
    title: &str,
    lines: Vec<Line>,
    height_percent: u16,
) {
    let popup_area = centered_rect(POPUP_WIDTH_PERCENT, height_percent, area);

    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(theme.border())
        .style(theme.popup_bg());

    let paragraph = Paragraph::new(lines)
        .block(block)
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true });

    frame.render_widget(paragraph, popup_area);
}

/// Draw the routing mode selection modal.
fn draw_routing_mode(frame: &mut Frame, model: &Model, area: Rect) {
    let modes = model.config.settings.geo_routing.available_modes();
    let label_strings: Vec<String> = modes.iter().map(|m| m.to_string()).collect();
    let labels: Vec<&str> = label_strings.iter().map(String::as_str).collect();
    let active = modes
        .iter()
        .position(|m| *m == model.config.settings.geo_routing.mode());
    draw_selection_modal(
        frame,
        &model.theme,
        area,
        "Select routing mode",
        " Routing Mode ",
        &labels,
        model.routing_selected,
        active,
        POPUP_HEIGHT_PERCENT,
    );
}

/// Draw the geo region selection modal.
fn draw_geo_region(frame: &mut Frame, model: &Model, area: Rect) {
    use crate::config::profile::GeoRegion;
    // Labels are aligned 1:1 with `GeoRegion::ALL`. The debug_assert catches
    // a missed entry when a new region is added.
    let labels = [
        "🇷🇺 Russia",
        "🇨🇳 China",
        "🇮🇷 Iran",
        "🌍 Global (no geo rules)",
    ];
    debug_assert_eq!(labels.len(), GeoRegion::ALL.len());
    let active = model
        .config
        .settings
        .geo_routing
        .current_region
        .and_then(|r| GeoRegion::ALL.iter().position(|x| *x == r));
    draw_selection_modal(
        frame,
        &model.theme,
        area,
        "Select geo region",
        " Geo Region ",
        &labels,
        model.geo_region_selected,
        active,
        POPUP_HEIGHT_PERCENT,
    );
}

/// Draw the DNS settings overlay: built-in presets, strategy cycle, fake-IP
/// toggle. Custom servers and per-domain rules are edited via the main
/// profiles.json (`e` key in the sources list).
fn draw_dns_settings(frame: &mut Frame, model: &Model, area: Rect) {
    let dns = &model.config.settings.dns;
    let strategy_label = if let Some(ref draft) = model.dns_strategy_draft {
        if *draft == dns.strategy {
            format!("Strategy: ‹ {} ›", draft.as_str())
        } else {
            format!("Strategy: ‹ {} › *", draft.as_str())
        }
    } else {
        format!("Strategy: ‹ {} ›", dns.strategy.as_str())
    };
    let fakeip = model.dns_fakeip_draft.unwrap_or(dns.fakeip_enabled);
    let fakeip_label = format!(
        "Fake-IP: ‹ {} ›{}",
        if fakeip { "on" } else { "off" },
        if model.dns_fakeip_draft.is_some() && fakeip != dns.fakeip_enabled {
            " *"
        } else {
            ""
        }
    );
    let labels: Vec<String> = vec![
        "Preset: Cloudflare DoH (1.1.1.1)".to_string(),
        "Preset: Google DoT (8.8.8.8)".to_string(),
        "Preset: Quad9 DoH (9.9.9.9)".to_string(),
        "Preset: System resolver (local)".to_string(),
        strategy_label,
        fakeip_label,
    ];
    let label_refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
    draw_selection_modal(
        frame,
        &model.theme,
        area,
        "DNS settings",
        " DNS ",
        &label_refs,
        model.dns_selected,
        current_dns_preset_index(dns),
        POPUP_HEIGHT_PERCENT,
    );
}

/// Draw the service routing overlay. Each row shows a service and its
/// draft route (`‹ value ›`, cycled with h/l); a `*` marks rows whose draft
/// differs from the committed setting. Enter commits the whole draft.
///
/// Rendered without the shared `draw_selection_modal`: that helper centers
/// each line and trims whitespace, which would break the fixed columns this
/// table-shaped overlay needs.
fn draw_service_routing(frame: &mut Frame, model: &Model, area: Rect) {
    use crate::config::profile::RoutedService;

    let theme = &model.theme;
    let committed = &model.config.settings.geo_routing.service_routes;

    let popup_area = centered_rect(POPUP_WIDTH_PERCENT, POPUP_HEIGHT_PERCENT, area);
    frame.render_widget(Clear, popup_area);
    let block = Block::default()
        .title(" Services ")
        .borders(Borders::ALL)
        .border_style(theme.border())
        .style(theme.popup_bg());
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled("Service routing", theme.accent())).centered(),
        Line::from(""),
    ];

    // marker(2) + name(9) + " ‹ " + route(8) + " ›" + dirty(2)
    const ROW_WIDTH: usize = 2 + 9 + 3 + 8 + 2 + 2;
    let indent = " ".repeat((inner.width as usize).saturating_sub(ROW_WIDTH) / 2);
    for (i, service) in RoutedService::ALL.into_iter().enumerate() {
        let saved = committed.get(&service).copied().unwrap_or_default();
        // Inside a draft an absent entry IS Disabled (the commit handler
        // normalizes Disabled to "no entry") — it must not fall back to the
        // committed value, or cycling back to Disabled would render stale.
        let shown = match model.service_routing_draft.as_ref() {
            Some(draft) => draft.get(&service).copied().unwrap_or_default(),
            None => saved,
        };
        let selected = i == model.service_routing_selected;
        let row = format!(
            "{}{}{:<9} ‹ {:^8} ›{}",
            indent,
            if selected { "> " } else { "  " },
            service.label(),
            shown.label(),
            if shown != saved { " *" } else { "" },
        );
        let style = if selected {
            theme.accent().add_modifier(Modifier::BOLD)
        } else {
            theme.normal()
        };
        lines.push(Line::from(Span::styled(row, style)));
    }

    lines.push(Line::from(""));
    lines.push(Line::from("j/k select, h/l change, Enter, Esc").centered());

    frame.render_widget(Paragraph::new(lines), inner);
}

/// Draw the theme picker overlay. Lists all bundled palettes plus an
/// optional Auto entry (when Omarchy is detected) that maps to the
/// `"omarchy"` sentinel slug. The active row is the one matching the
/// committed `Settings.theme`; the cursor tracks `model.theme_selected`.
fn draw_theme_settings(frame: &mut Frame, model: &Model, area: Rect) {
    let slugs = crate::app::update::theme_picker_slugs();
    let labels: Vec<String> = crate::app::update::theme_picker_labels();
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let active = slugs.iter().position(|s| s == &model.config.settings.theme);
    draw_selection_modal(
        frame,
        &model.theme,
        area,
        "Select theme",
        " Theme ",
        &label_refs,
        model.theme_selected,
        active,
        POPUP_HEIGHT_PERCENT_TALL,
    );
}

/// Return the index of the built-in preset (0..=3) that matches the user's
/// current `dns.servers + final_server`, or `None` for a custom config.
fn current_dns_preset_index(dns: &crate::config::profile::DnsConfig) -> Option<usize> {
    use crate::config::profile::DnsServer;
    let non_fakeip: Vec<&DnsServer> = dns
        .servers
        .iter()
        .filter(|s| !matches!(s, DnsServer::FakeIp { .. }))
        .collect();
    let final_entry = non_fakeip.iter().find(|s| s.tag() == dns.final_server)?;

    // System: only one server, the local resolver.
    if non_fakeip.len() == 1 {
        return matches!(final_entry, DnsServer::Local { .. }).then_some(3);
    }
    if non_fakeip.len() != 2
        || !non_fakeip
            .iter()
            .any(|s| matches!(s, DnsServer::Local { .. }))
    {
        return None;
    }
    match final_entry {
        DnsServer::Https { server, path, .. } if server == "1.1.1.1" && path == "/dns-query" => {
            Some(0)
        }
        DnsServer::Tls { server, .. } if server == "8.8.8.8" => Some(1),
        DnsServer::Https { server, path, .. } if server == "9.9.9.9" && path == "/dns-query" => {
            Some(2)
        }
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_selection_modal(
    frame: &mut Frame,
    theme: &Theme,
    area: Rect,
    heading: &str,
    modal_title: &str,
    items: &[&str],
    selected: usize,
    active: Option<usize>,
    height_percent: u16,
) {
    let popup_area = centered_rect(POPUP_WIDTH_PERCENT, height_percent, area);
    let max_visible_items = popup_area.height.saturating_sub(6) as usize;
    let visible_count = items.len().min(max_visible_items);
    let window_start = if items.len() > visible_count {
        selected
            .saturating_sub(visible_count / 2)
            .min(items.len() - visible_count)
    } else {
        0
    };
    let window_end = window_start + visible_count;

    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(heading, theme.accent())),
        Line::from(""),
    ];
    for (i, label) in items.iter().enumerate().take(window_end).skip(window_start) {
        let marker = if i == selected { "> " } else { "  " };
        let is_active = active == Some(i);
        let style = if is_active {
            theme.success().add_modifier(Modifier::BOLD)
        } else if i == selected {
            theme.accent().add_modifier(Modifier::BOLD)
        } else {
            theme.normal()
        };
        lines.push(Line::from(Span::styled(
            format!("{}{}", marker, label),
            style,
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from("j/k navigate, Enter confirm, Esc cancel"));
    draw_modal(frame, theme, area, modal_title, lines, height_percent);
}

/// Draw the unified Sources list: standalone profiles and subscription trees.
fn draw_sources(frame: &mut Frame, model: &Model, area: Rect) {
    let theme = &model.theme;
    let block = Block::default()
        .title(" Sources ")
        .borders(Borders::ALL)
        .border_style(theme.border());

    let inner_width = area.width.saturating_sub(2) as usize;
    let mut lines: Vec<Line> = Vec::new();
    let rows = model.source_rows();

    if rows.is_empty() {
        lines.push(Line::from(
            "No sources. Press p to paste a profile or subscription URL from clipboard.",
        ));
    } else {
        // Global address-column width: max across all visible profiles so every
        // row shares the same column layout and aligns vertically.
        let show_latency =
            !model.profile_latencies.is_empty() || !model.testing_profiles.is_empty();
        let overhead = FIXED_OVERHEAD_BASE + if show_latency { LATENCY_WIDTH } else { 0 };
        let remaining = inner_width.saturating_sub(overhead);
        let addr_width = model
            .config
            .profiles
            .iter()
            .map(|p| visual_width(&format!("{}:{}", p.address, p.port)).max(MIN_ADDR_WIDTH))
            .max()
            .unwrap_or(MIN_ADDR_WIDTH)
            .min(MAX_ADDR_WIDTH)
            .min(remaining.saturating_sub(MIN_NAME_WIDTH));

        // Single pass over rows to collect indices — avoids O(n²) position() searches.
        let sub_count = model.config.subscriptions.len();
        let mut standalone: Vec<(usize, usize)> = Vec::new(); // (row_idx, profile_idx)
        let mut sub_header_idx = vec![0usize; sub_count];
        let mut sub_profile_rows: Vec<Vec<(usize, usize)>> = vec![Vec::new(); sub_count];
        for (i, row) in rows.iter().enumerate() {
            match row {
                SourceRow::StandaloneProfile(idx) => standalone.push((i, *idx)),
                SourceRow::SubscriptionHeader(idx) => sub_header_idx[*idx] = i,
                SourceRow::SubscriptionProfile {
                    sub_idx,
                    profile_idx,
                } => {
                    sub_profile_rows[*sub_idx].push((i, *profile_idx));
                }
            }
        }

        // Standalone profiles group.
        if !standalone.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("Standalone profiles ({})", standalone.len()),
                theme.accent().add_modifier(Modifier::BOLD),
            )));
            let last = standalone.len() - 1;
            for (pos, (row_idx, profile_idx)) in standalone.iter().enumerate() {
                lines.push(profile_line(
                    model,
                    *profile_idx,
                    *row_idx,
                    pos == last,
                    inner_width,
                    addr_width,
                    show_latency,
                ));
            }
            lines.push(Line::from(""));
        }

        // Subscription groups.
        for (sub_idx, sub) in model.config.subscriptions.iter().enumerate() {
            let header_idx = sub_header_idx[sub_idx];
            let is_selected = model.selected == header_idx;
            let header_style = if is_selected {
                theme.selected()
            } else {
                theme.normal()
            };
            let profiles = &sub_profile_rows[sub_idx];
            let header_text = format!(
                "Subscription: {} ({}) [{}]",
                sub.name,
                profiles.len(),
                sub.auto_update.label(),
            );
            let header_text = if is_selected {
                pad_to_visual_width(&header_text, inner_width)
            } else {
                header_text
            };
            lines.push(Line::from(Span::styled(header_text, header_style)));

            let last = profiles.len().saturating_sub(1);
            for (pos, (row_idx, profile_idx)) in profiles.iter().enumerate() {
                lines.push(profile_line(
                    model,
                    *profile_idx,
                    *row_idx,
                    pos == last,
                    inner_width,
                    addr_width,
                    show_latency,
                ));
            }
            lines.push(Line::from(""));
        }
    }

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

/// Visual width of a string, counting Unicode characters according to their
/// display width.
fn visual_width(s: &str) -> usize {
    s.chars().filter_map(UnicodeWidthChar::width).sum()
}

/// Truncate a string to fit within a visual width, appending "..." if truncated.
fn truncate_to_visual_width(s: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if max_width <= 3 {
        return ".".repeat(max_width);
    }
    let total_width = visual_width(s);
    if total_width <= max_width {
        return s.to_string();
    }
    let mut result = String::new();
    let mut width = 0;
    let limit = max_width.saturating_sub(3);
    for ch in s.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + cw > limit {
            result.push_str("...");
            return result;
        }
        result.push(ch);
        width += cw;
    }
    result
}

/// Pad a string on the right with spaces up to the target visual width.
fn pad_to_visual_width(s: &str, target: usize) -> String {
    let mut result = s.to_string();
    let width = visual_width(s);
    if width < target {
        result.push_str(&" ".repeat(target - width));
    }
    result
}

/// Truncate and pad a string to exactly the target visual width.
fn fit_to_visual_width(s: &str, target: usize) -> String {
    pad_to_visual_width(&truncate_to_visual_width(s, target), target)
}

/// Width reserved for the tree prefix at the start of a profile row.
const PREFIX_WIDTH: usize = 2;
/// Minimum width for the profile name column.
const MIN_NAME_WIDTH: usize = 8;
/// Width reserved for the protocol column.
const PROTOCOL_WIDTH: usize = 6;
/// Minimum width for the address:port column.
const MIN_ADDR_WIDTH: usize = 11;
/// Maximum width for the address:port column (covers full IPv4 `255.255.255.255:65535`).
/// Caps the address column so the name is never squeezed below MIN_NAME_WIDTH
/// even when a profile has a very long hostname.
const MAX_ADDR_WIDTH: usize = 21;
/// Width of the latency column including its leading space: " 9999ms" = 7 chars.
const LATENCY_WIDTH: usize = 7;
/// Fixed overhead without latency column: prefix + protocol + two spaces between columns.
const FIXED_OVERHEAD_BASE: usize = PREFIX_WIDTH + PROTOCOL_WIDTH + 2;

/// Build one profile row for the Sources list.
/// `addr_width` is pre-computed globally so all rows share the same column layout.
fn profile_line(
    model: &Model,
    profile_idx: usize,
    row_idx: usize,
    is_last: bool,
    inner_width: usize,
    addr_width: usize,
    show_latency: bool,
) -> Line<'static> {
    use ratatui::text::Span;

    let theme = &model.theme;
    let profile = &model.config.profiles[profile_idx];
    let is_selected = model.selected == row_idx;

    let is_connected = model.active_profile_id == Some(profile.id);

    let prefix = if is_last { "└ " } else { "├ " };

    let addr_port = format!("{}:{}", profile.address, profile.port);
    let latency_w = if show_latency { LATENCY_WIDTH } else { 0 };
    let remaining = inner_width.saturating_sub(FIXED_OVERHEAD_BASE + latency_w);
    let name_width = remaining.saturating_sub(addr_width).max(MIN_NAME_WIDTH);

    let name_col = fit_to_visual_width(&profile.name, name_width);
    let protocol_col = fit_to_visual_width(profile.protocol_label(), PROTOCOL_WIDTH);
    let addr_col = truncate_to_visual_width(&addr_port, addr_width);

    let style = if is_selected && is_connected {
        theme.selected_connected()
    } else if is_selected {
        theme.selected()
    } else if is_connected {
        theme.success()
    } else {
        theme.normal()
    };

    let addr_col_padded = if is_selected || show_latency {
        pad_to_visual_width(&addr_col, addr_width)
    } else {
        addr_col
    };

    let used = PREFIX_WIDTH
        + name_width
        + 1
        + PROTOCOL_WIDTH
        + 1
        + visual_width(&addr_col_padded)
        + latency_w;
    let trailing = inner_width.saturating_sub(used);

    let mut spans = vec![
        Span::styled(prefix, style),
        Span::styled(name_col, style),
        Span::styled(" ", style),
        Span::styled(protocol_col, style),
        Span::styled(" ", style),
        Span::styled(addr_col_padded, style),
    ];
    if show_latency {
        let latency_text = if model.testing_profiles.contains(&profile.id) {
            format!(" {:<6}", "…")
        } else {
            match model.profile_latencies.get(&profile.id) {
                Some(&Some(ms)) => format!(" {:<6}", format!("{}ms", ms.min(9999))),
                Some(None) => format!(" {:<6}", "err"),
                None => " ".repeat(LATENCY_WIDTH),
            }
        };
        spans.push(Span::styled(latency_text, style));
    }
    if is_selected && trailing > 0 {
        spans.push(Span::styled(" ".repeat(trailing), style));
    }
    Line::from(spans)
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
    use crate::config::profile::Profile;
    use crate::test_helpers::{buffer_to_string, model_with_profiles};
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
    fn dns_preset_index_recognises_defaults() {
        use crate::config::profile::DnsConfig;
        let dns = DnsConfig::default();
        assert_eq!(current_dns_preset_index(&dns), Some(0));
    }

    #[test]
    fn dns_preset_index_recognises_quad9_doh() {
        use crate::config::profile::{DnsConfig, DnsServer, DnsStrategy};
        let dns = DnsConfig {
            servers: vec![
                DnsServer::Local {
                    tag: "local".to_string(),
                },
                DnsServer::Https {
                    tag: "remote".to_string(),
                    server: "9.9.9.9".to_string(),
                    server_port: None,
                    path: "/dns-query".to_string(),
                },
            ],
            rules: Vec::new(),
            final_server: "remote".to_string(),
            strategy: DnsStrategy::PreferIpv4,
            fakeip_enabled: false,
        };
        assert_eq!(current_dns_preset_index(&dns), Some(2));
    }

    #[test]
    fn dns_preset_index_recognises_system_only() {
        use crate::config::profile::{DnsConfig, DnsServer, DnsStrategy};
        let dns = DnsConfig {
            servers: vec![DnsServer::Local {
                tag: "local".to_string(),
            }],
            rules: Vec::new(),
            final_server: "local".to_string(),
            strategy: DnsStrategy::PreferIpv4,
            fakeip_enabled: false,
        };
        assert_eq!(current_dns_preset_index(&dns), Some(3));
    }

    #[test]
    fn dns_preset_index_ignores_fakeip_extras() {
        // Fake-IP servers must not affect preset detection — they sit alongside
        // any preset.
        use crate::config::profile::{DnsConfig, DnsServer};
        let mut dns = DnsConfig::default();
        dns.servers.push(DnsServer::FakeIp {
            tag: "fakeip".to_string(),
            inet4_range: "198.18.0.0/15".to_string(),
            inet6_range: "fc00::/18".to_string(),
        });
        dns.fakeip_enabled = true;
        assert_eq!(current_dns_preset_index(&dns), Some(0));
    }

    #[test]
    fn dns_preset_index_none_for_custom_endpoint() {
        use crate::config::profile::{DnsConfig, DnsServer, DnsStrategy};
        let dns = DnsConfig {
            servers: vec![
                DnsServer::Local {
                    tag: "local".to_string(),
                },
                DnsServer::Https {
                    tag: "remote".to_string(),
                    server: "94.140.14.14".to_string(),
                    server_port: None,
                    path: "/dns-query".to_string(),
                },
            ],
            rules: Vec::new(),
            final_server: "remote".to_string(),
            strategy: DnsStrategy::PreferIpv4,
            fakeip_enabled: false,
        };
        assert_eq!(current_dns_preset_index(&dns), None);
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
                draw_help(frame, &Theme::legacy(), area);
            })
            .unwrap();

        let content: String = frame.buffer.content.iter().map(|c| c.symbol()).collect();
        let expected = [
            ("j / Down", "Move down"),
            ("k / Up", "Move up"),
            ("g", "Go to first"),
            ("G", "Go to last"),
            ("Enter", "Connect to selected profile"),
            ("p", "Paste from clipboard"),
            ("d", "Delete selected source"),
            ("m", "Routing mode (popup list)"),
            ("u", "Update subscription or geo"),
            ("i", "Cycle subscription auto-update"),
            ("e", "Open profiles.json in $EDITOR"),
            ("C", "Theme picker"),
            ("t", "Test selected profile latency"),
            ("T", "Test all profiles (batch)"),
            ("r", "Reconnect"),
            ("s", "Stop / disconnect"),
            ("q / Esc", "Detach TUI"),
            ("Ctrl+C", "Quit"),
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
        let mut model = model_with_profiles(vec![Profile::new_vless(
            "Alpha".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        model.geo_last_updated = Some("2026-05-31 13:41".to_string());
        model.logs.push_back("log line 1".to_string());
        model.logs.push_back("log line 2".to_string());
        model.connection = ConnectionState::Connected;
        model.active_profile_id = Some(model.config.profiles[0].id);
        insta::assert_snapshot!(snapshot_terminal(&model, 80, 20));
    }

    #[test]
    fn draw_traffic_panel_snapshot() {
        let mut model = model_with_profiles(vec![Profile::new_vless(
            "Alpha".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        model.geo_last_updated = Some("2026-05-31 13:41".to_string());
        model.connection = ConnectionState::Connected;
        model.active_profile_id = Some(model.config.profiles[0].id);
        model.traffic = crate::app::model::TrafficStats {
            up_rate_bps: 12_345,
            down_rate_bps: 3_500_000,
            up_total: 142 * 1024 * 1024,
            down_total: 3 * 1024 * 1024 * 1024,
            conn_count: 18,
        };
        insta::assert_snapshot!(snapshot_terminal(&model, 100, 20));
    }

    #[test]
    fn draw_main_hides_traffic_panel_when_idle() {
        let mut model = model_with_profiles(vec![Profile::new_vless(
            "Alpha".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        // Connection state Idle → traffic panel must not appear.
        model.traffic = crate::app::model::TrafficStats {
            up_rate_bps: 1000,
            down_rate_bps: 1000,
            up_total: 1000,
            down_total: 1000,
            conn_count: 4,
        };
        let output = snapshot_terminal(&model, 100, 20);
        assert!(
            !output.contains("Traffic"),
            "traffic panel must be hidden when not connected: {}",
            output
        );
    }

    #[test]
    fn draw_help_overlay_snapshot() {
        let mut model = model_with_profiles(vec![]);
        model.geo_last_updated = Some("2026-05-31 13:41".to_string());
        model.overlay = Overlay::Help;
        insta::assert_snapshot!(snapshot_terminal(&model, 80, 40));
    }

    #[test]
    fn draw_confirm_delete_overlay_snapshot() {
        let mut model = model_with_profiles(vec![Profile::new_vless(
            "Alpha".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        model.geo_last_updated = Some("2026-05-31 13:41".to_string());
        model.overlay = Overlay::ConfirmDelete;
        insta::assert_snapshot!(snapshot_terminal(&model, 80, 20));
    }

    #[test]
    fn draw_routing_mode_overlay_snapshot() {
        let mut model = model_with_profiles(vec![]);
        model.geo_last_updated = Some("2026-05-31 13:41".to_string());
        model
            .config
            .settings
            .geo_routing
            .set_region(crate::config::profile::GeoRegion::Ru);
        model.overlay = Overlay::RoutingMode;
        model.routing_selected = 2;
        insta::assert_snapshot!(snapshot_terminal(&model, 80, 20));
    }

    #[test]
    fn draw_geo_region_overlay_snapshot() {
        let mut model = model_with_profiles(vec![]);
        model.geo_last_updated = Some("2026-05-31 13:41".to_string());
        model.overlay = Overlay::GeoRegions;
        model.geo_region_selected = 1;
        insta::assert_snapshot!(snapshot_terminal(&model, 80, 20));
    }

    #[test]
    fn draw_dns_settings_overlay_snapshot() {
        let mut model = model_with_profiles(vec![]);
        model.geo_last_updated = Some("2026-05-31 13:41".to_string());
        model.overlay = Overlay::DnsSettings;
        // Cursor on the "Strategy" row so it gets highlighted in the snapshot.
        model.dns_selected = 4;
        insta::assert_snapshot!(snapshot_terminal(&model, 80, 24));
    }

    /// Service routing overlay with the cursor on the second row and an
    /// uncommitted draft change (`*`). Covers the Direct and Proxy
    /// renderings; Disabled is pinned by the `_no_edits` snapshot below.
    #[test]
    fn draw_service_routing_overlay_snapshot() {
        use crate::config::profile::{RoutedService, ServiceRoute};
        use std::collections::HashMap;

        let mut model = model_with_profiles(vec![]);
        model.geo_last_updated = Some("2026-05-31 13:41".to_string());
        model.overlay = Overlay::ServiceRouting;
        model.service_routing_selected = 1;
        // Steam is committed as Direct; the Telegram→Proxy edit is
        // draft-only, so its row carries the dirty marker.
        model
            .config
            .settings
            .geo_routing
            .service_routes
            .insert(RoutedService::Steam, ServiceRoute::Direct);
        model.service_routing_draft = Some(HashMap::from([
            (RoutedService::Steam, ServiceRoute::Direct),
            (RoutedService::Telegram, ServiceRoute::Proxy),
        ]));
        insta::assert_snapshot!(snapshot_terminal(&model, 80, 24));
    }

    /// Freshly opened overlay with no committed routes: every row renders
    /// Disabled (an absent draft entry), no dirty markers.
    #[test]
    fn draw_service_routing_overlay_no_edits_snapshot() {
        use std::collections::HashMap;

        let mut model = model_with_profiles(vec![]);
        model.geo_last_updated = Some("2026-05-31 13:41".to_string());
        model.overlay = Overlay::ServiceRouting;
        model.service_routing_selected = 0;
        model.service_routing_draft = Some(HashMap::new());
        insta::assert_snapshot!(snapshot_terminal(&model, 80, 24));
    }

    /// Theme picker overlay rendered with the dark default palette.
    /// Pins the layout, label format, and active-row highlighting.
    #[test]
    fn draw_theme_settings_overlay_snapshot() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        unsafe { std::env::set_var("XDG_STATE_HOME", dir.path()) };
        let mut model = model_with_profiles(vec![]);
        model.geo_last_updated = Some("2026-05-31 13:41".to_string());
        model.overlay = Overlay::ThemeSettings;
        model.config.settings.theme = "gruvbox".into();
        let slugs = crate::app::update::theme_picker_slugs();
        model.theme_selected = slugs
            .iter()
            .position(|s| s == &model.config.settings.theme)
            .unwrap_or(0);
        insta::assert_snapshot!(snapshot_terminal(&model, 80, 32));
    }

    /// Theme picker rendered with a light palette — sanity check for
    /// contrast on backgrounds where the dark-mode defaults don't apply.
    #[test]
    fn draw_theme_settings_overlay_light_snapshot() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        unsafe { std::env::set_var("XDG_STATE_HOME", dir.path()) };
        let mut model = model_with_profiles(vec![]);
        model.geo_last_updated = Some("2026-05-31 13:41".to_string());
        model.overlay = Overlay::ThemeSettings;
        model.config.settings.theme = "catppuccin-latte".into();
        model.theme = crate::ui::styles::Theme::resolve(&model.config.settings.theme);
        let slugs = crate::app::update::theme_picker_slugs();
        model.theme_selected = slugs
            .iter()
            .position(|s| s == &model.config.settings.theme)
            .unwrap_or(0);
        insta::assert_snapshot!(snapshot_terminal(&model, 80, 32));
    }

    #[test]
    fn theme_picker_scrolls_to_keep_selection_and_footer_visible() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        unsafe { std::env::set_var("XDG_STATE_HOME", dir.path()) };
        let mut model = model_with_profiles(vec![]);
        model.overlay = Overlay::ThemeSettings;
        model.theme_selected = crate::app::update::theme_picker_slugs().len() - 1;

        let rendered = snapshot_terminal(&model, 80, 24);
        assert!(rendered.contains("> white"));
        assert!(rendered.contains("j/k navigate, Enter confirm, Esc cancel"));
        assert!(!rendered.contains("catppuccin-latte"));
    }

    #[test]
    fn draw_dns_settings_overlay_fakeip_on_snapshot() {
        let mut model = model_with_profiles(vec![]);
        model.geo_last_updated = Some("2026-05-31 13:41".to_string());
        model.config.settings.dns.fakeip_enabled = true;
        model
            .config
            .settings
            .dns
            .servers
            .push(crate::config::profile::DnsServer::FakeIp {
                tag: "fakeip".to_string(),
                inet4_range: "198.18.0.0/15".to_string(),
                inet6_range: "fc00::/18".to_string(),
            });
        model.overlay = Overlay::DnsSettings;
        model.dns_selected = 5;
        insta::assert_snapshot!(snapshot_terminal(&model, 80, 24));
    }

    #[test]
    fn draw_sources_snapshot() {
        use crate::config::profile::{Subscription, SubscriptionAutoUpdate};
        use uuid::Uuid;

        let sub_id = Uuid::new_v4();
        let profiles = vec![
            Profile::new_vless(
                "Alpha".to_string(),
                "1.1.1.1".to_string(),
                443,
                "u1".to_string(),
            ),
            Profile::new_vless(
                "Beta".to_string(),
                "2.2.2.2".to_string(),
                443,
                "u2".to_string(),
            ),
            {
                let mut p = Profile::new_vless(
                    "Gamma".to_string(),
                    "3.3.3.3".to_string(),
                    443,
                    "u3".to_string(),
                );
                p.subscription_id = Some(sub_id);
                p
            },
            {
                let mut p = Profile::new_vless(
                    "Delta".to_string(),
                    "4.4.4.4".to_string(),
                    443,
                    "u4".to_string(),
                );
                p.subscription_id = Some(sub_id);
                p
            },
        ];
        let mut model = model_with_profiles(profiles);
        model.geo_last_updated = Some("2026-05-31 13:41".to_string());
        model.config.subscriptions.push(Subscription {
            id: sub_id,
            name: "Example".to_string(),
            url: "http://example.com/sub".to_string(),
            auto_update: SubscriptionAutoUpdate::Every1h,
            last_updated: None,
        });
        model.selected = 0;
        insta::assert_snapshot!(snapshot_terminal(&model, 80, 20));
    }

    /// A very long hostname must not push the name column below MIN_NAME_WIDTH.
    #[test]
    fn draw_sources_long_address_does_not_hide_name() {
        let profiles = vec![
            Profile::new_vless(
                "MyProfile".to_string(),
                "very-long-hostname.infrastructure.example.com".to_string(),
                65535,
                "u1".to_string(),
            ),
            Profile::new_vless(
                "Short".to_string(),
                "1.1.1.1".to_string(),
                443,
                "u2".to_string(),
            ),
        ];
        let mut model = model_with_profiles(profiles);
        model.geo_last_updated = Some("2026-05-31 13:41".to_string());
        model.selected = 0;
        let output = snapshot_terminal(&model, 80, 20);
        // Name "MyProfile" must still be visible (truncated to MIN_NAME_WIDTH).
        assert!(
            output.contains("MyPro"),
            "name column vanished with long address"
        );
        insta::assert_snapshot!(output);
    }

    #[test]
    fn draw_sources_long_name_truncated() {
        let profiles = vec![
            Profile::new_vless(
                "VeryLongProfileNameThatMustBeTruncated".to_string(),
                "1.1.1.1".to_string(),
                443,
                "u1".to_string(),
            ),
            Profile::new_vless(
                "Second".to_string(),
                "2.2.2.2".to_string(),
                443,
                "u2".to_string(),
            ),
        ];
        let mut model = model_with_profiles(profiles);
        model.geo_last_updated = Some("2026-05-31 13:41".to_string());
        model.selected = 0;
        insta::assert_snapshot!(snapshot_terminal(&model, 80, 20));
    }

    /// Verify every protocol's UI badge renders without truncation.
    #[test]
    fn draw_sources_multi_protocol_badges_snapshot() {
        use crate::config::profile::{
            AnytlsConfig, HttpConfig, Hysteria2Config, ProtocolConfig, ShadowsocksCipher,
            ShadowsocksConfig, ShadowtlsConfig, SocksConfig, SshConfig, TrojanConfig, TuicConfig,
            VmessConfig,
        };
        use uuid::Uuid;

        let make = |name: &str, address: &str, port: u16, config: ProtocolConfig| Profile {
            id: Uuid::new_v4(),
            name: name.to_string(),
            address: address.to_string(),
            port,
            config,
            tags: vec![],
            subscription_id: None,
        };

        let profiles = vec![
            Profile::new_vless(
                "A-vless".to_string(),
                "1.1.1.1".to_string(),
                443,
                "u1".to_string(),
            ),
            make(
                "B-vmess",
                "2.2.2.2",
                443,
                ProtocolConfig::Vmess(VmessConfig {
                    uuid: "u2".to_string(),
                    ..Default::default()
                }),
            ),
            make(
                "C-trojan",
                "3.3.3.3",
                443,
                ProtocolConfig::Trojan(TrojanConfig {
                    password: "pw".to_string(),
                    ..Default::default()
                }),
            ),
            make(
                "D-ss",
                "4.4.4.4",
                8388,
                ProtocolConfig::Shadowsocks(ShadowsocksConfig {
                    method: ShadowsocksCipher::Chacha20IetfPoly1305,
                    password: "pw".to_string(),
                }),
            ),
            make(
                "E-hy2",
                "5.5.5.5",
                443,
                ProtocolConfig::Hysteria2(Hysteria2Config {
                    password: "pw".to_string(),
                    ..Default::default()
                }),
            ),
            make(
                "F-tuic",
                "6.6.6.6",
                443,
                ProtocolConfig::Tuic(TuicConfig {
                    uuid: "u6".to_string(),
                    password: "pw".to_string(),
                    ..Default::default()
                }),
            ),
            make(
                "G-stls",
                "7.7.7.7",
                443,
                ProtocolConfig::Shadowtls(ShadowtlsConfig {
                    password: "pw".to_string(),
                    ss_password: "sp".to_string(),
                    ..Default::default()
                }),
            ),
            make(
                "H-anytls",
                "8.8.8.8",
                443,
                ProtocolConfig::Anytls(AnytlsConfig {
                    password: "pw".to_string(),
                    ..Default::default()
                }),
            ),
            make(
                "I-socks",
                "9.9.9.9",
                1080,
                ProtocolConfig::Socks(SocksConfig::default()),
            ),
            make(
                "J-http",
                "10.0.0.1",
                8080,
                ProtocolConfig::Http(HttpConfig::default()),
            ),
            make(
                "K-ssh",
                "10.0.0.2",
                22,
                ProtocolConfig::Ssh(SshConfig {
                    user: "root".to_string(),
                    ..Default::default()
                }),
            ),
        ];
        let mut model = model_with_profiles(profiles);
        model.geo_last_updated = Some("2026-05-31 13:41".to_string());
        model.selected = 0;
        insta::assert_snapshot!(snapshot_terminal(&model, 80, 26));
    }

    #[test]
    fn draw_sources_connected_profile_colored() {
        let mut model = model_with_profiles(vec![
            Profile::new_vless(
                "Alpha".to_string(),
                "1.1.1.1".to_string(),
                443,
                "u1".to_string(),
            ),
            Profile::new_vless(
                "Beta".to_string(),
                "2.2.2.2".to_string(),
                443,
                "u2".to_string(),
            ),
        ]);
        model.geo_last_updated = Some("2026-05-31 13:41".to_string());
        model.connection = ConnectionState::Connected;
        model.active_profile_id = Some(model.config.profiles[1].id);
        model.selected = 1;
        insta::assert_snapshot!(snapshot_terminal(&model, 80, 20));
    }

    /// Empty Sources pane: pins the "No sources." placeholder at
    /// `draw_sources` L402–405, which currently has no visual regression guard.
    #[test]
    fn draw_sources_empty_state_snapshot() {
        let mut model = model_with_profiles(vec![]);
        model.geo_last_updated = Some("2026-05-31 13:41".to_string());
        insta::assert_snapshot!(snapshot_terminal(&model, 80, 20));
    }

    /// Routing-mode overlay in Global region: `available_modes()` returns a
    /// single entry, exercising the small-list rendering path.
    #[test]
    fn draw_routing_mode_overlay_global_snapshot() {
        let mut model = model_with_profiles(vec![]);
        model.geo_last_updated = Some("2026-05-31 13:41".to_string());
        model
            .config
            .settings
            .geo_routing
            .set_region(crate::config::profile::GeoRegion::Global);
        model.overlay = Overlay::RoutingMode;
        model.routing_selected = 0;
        insta::assert_snapshot!(snapshot_terminal(&model, 80, 20));
    }

    /// Pin the `[error]`-prefixed log styling path at `draw_main` L80–86.
    #[test]
    fn draw_main_error_log_styling_snapshot() {
        let mut model = model_with_profiles(vec![Profile::new_vless(
            "Alpha".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        model.geo_last_updated = Some("2026-05-31 13:41".to_string());
        model.connection = ConnectionState::Connected;
        model.active_profile_id = Some(model.config.profiles[0].id);
        model.logs.push_back("normal info line".to_string());
        model
            .logs
            .push_back("[error] sing-box exited with code 1".to_string());
        insta::assert_snapshot!(snapshot_terminal(&model, 80, 20));
    }

    /// Subscription rendered with a populated `last_updated` and a non-default
    /// `auto_update` interval — covers the subscription-header formatting path
    /// when `Every1d` is active.
    #[test]
    fn draw_sources_subscription_with_last_updated_snapshot() {
        use crate::config::profile::{Subscription, SubscriptionAutoUpdate};
        use chrono::TimeZone;
        use uuid::Uuid;

        let sub_id = Uuid::new_v4();
        let mut profile = Profile::new_vless(
            "Alpha".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        );
        profile.subscription_id = Some(sub_id);
        let mut model = model_with_profiles(vec![profile]);
        model.geo_last_updated = Some("2026-05-31 13:41".to_string());
        let last_updated = chrono::Local
            .with_ymd_and_hms(2026, 6, 14, 9, 30, 0)
            .unwrap();
        model.config.subscriptions.push(Subscription {
            id: sub_id,
            name: "Example".to_string(),
            url: "http://example.com/sub".to_string(),
            auto_update: SubscriptionAutoUpdate::Every1d,
            last_updated: Some(last_updated),
        });
        model.selected = 0;
        insta::assert_snapshot!(snapshot_terminal(&model, 80, 20));
    }
}
