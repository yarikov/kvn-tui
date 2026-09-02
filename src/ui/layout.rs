use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Row, Table, TableState, Wrap};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthChar;

use crate::app::model::{HelpMode, MainPaneFocus, Model, Overlay, SourceRow};
use crate::ui::styles::Theme;
use crate::ui::widgets::{
    StatusBar, format_bps_field, format_bytes_field, format_connections_field,
    format_connections_padding,
};

/// Height (including borders) of the full-width traffic header rendered at
/// the very top of the UI when the VPN is connected. One content row plus
/// top/bottom borders = 3 lines.
const TRAFFIC_PANEL_HEIGHT: u16 = 3;

pub(crate) const LOG_CURSOR_TIMEOUT: Duration = Duration::from_secs(15);

/// TUI-client-local keyboard state for the log pane. Log contents are local
/// to the client as well, so none of this belongs in the daemon snapshot.
#[derive(Debug, Clone, Default)]
pub(crate) struct LogNavigation {
    cursor: Option<usize>,
    anchor: Option<usize>,
    scroll_top_log: Option<usize>,
    last_activity: Option<Instant>,
}

impl LogNavigation {
    pub(crate) fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    pub(crate) fn is_visual(&self) -> bool {
        self.anchor.is_some()
    }

    pub(crate) fn select_edge(&mut self, viewport: &LogViewport, from_top: bool, now: Instant) {
        let cursor = if from_top {
            viewport.first_log_index()
        } else {
            viewport.last_log_index()
        };
        if let Some(cursor) = cursor {
            self.cursor = Some(cursor);
            self.scroll_top_log = viewport.first_log_index();
            self.last_activity = Some(now);
        }
    }

    pub(crate) fn select_buffer_edge(&mut self, log_count: usize, from_top: bool, now: Instant) {
        if log_count == 0 {
            return;
        }
        let cursor = if from_top { 0 } else { log_count - 1 };
        self.cursor = Some(cursor);
        self.scroll_top_log = Some(cursor);
        self.last_activity = Some(now);
    }

    pub(crate) fn move_by(&mut self, delta: isize, log_count: usize, now: Instant) {
        let Some(cursor) = self.cursor else {
            return;
        };
        if log_count == 0 {
            self.clear();
            return;
        }
        self.cursor = Some(cursor.saturating_add_signed(delta).min(log_count - 1));
        self.last_activity = Some(now);
    }

    pub(crate) fn enter_visual(&mut self, now: Instant) {
        if let Some(cursor) = self.cursor {
            self.anchor = Some(cursor);
            self.last_activity = Some(now);
        }
    }

    pub(crate) fn cancel_visual(&mut self) {
        self.anchor = None;
    }

    pub(crate) fn selected_range(&self) -> Option<std::ops::RangeInclusive<usize>> {
        let cursor = self.cursor?;
        let anchor = self.anchor.unwrap_or(cursor);
        Some(anchor.min(cursor)..=anchor.max(cursor))
    }

    pub(crate) fn selected_text(&self, model: &Model) -> Option<(String, usize)> {
        let range = self.selected_range()?;
        let lines = range
            .clone()
            .filter_map(|index| model.logs.get(index))
            .cloned()
            .collect::<Vec<_>>();
        (!lines.is_empty()).then(|| (lines.join("\n"), lines.len()))
    }

    pub(crate) fn copied(&mut self, now: Instant) {
        self.anchor = None;
        self.last_activity = Some(now);
    }

    pub(crate) fn expire_if_idle(&mut self, now: Instant) -> bool {
        if self
            .last_activity
            .is_some_and(|last| now.saturating_duration_since(last) >= LOG_CURSOR_TIMEOUT)
        {
            self.clear();
            true
        } else {
            false
        }
    }

    pub(crate) fn oldest_log_evicted(&mut self) {
        if self.cursor == Some(0) {
            self.clear();
            return;
        }
        self.cursor = self.cursor.map(|index| index - 1);
        self.anchor = self.anchor.and_then(|index| index.checked_sub(1));
        self.scroll_top_log = self
            .scroll_top_log
            .and_then(|index| index.checked_sub(1))
            .or(Some(0));
    }

    pub(crate) fn clear(&mut self) {
        self.cursor = None;
        self.anchor = None;
        self.scroll_top_log = None;
        self.last_activity = None;
    }

    fn contains(&self, log_index: usize) -> bool {
        self.selected_range()
            .is_some_and(|range| range.contains(&log_index))
    }

    fn set_scroll_top_from(&mut self, viewport: &LogViewport) {
        if self.cursor.is_some() {
            self.scroll_top_log = viewport.first_log_index();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct LogPoint {
    row: usize,
    column: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LogDisplayRow {
    text: String,
    hard_break_after: bool,
    error: bool,
    log_index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LogViewport {
    area: Rect,
    rows: Vec<LogDisplayRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LogSelection {
    viewport: LogViewport,
    anchor: LogPoint,
    focus: LogPoint,
}

impl LogSelection {
    pub(crate) fn start(viewport: LogViewport, column: u16, row: u16) -> Option<Self> {
        let point = viewport.point_at(column, row)?;
        Some(Self {
            viewport,
            anchor: point,
            focus: point,
        })
    }

    pub(crate) fn update(&mut self, column: u16, row: u16) {
        self.focus = self.viewport.clamped_point(column, row);
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.anchor == self.focus
    }

    pub(crate) fn text(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let (start, end) = self.bounds();
        let mut output = String::new();
        for row_index in start.row..=end.row {
            let row = &self.viewport.rows[row_index];
            let from = if row_index == start.row {
                start.column
            } else {
                0
            };
            let to = if row_index == end.row {
                end.column
            } else {
                u16::MAX
            };
            output.push_str(&text_between_columns(&row.text, from, to));
            if row_index < end.row && row.hard_break_after {
                output.push('\n');
            }
        }
        output
    }

    fn bounds(&self) -> (LogPoint, LogPoint) {
        if self.anchor <= self.focus {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }

    fn contains(&self, row: usize, column: u16, width: u16) -> bool {
        if self.is_empty() {
            return false;
        }
        let (start, end) = self.bounds();
        let point_end = column.saturating_add(width.saturating_sub(1));
        (row > start.row || (row == start.row && point_end >= start.column))
            && (row < end.row || (row == end.row && column <= end.column))
    }
}

impl LogViewport {
    pub(crate) fn contains(&self, column: u16, row: u16) -> bool {
        column >= self.area.x
            && column < self.area.x.saturating_add(self.area.width)
            && row >= self.area.y
            && row < self.area.y.saturating_add(self.area.height)
    }

    fn point_at(&self, column: u16, row: u16) -> Option<LogPoint> {
        if self.rows.is_empty()
            || !self.contains(column, row)
            || row.saturating_sub(self.area.y) as usize >= self.rows.len()
        {
            return None;
        }
        Some(self.clamped_point(column, row))
    }

    fn clamped_point(&self, column: u16, row: u16) -> LogPoint {
        let max_row = self.rows.len().saturating_sub(1);
        let local_row = row.saturating_sub(self.area.y) as usize;
        let row = local_row.min(max_row);
        let max_column = visual_width(&self.rows[row].text).saturating_sub(1) as u16;
        LogPoint {
            row,
            column: column.saturating_sub(self.area.x).min(max_column),
        }
    }

    fn first_log_index(&self) -> Option<usize> {
        self.rows.first().map(|row| row.log_index)
    }

    fn last_log_index(&self) -> Option<usize> {
        self.rows.last().map(|row| row.log_index)
    }
}

fn build_all_log_rows(model: &Model, width: usize) -> Vec<LogDisplayRow> {
    let mut rows = Vec::new();
    if width > 0 {
        for (log_index, line) in model.logs.iter().enumerate() {
            let error = line.starts_with("[error]");
            let wrapped = wrap_log_line(line, width);
            let last = wrapped.len().saturating_sub(1);
            rows.extend(
                wrapped
                    .into_iter()
                    .enumerate()
                    .map(|(index, text)| LogDisplayRow {
                        text,
                        hard_break_after: index == last,
                        error,
                        log_index,
                    }),
            );
        }
    }
    rows
}

fn build_log_viewport(
    model: &Model,
    area: Rect,
    navigation: Option<&LogNavigation>,
) -> LogViewport {
    let all_rows = build_all_log_rows(model, area.width as usize);
    let keep = area.height as usize;
    let auto_start = all_rows.len().saturating_sub(keep);
    let mut start = navigation
        .and_then(|nav| nav.cursor.map(|_| nav.scroll_top_log.unwrap_or(0)))
        .and_then(|top_log| all_rows.iter().position(|row| row.log_index == top_log))
        .unwrap_or(auto_start);

    if let Some(cursor) = navigation.and_then(LogNavigation::cursor)
        && let Some(first) = all_rows.iter().position(|row| row.log_index == cursor)
    {
        let last = all_rows
            .iter()
            .rposition(|row| row.log_index == cursor)
            .unwrap_or(first);
        if first < start {
            start = first;
        } else if last >= start.saturating_add(keep) {
            let candidate = last.saturating_add(1).saturating_sub(keep);
            // Prefer the first complete record boundary that still keeps
            // the cursor visible. This prevents a wrapped log from becoming
            // several keyboard navigation positions.
            start = (candidate..=first)
                .find(|&index| {
                    index == 0 || all_rows[index - 1].log_index != all_rows[index].log_index
                })
                .unwrap_or(first);
        }
    }

    start = start.min(all_rows.len().saturating_sub(keep));
    let rows = all_rows.into_iter().skip(start).take(keep).collect();
    LogViewport { area, rows }
}

fn wrap_log_line(line: &str, width: usize) -> Vec<String> {
    if line.is_empty() || width == 0 {
        return vec![String::new()];
    }
    let mut rows = vec![String::new()];
    let mut used = 0;
    for character in line.chars() {
        let char_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if used > 0 && used + char_width > width {
            rows.push(String::new());
            used = 0;
        }
        rows.last_mut().expect("one row exists").push(character);
        used += char_width;
    }
    rows
}

fn text_between_columns(text: &str, from: u16, to: u16) -> String {
    let mut result = String::new();
    let mut column = 0_u16;
    for character in text.chars() {
        let width = UnicodeWidthChar::width(character).unwrap_or(0) as u16;
        let end = column.saturating_add(width.saturating_sub(1));
        if end >= from && column <= to {
            result.push(character);
        }
        column = column.saturating_add(width);
    }
    result
}

fn log_display_line(
    model: &Model,
    row: &LogDisplayRow,
    row_index: usize,
    selection: Option<&LogSelection>,
    navigation: Option<&LogNavigation>,
) -> Line<'static> {
    let base = if row.error {
        model.theme.error()
    } else {
        model.theme.normal()
    };
    let mut column = 0_u16;
    let spans = row
        .text
        .chars()
        .map(|character| {
            let width = UnicodeWidthChar::width(character).unwrap_or(0) as u16;
            let selected = navigation.is_some_and(|nav| nav.contains(row.log_index))
                || selection.is_some_and(|selection| selection.contains(row_index, column, width));
            column = column.saturating_add(width);
            Span::styled(
                character.to_string(),
                if selected {
                    model.theme.selected()
                } else {
                    base
                },
            )
        })
        .collect::<Vec<_>>();
    Line::from(spans)
}

fn main_panes(terminal_area: Rect) -> (Rect, Rect) {
    let main_area = if terminal_area.height > TRAFFIC_PANEL_HEIGHT + 5 {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(TRAFFIC_PANEL_HEIGHT),
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .split(terminal_area)[1]
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .split(terminal_area)[0]
    };
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(main_area);
    (panes[0], panes[1])
}

pub(crate) fn log_viewport(model: &Model, terminal_area: Rect) -> Option<LogViewport> {
    log_viewport_with_navigation(model, terminal_area, None)
}

pub(crate) fn log_viewport_with_navigation(
    model: &Model,
    terminal_area: Rect,
    navigation: Option<&LogNavigation>,
) -> Option<LogViewport> {
    if model.overlay != Overlay::None {
        return None;
    }
    let (_, logs) = main_panes(terminal_area);
    let area = Rect::new(
        logs.x.saturating_add(1),
        logs.y.saturating_add(1),
        logs.width.saturating_sub(2),
        logs.height.saturating_sub(2),
    );
    if area.width == 0 || area.height == 0 {
        return None;
    }
    Some(build_log_viewport(model, area, navigation))
}

pub(crate) fn sync_log_scroll(model: &Model, terminal_area: Rect, navigation: &mut LogNavigation) {
    if let Some(viewport) = log_viewport_with_navigation(model, terminal_area, Some(navigation)) {
        navigation.set_scroll_top_from(&viewport);
    }
}

/// Return the selectable Sources row under a terminal cell. Borders, group
/// labels, separators, Logs, clipped rows, and every overlay are inert.
pub(crate) fn source_hit_test(
    model: &Model,
    terminal_area: Rect,
    column: u16,
    row: u16,
) -> Option<usize> {
    if model.overlay != Overlay::None {
        return None;
    }
    let (sources, _) = main_panes(terminal_area);
    let inside = column > sources.x
        && column < sources.x.saturating_add(sources.width).saturating_sub(1)
        && row > sources.y
        && row < sources.y.saturating_add(sources.height).saturating_sub(1);
    if !inside {
        return None;
    }

    let rows = model.source_rows();
    let mut visual_rows = Vec::new();
    let standalone: Vec<_> = rows
        .iter()
        .enumerate()
        .filter(|(_, source)| matches!(source, SourceRow::StandaloneProfile(_)))
        .map(|(index, _)| index)
        .collect();
    if !standalone.is_empty() {
        visual_rows.push(None); // "Standalone profiles" is only a label.
        visual_rows.extend(standalone.into_iter().map(Some));
        visual_rows.push(None);
    }
    for sub_idx in 0..model.config.subscriptions.len() {
        visual_rows.push(rows.iter().position(
            |source| matches!(source, SourceRow::SubscriptionHeader(index) if *index == sub_idx),
        ));
        visual_rows.extend(rows.iter().enumerate().filter_map(|(index, source)| {
            matches!(source, SourceRow::SubscriptionProfile { sub_idx: index_sub, .. } if *index_sub == sub_idx)
                .then_some(Some(index))
        }));
        visual_rows.push(None);
    }
    let line = row.saturating_sub(sources.y + 1) as usize;
    visual_rows.get(line).copied().flatten()
}

/// Render the full application UI into the terminal frame.
pub fn draw(frame: &mut Frame, model: &Model) {
    draw_with_interaction(frame, model, MainPaneFocus::Sources, None, None);
}

#[cfg(test)]
pub(crate) fn draw_with_log_selection(
    frame: &mut Frame,
    model: &Model,
    log_selection: Option<&LogSelection>,
) {
    draw_with_interaction(frame, model, MainPaneFocus::Sources, None, log_selection);
}

pub(crate) fn draw_with_interaction(
    frame: &mut Frame,
    model: &Model,
    pane_focus: MainPaneFocus,
    log_navigation: Option<&LogNavigation>,
    log_selection: Option<&LogSelection>,
) {
    let area = frame.area();

    // Paint the palette's background across the whole frame first so that
    // every widget below — most of which set only `fg` — inherits a
    // theme-consistent background instead of the terminal default. Popups
    // override with their own `popup_bg()` (currently the same color).
    frame.render_widget(Block::default().style(model.theme.background()), area);

    // Top-level vertical layout: traffic header, main content, status bar. Keep
    // the header visible while disconnected so the layout does not jump when
    // the connection state changes; only hide it in very short terminals.
    let show_traffic = area.height > TRAFFIC_PANEL_HEIGHT + 5;
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
    draw_main(
        frame,
        model,
        main_area,
        pane_focus,
        log_navigation,
        log_selection,
    );
    draw_status_bar(frame, model, status_area);

    match model.overlay {
        Overlay::Help(state) => draw_help(frame, model, state, area),
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
fn draw_main(
    frame: &mut Frame,
    model: &Model,
    area: Rect,
    pane_focus: MainPaneFocus,
    log_navigation: Option<&LogNavigation>,
    log_selection: Option<&LogSelection>,
) {
    let theme = &model.theme;
    let main_focus_active = model.overlay == Overlay::None;
    let content_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    draw_sources(
        frame,
        model,
        content_chunks[0],
        main_focus_active && pane_focus == MainPaneFocus::Sources,
    );

    let log_block = Block::default()
        .title(" Logs ")
        .borders(Borders::ALL)
        .border_style(if main_focus_active && pane_focus == MainPaneFocus::Logs {
            theme.accent()
        } else {
            theme.border()
        });

    let inner = Rect::new(
        content_chunks[1].x.saturating_add(1),
        content_chunks[1].y.saturating_add(1),
        content_chunks[1].width.saturating_sub(2),
        content_chunks[1].height.saturating_sub(2),
    );
    let viewport = log_selection
        .map(|selection| &selection.viewport)
        .filter(|viewport| viewport.area == inner)
        .cloned()
        .unwrap_or_else(|| build_log_viewport(model, inner, log_navigation));
    let log_text: Vec<Line> = viewport
        .rows
        .iter()
        .enumerate()
        .map(|(row_index, row)| {
            log_display_line(model, row, row_index, log_selection, log_navigation)
        })
        .collect();

    let logs = Paragraph::new(log_text).block(log_block);
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
        Span::styled(format_bps_field(t.up_rate_bps), theme.normal()),
        Span::raw("  "),
        Span::styled("↓ ", theme.accent()),
        Span::styled(format_bps_field(t.down_rate_bps), theme.normal()),
        Span::raw("    "),
        Span::styled("Total: ", theme.border()),
        Span::styled("↑ ", theme.success()),
        Span::styled(format_bytes_field(t.up_total), theme.normal()),
        Span::raw("  "),
        Span::styled("↓ ", theme.accent()),
        Span::styled(format_bytes_field(t.down_total), theme.normal()),
        Span::raw("    "),
        Span::styled(format_connections_field(t.conn_count), theme.accent()),
        Span::styled(" connections", theme.normal()),
        Span::raw(format_connections_padding(t.conn_count)),
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
fn draw_help(
    frame: &mut Frame,
    model: &Model,
    help_state: crate::app::model::HelpState,
    area: Rect,
) {
    let theme = &model.theme;
    let help_rows = crate::ui::help::rows(
        help_state,
        model.config.settings.geo_routing.current_region.is_some(),
    );
    let needed = help_rows.len() as u16 + 1 + 2 + 2;
    let percent = needed
        .saturating_mul(100)
        .checked_div(area.height)
        .unwrap_or(90)
        .clamp(50, 90);
    let width_percent = match help_state.mode {
        HelpMode::Context => 70,
        HelpMode::All => 80,
    };
    let popup_area = centered_rect(width_percent, percent, area);

    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .title(format!(
            " Help — {} ",
            match help_state.mode {
                HelpMode::Context => crate::ui::help::title(help_state.context),
                HelpMode::All => "All",
            }
        ))
        .borders(Borders::ALL)
        .border_style(theme.accent())
        .style(theme.popup_bg());

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(2)])
        .split(inner);
    let visible_count = chunks[0].height.saturating_sub(1) as usize;
    let selected = help_state.selected.min(help_rows.len().saturating_sub(1));
    let window_start = if help_rows.len() > visible_count {
        selected
            .saturating_sub(visible_count / 2)
            .min(help_rows.len() - visible_count)
    } else {
        0
    };
    let window_end = (window_start + visible_count).min(help_rows.len());
    let all_mode = help_state.mode == HelpMode::All;
    let header = if all_mode {
        Row::new(vec!["Context", "Key", "Action"])
    } else {
        Row::new(vec!["Key", "Action"])
    }
    .style(theme.accent().add_modifier(Modifier::BOLD));
    let rows = help_rows[window_start..window_end].iter().map(|row| {
        if all_mode {
            Row::new(vec![row.context, row.key, row.action])
        } else {
            Row::new(vec![row.key, row.action])
        }
    });
    let widths = if all_mode {
        vec![
            Constraint::Length(16),
            Constraint::Length(18),
            Constraint::Min(1),
        ]
    } else {
        vec![Constraint::Length(18), Constraint::Min(1)]
    };
    let table = Table::new(rows, widths)
        .header(header)
        .style(theme.normal())
        .row_highlight_style(theme.selected())
        .highlight_symbol("> ");
    let mut table_state = TableState::default().with_selected(Some(selected - window_start));
    frame.render_stateful_widget(table, chunks[0], &mut table_state);

    let mode_hint = match help_state.mode {
        HelpMode::Context => "Tab: show all commands",
        HelpMode::All => "Tab: show contextual commands",
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(mode_hint).centered(),
            Line::from("j/k scroll · gg/G edges · q/Esc/? back").centered(),
        ])
        .style(theme.normal()),
        chunks[1],
    );
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
            Line::from("y/Enter confirm, q/Esc cancel, ? help"),
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
        .border_style(theme.accent())
        .style(theme.popup_bg());

    let paragraph = Paragraph::new(lines)
        .block(block)
        .style(theme.normal())
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
        &["Enter confirm, q/Esc cancel, ? help"],
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
        if model.config.settings.geo_routing.current_region.is_some() {
            &["Enter confirm, q/Esc cancel, ? help"]
        } else {
            &["Enter confirm, ? help"]
        },
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
        &["Enter confirm, q/Esc cancel, ? help"],
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
        .border_style(theme.accent())
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
    lines.push(Line::from("Enter confirm, q/Esc cancel, ? help").centered());

    frame.render_widget(Paragraph::new(lines).style(theme.normal()), inner);
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
        &["Enter confirm, q/Esc cancel, ? help"],
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
    footer: &[&str],
) {
    let popup_area = centered_rect(POPUP_WIDTH_PERCENT, height_percent, area);
    let footer_height = footer.len() as u16;
    let max_visible_items = popup_area.height.saturating_sub(5 + footer_height) as usize;
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
    lines.extend(footer.iter().map(|text| Line::from(*text)));
    draw_modal(frame, theme, area, modal_title, lines, height_percent);
}

/// Draw the unified Sources list: standalone profiles and subscription trees.
fn draw_sources(frame: &mut Frame, model: &Model, area: Rect, focused: bool) {
    let theme = &model.theme;
    let block = Block::default()
        .title(" Sources ")
        .borders(Borders::ALL)
        .border_style(if focused {
            theme.accent()
        } else {
            theme.border()
        });

    let inner_width = area.width.saturating_sub(2) as usize;
    let mut lines: Vec<Line> = Vec::new();
    let rows = model.source_rows();

    if rows.is_empty() {
        lines.push(Line::from(Span::styled(
            "No sources. Press p to paste a profile or subscription URL from clipboard.",
            theme.normal(),
        )));
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

    fn mouse_model() -> Model {
        use crate::config::profile::{Subscription, SubscriptionAutoUpdate};
        use uuid::Uuid;

        let sub_id = Uuid::new_v4();
        let standalone = Profile::new_vless("A".into(), "a".into(), 1, "u".into());
        let mut nested = Profile::new_vless("B".into(), "b".into(), 2, "v".into());
        nested.subscription_id = Some(sub_id);
        let mut model = model_with_profiles(vec![standalone, nested]);
        model.config.subscriptions.push(Subscription {
            id: sub_id,
            name: "Sub".into(),
            url: "https://example.test".into(),
            auto_update: SubscriptionAutoUpdate::Off,
            last_updated: None,
            next_auto_update: None,
            retry_state: None,
            send_hwid: false,
            hwid: None,
        });
        model
    }

    #[test]
    fn source_hit_test_maps_rows_in_both_vertical_layouts() {
        let model = mouse_model();
        // Tall: traffic panel occupies rows 0..=2, Sources content starts at 4.
        assert_eq!(
            source_hit_test(&model, Rect::new(0, 0, 80, 20), 2, 5),
            Some(0)
        );
        assert_eq!(
            source_hit_test(&model, Rect::new(0, 0, 80, 20), 2, 7),
            Some(1)
        );
        assert_eq!(
            source_hit_test(&model, Rect::new(0, 0, 80, 20), 2, 8),
            Some(2)
        );
        // Short: traffic is hidden and Sources content starts at row 1.
        assert_eq!(
            source_hit_test(&model, Rect::new(0, 0, 80, 8), 2, 2),
            Some(0)
        );
        assert_eq!(
            source_hit_test(&model, Rect::new(0, 0, 80, 8), 2, 4),
            Some(1)
        );
        assert_eq!(
            source_hit_test(&model, Rect::new(0, 0, 80, 8), 2, 5),
            Some(2)
        );
    }

    #[test]
    fn source_hit_test_ignores_labels_borders_separators_logs_clipping_and_overlays() {
        let mut model = mouse_model();
        let area = Rect::new(0, 0, 80, 20);
        assert_eq!(source_hit_test(&model, area, 2, 4), None); // standalone label
        assert_eq!(source_hit_test(&model, area, 2, 6), None); // separator
        assert_eq!(source_hit_test(&model, area, 0, 5), None); // border
        assert_eq!(source_hit_test(&model, area, 50, 5), None); // Logs
        assert_eq!(source_hit_test(&model, Rect::new(0, 0, 80, 5), 2, 4), None);
        model.overlay = Overlay::Help(crate::app::model::HelpState::default());
        assert_eq!(source_hit_test(&model, area, 2, 5), None);
    }

    #[test]
    fn log_viewport_hit_test_excludes_border_and_overlay() {
        let mut model = mouse_model();
        model.push_log("hello".into());
        let area = Rect::new(0, 0, 80, 20);
        let viewport = log_viewport(&model, area).unwrap();
        assert!(viewport.contains(41, 4));
        assert!(!viewport.contains(40, 4));
        assert!(!viewport.contains(41, 3));
        model.overlay = Overlay::Help(crate::app::model::HelpState::default());
        assert!(log_viewport(&model, area).is_none());
    }

    #[test]
    fn log_navigation_starts_j_at_top_and_k_at_bottom_of_visible_logs() {
        let mut model = mouse_model();
        for index in 0..6 {
            model.push_log(format!("line {index}"));
        }
        let area = Rect::new(0, 0, 80, 9); // three log content rows
        let viewport = log_viewport(&model, area).unwrap();
        assert_eq!(viewport.first_log_index(), Some(3));
        assert_eq!(viewport.last_log_index(), Some(5));

        let now = Instant::now();
        let mut down = LogNavigation::default();
        down.select_edge(&viewport, true, now);
        assert_eq!(down.cursor(), Some(3));

        let mut up = LogNavigation::default();
        up.select_edge(&viewport, false, now);
        assert_eq!(up.cursor(), Some(5));
    }

    #[test]
    fn log_navigation_moves_by_whole_wrapped_records() {
        let mut model = mouse_model();
        model.push_log("abcdefgh".into());
        model.push_log("next".into());
        let viewport = build_log_viewport(&model, Rect::new(0, 0, 4, 3), None);
        assert_eq!(viewport.rows.len(), 3);
        assert_eq!(viewport.rows[0].log_index, 0);
        assert_eq!(viewport.rows[1].log_index, 0);

        let now = Instant::now();
        let mut navigation = LogNavigation::default();
        navigation.select_edge(&viewport, true, now);
        assert_eq!(navigation.cursor(), Some(0));
        navigation.move_by(1, model.logs.len(), now);
        assert_eq!(navigation.cursor(), Some(1));
        assert_eq!(navigation.selected_text(&model), Some(("next".into(), 1)));
    }

    #[test]
    fn log_visual_selection_copies_complete_records_in_both_directions() {
        let mut model = mouse_model();
        for line in ["first", "second", "third"] {
            model.push_log(line.into());
        }
        let viewport = build_log_viewport(&model, Rect::new(0, 0, 20, 3), None);
        let now = Instant::now();
        let mut navigation = LogNavigation::default();
        navigation.select_edge(&viewport, false, now);
        navigation.enter_visual(now);
        navigation.move_by(-2, model.logs.len(), now);
        assert_eq!(
            navigation.selected_text(&model),
            Some(("first\nsecond\nthird".into(), 3))
        );

        navigation.copied(now);
        assert!(!navigation.is_visual());
        assert_eq!(navigation.cursor(), Some(0));
    }

    #[test]
    fn log_navigation_expires_after_fifteen_seconds() {
        let mut model = mouse_model();
        model.push_log("line".into());
        let viewport = build_log_viewport(&model, Rect::new(0, 0, 20, 1), None);
        let start = Instant::now();
        let mut navigation = LogNavigation::default();
        navigation.select_edge(&viewport, true, start);
        navigation.enter_visual(start);

        assert!(!navigation.expire_if_idle(start + LOG_CURSOR_TIMEOUT - Duration::from_millis(1)));
        assert!(navigation.is_visual());
        assert!(navigation.expire_if_idle(start + LOG_CURSOR_TIMEOUT));
        assert_eq!(navigation.cursor(), None);
        assert!(!navigation.is_visual());
    }

    #[test]
    fn log_buffer_edge_jumps_scroll_to_full_buffer_bounds() {
        let mut model = mouse_model();
        for index in 0..6 {
            model.push_log(format!("line {index}"));
        }
        let now = Instant::now();
        let mut navigation = LogNavigation::default();
        navigation.select_buffer_edge(model.logs.len(), true, now);
        let viewport = build_log_viewport(&model, Rect::new(0, 0, 20, 3), Some(&navigation));
        assert_eq!(navigation.cursor(), Some(0));
        assert_eq!(viewport.first_log_index(), Some(0));

        navigation.select_buffer_edge(model.logs.len(), false, now);
        let viewport = build_log_viewport(&model, Rect::new(0, 0, 20, 3), Some(&navigation));
        assert_eq!(navigation.cursor(), Some(5));
        assert_eq!(viewport.last_log_index(), Some(5));
    }

    #[test]
    fn log_visual_buffer_jumps_extend_beyond_viewport() {
        let mut model = mouse_model();
        for index in 0..6 {
            model.push_log(format!("line {index}"));
        }
        let now = Instant::now();
        let mut navigation = LogNavigation::default();
        navigation.select_buffer_edge(model.logs.len(), false, now);
        navigation.move_by(-1, model.logs.len(), now);
        navigation.enter_visual(now);

        navigation.select_buffer_edge(model.logs.len(), true, now);
        assert_eq!(navigation.selected_range(), Some(0..=4));
        assert_eq!(navigation.selected_text(&model).unwrap().1, 5);
        navigation.select_buffer_edge(model.logs.len(), false, now);
        assert_eq!(navigation.selected_range(), Some(4..=5));
    }

    #[test]
    fn log_buffer_edge_jump_is_noop_when_empty() {
        let mut navigation = LogNavigation::default();
        navigation.select_buffer_edge(0, true, Instant::now());
        assert_eq!(navigation.cursor(), None);
    }

    #[test]
    fn log_navigation_tracks_oldest_buffer_eviction() {
        let mut model = mouse_model();
        for line in ["old", "selected", "new"] {
            model.push_log(line.into());
        }
        let viewport = build_log_viewport(&model, Rect::new(0, 0, 20, 3), None);
        let now = Instant::now();
        let mut navigation = LogNavigation::default();
        navigation.select_edge(&viewport, true, now);
        navigation.move_by(1, model.logs.len(), now);
        navigation.oldest_log_evicted();
        model.logs.pop_front();
        assert_eq!(navigation.cursor(), Some(0));
        assert_eq!(
            navigation.selected_text(&model),
            Some(("selected".into(), 1))
        );
    }

    #[test]
    fn log_selection_joins_soft_wraps_and_preserves_real_line_breaks() {
        let mut model = mouse_model();
        model.push_log("abcdef".into());
        model.push_log("ghi".into());
        let viewport = build_log_viewport(&model, Rect::new(10, 5, 3, 3), None);
        assert_eq!(viewport.rows.len(), 3);
        let mut selection = LogSelection::start(viewport, 10, 5).unwrap();
        selection.update(12, 7);
        assert_eq!(selection.text(), "abcdef\nghi");
    }

    #[test]
    fn log_selection_works_backwards_and_clamps_to_log_bounds() {
        let mut model = mouse_model();
        model.push_log("abc".into());
        model.push_log("def".into());
        let viewport = build_log_viewport(&model, Rect::new(10, 5, 3, 2), None);
        let mut selection = LogSelection::start(viewport, 12, 6).unwrap();
        selection.update(0, 0);
        assert_eq!(selection.text(), "abc\ndef");
    }

    #[test]
    fn log_selection_does_not_split_wide_unicode_characters() {
        let mut model = mouse_model();
        model.push_log("a界b".into());
        let viewport = build_log_viewport(&model, Rect::new(0, 0, 4, 1), None);
        let mut selection = LogSelection::start(viewport, 1, 0).unwrap();
        selection.update(2, 0);
        assert_eq!(selection.text(), "界");
    }

    #[test]
    fn log_selection_single_click_is_empty() {
        let mut model = mouse_model();
        model.push_log("abc".into());
        let viewport = build_log_viewport(&model, Rect::new(0, 0, 3, 1), None);
        let selection = LogSelection::start(viewport, 1, 0).unwrap();
        assert!(selection.is_empty());
        assert!(selection.text().is_empty());
    }

    #[test]
    fn active_log_selection_freezes_viewport_until_it_is_cleared() {
        let mut model = mouse_model();
        model.push_log("visible before drag".into());
        let area = Rect::new(0, 0, 80, 20);
        let viewport = log_viewport(&model, area).unwrap();
        let mut selection = LogSelection::start(viewport, 41, 4).unwrap();
        selection.update(42, 4);

        model.push_log("arrived during drag".into());
        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| draw_with_log_selection(frame, &model, Some(&selection)))
            .unwrap();
        let frozen = buffer_to_string(terminal.backend().buffer());
        assert!(frozen.contains("visible before drag"));
        assert!(!frozen.contains("arrived during drag"));

        terminal.draw(|frame| draw(frame, &model)).unwrap();
        let current = buffer_to_string(terminal.backend().buffer());
        assert!(current.contains("arrived during drag"));
    }

    fn snapshot_terminal(model: &Model, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let frame = terminal.draw(|f| draw(f, model)).unwrap();
        buffer_to_string(frame.buffer)
    }

    fn find_text(buffer: &ratatui::buffer::Buffer, needle: &str) -> usize {
        buffer
            .content
            .windows(needle.len())
            .position(|cells| {
                cells
                    .iter()
                    .map(ratatui::buffer::Cell::symbol)
                    .collect::<String>()
                    == needle
            })
            .unwrap_or_else(|| panic!("{needle:?} should be rendered"))
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
        let model = model_with_profiles(vec![]);
        let state = crate::app::model::HelpState::default();
        let frame = terminal
            .draw(|frame| {
                let area = frame.area();
                draw_help(frame, &model, state, area);
            })
            .unwrap();

        let content: String = frame.buffer.content.iter().map(|c| c.symbol()).collect();
        let expected = [
            ("h/l / Left/Right", "Focus Sources / Logs"),
            ("j / Down", "Move down"),
            ("k / Up", "Move up"),
            ("gg", "Go to first"),
            ("G", "Go to last"),
            ("Enter", "Connect to selected profile"),
            ("p", "Paste from clipboard"),
            ("d", "Delete selected source"),
            ("m", "Routing mode"),
            ("u", "Update subscription or geo"),
            ("i", "Cycle subscription auto-update"),
            ("e", "Open profiles.json in $EDITOR"),
            ("C", "Theme picker"),
            ("t", "Test selected profile latency"),
            ("T", "Test all profiles"),
            ("r", "Reconnect"),
            ("s", "Disconnect"),
            ("q / Esc", "Detach TUI"),
            ("Ctrl+C", "Quit"),
            ("?", "Show help"),
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
    fn focused_main_pane_uses_accent_border() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use ratatui::style::Color;

        let model = mouse_model();
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        let navigation = LogNavigation::default();
        terminal
            .draw(|frame| {
                draw_with_interaction(frame, &model, MainPaneFocus::Logs, Some(&navigation), None)
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        let source_border = &buffer.content[3 * 80];
        let log_border = &buffer.content[3 * 80 + 40];
        assert_eq!(source_border.style().fg, Some(Color::DarkGray));
        assert_eq!(log_border.style().fg, Some(Color::Cyan));
    }

    #[test]
    fn overlay_focus_temporarily_suspends_and_restores_main_pane_focus() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use ratatui::style::Color;

        for pane_focus in [MainPaneFocus::Sources, MainPaneFocus::Logs] {
            let mut model = mouse_model();
            model.overlay = Overlay::ConfirmDelete;
            let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
            terminal
                .draw(|frame| {
                    draw_with_interaction(frame, &model, pane_focus, None, None);
                })
                .unwrap();

            let buffer = terminal.backend().buffer();
            assert_eq!(buffer.content[3 * 80].style().fg, Some(Color::DarkGray));
            assert_eq!(
                buffer.content[3 * 80 + 79].style().fg,
                Some(Color::DarkGray)
            );

            model.overlay = Overlay::None;
            terminal
                .draw(|frame| {
                    draw_with_interaction(frame, &model, pane_focus, None, None);
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            let focused_border = match pane_focus {
                MainPaneFocus::Sources => &buffer.content[3 * 80],
                MainPaneFocus::Logs => &buffer.content[3 * 80 + 40],
            };
            assert_eq!(focused_border.style().fg, Some(Color::Cyan));
        }
    }

    #[test]
    fn every_overlay_border_renderer_uses_accent() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use ratatui::style::Color;

        for (overlay, width_percent, height_percent) in [
            (
                Overlay::ConfirmDelete,
                POPUP_WIDTH_PERCENT,
                POPUP_HEIGHT_PERCENT,
            ),
            (
                Overlay::Help(crate::app::model::HelpState::default()),
                70,
                90,
            ),
            (
                Overlay::ServiceRouting,
                POPUP_WIDTH_PERCENT,
                POPUP_HEIGHT_PERCENT,
            ),
        ] {
            let mut model = mouse_model();
            model.overlay = overlay;
            let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
            terminal
                .draw(|frame| {
                    draw_with_interaction(frame, &model, MainPaneFocus::Logs, None, None);
                })
                .unwrap();

            let popup = centered_rect(width_percent, height_percent, Rect::new(0, 0, 80, 20));
            let corner =
                &terminal.backend().buffer().content[popup.y as usize * 80 + popup.x as usize];
            assert_eq!(corner.style().fg, Some(Color::Cyan), "overlay: {overlay:?}");
        }
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
    fn draw_main_shows_zeroed_traffic_panel_when_idle() {
        let model = model_with_profiles(vec![Profile::new_vless(
            "Alpha".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        let output = snapshot_terminal(&model, 100, 20);
        assert!(
            output.contains("Traffic") && output.contains("0.0 KB/s"),
            "traffic panel must show zeroed stats while disconnected: {}",
            output
        );
    }

    #[test]
    fn draw_help_overlay_snapshot() {
        let mut model = model_with_profiles(vec![]);
        model.geo_last_updated = Some("2026-05-31 13:41".to_string());
        model.overlay = Overlay::Help(crate::app::model::HelpState::default());
        insta::assert_snapshot!(snapshot_terminal(&model, 80, 40));
    }

    #[test]
    fn draw_all_help_overlay_snapshot() {
        let mut model = model_with_profiles(vec![]);
        model.overlay = Overlay::Help(crate::app::model::HelpState {
            context: crate::app::model::HelpContext::Logs,
            mode: HelpMode::All,
            selected: 12,
        });
        insta::assert_snapshot!(snapshot_terminal(&model, 100, 24));
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
    fn light_overlay_footer_uses_theme_foreground() {
        let mut model = model_with_profiles(vec![Profile::new_vless(
            "Alpha".to_string(),
            "1.1.1.1".to_string(),
            443,
            "u1".to_string(),
        )]);
        model.overlay = Overlay::ConfirmDelete;
        model.theme = crate::ui::styles::Theme::resolve("catppuccin-latte");

        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal.draw(|frame| draw(frame, &model)).unwrap();
        let buffer = terminal.backend().buffer();
        let footer_start = find_text(buffer, "y/Enter");

        assert_eq!(
            buffer.content[footer_start].style().fg,
            model.theme.normal().fg
        );
    }

    #[test]
    fn light_theme_uses_theme_foreground_for_help_rows_and_empty_sources() {
        let mut model = model_with_profiles(vec![]);
        model.theme = crate::ui::styles::Theme::resolve("catppuccin-latte");
        model.overlay = Overlay::Help(crate::app::model::HelpState::default());

        let mut terminal = Terminal::new(TestBackend::new(80, 40)).unwrap();
        terminal.draw(|frame| draw(frame, &model)).unwrap();
        let buffer = terminal.backend().buffer();
        let help_row = find_text(buffer, "Move up");
        assert_eq!(buffer.content[help_row].style().fg, model.theme.normal().fg);

        model.overlay = Overlay::None;
        terminal.draw(|frame| draw(frame, &model)).unwrap();
        let buffer = terminal.backend().buffer();
        let empty_state = find_text(buffer, "No sources");
        assert_eq!(
            buffer.content[empty_state].style().fg,
            model.theme.normal().fg
        );
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
    fn geo_region_footer_only_offers_cancel_after_initial_selection() {
        let mut model = model_with_profiles(vec![]);
        model.overlay = Overlay::GeoRegions;

        let required = snapshot_terminal(&model, 80, 20);
        assert!(required.contains("Enter confirm, ? help"));
        assert!(!required.contains("q/Esc cancel"));
        assert!(!required.contains("j/k navigate"));

        model
            .config
            .settings
            .geo_routing
            .set_region(crate::config::profile::GeoRegion::Ru);
        let optional = snapshot_terminal(&model, 80, 20);
        assert!(optional.contains("Enter confirm, q/Esc cancel, ? help"));
        assert!(!optional.contains("j/k navigate"));
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
        assert!(rendered.contains("Enter confirm, q/Esc cancel, ? help"));
        assert!(!rendered.contains("j/k navigate"));
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
            auto_update: SubscriptionAutoUpdate::Every1d,
            last_updated: None,
            next_auto_update: None,
            retry_state: None,
            send_hwid: false,
            hwid: None,
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
            next_auto_update: None,
            retry_state: None,
            send_hwid: false,
            hwid: None,
        });
        model.selected = 0;
        insta::assert_snapshot!(snapshot_terminal(&model, 80, 20));
    }
}
