use crate::app::model::HelpContext;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HelpLine {
    Heading(&'static str),
    Separator,
    Command {
        key: &'static str,
        action: &'static str,
    },
}

impl HelpLine {
    pub(crate) fn is_command(self) -> bool {
        matches!(self, Self::Command { .. })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HelpGroup {
    Navigation,
    Sources,
    Logs,
    Connection,
    Settings,
    Dialogs,
    General,
}

const BASE_ORDER: &[HelpGroup] = &[
    HelpGroup::Sources,
    HelpGroup::Logs,
    HelpGroup::Connection,
    HelpGroup::Settings,
    HelpGroup::Dialogs,
    HelpGroup::General,
];

const NAVIGATION: &[(&str, &str)] = &[
    ("h/l, ←/→", "Focus panes / change value"),
    ("j/k, ↑/↓", "Move or scroll"),
    ("gg/G", "Go to first / last"),
];

const SOURCES: &[(&str, &str)] = &[
    ("Enter", "Connect selected profile"),
    ("e", "Open profiles.json in $EDITOR"),
    ("y", "Yank selected source"),
    ("p", "Paste source from clipboard"),
    ("d", "Delete selected source"),
    ("u", "Update subscription or geo"),
    ("i/I", "Cycle subscription / geo auto-update"),
    ("t/T", "Test selected / all profiles"),
];

const LOGS: &[(&str, &str)] = &[
    ("y", "Yank selected log(s)"),
    ("Shift+V", "Select multiple logs"),
    ("Esc", "Cancel log selection"),
];

const CONNECTION: &[(&str, &str)] = &[
    ("r", "Reconnect"),
    ("s", "Disconnect"),
    ("a", "Toggle auto-connect"),
    ("K", "Toggle kill switch"),
];

const SETTINGS: &[(&str, &str)] = &[
    ("m", "Routing mode"),
    ("o", "Geo region"),
    ("D", "DNS settings"),
    ("S", "Service routing"),
    ("C", "Theme picker"),
];

const DIALOGS: &[(&str, &str)] = &[
    ("h/l, ←/→", "Change selected value"),
    ("Enter", "Confirm selection or changes"),
    ("y/n", "Confirm / cancel deletion"),
    ("q/Esc", "Cancel dialog"),
];

const GENERAL: &[(&str, &str)] = &[
    ("q/Esc", "Detach TUI from main screen"),
    ("Ctrl+C", "Quit daemon"),
    ("?", "Open or close help"),
];

impl HelpGroup {
    fn title(self) -> &'static str {
        match self {
            Self::Navigation => "Navigation",
            Self::Sources => "Sources",
            Self::Logs => "Logs",
            Self::Connection => "Connection",
            Self::Settings => "Settings",
            Self::Dialogs => "Dialogs",
            Self::General => "General",
        }
    }

    fn commands(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Navigation => NAVIGATION,
            Self::Sources => SOURCES,
            Self::Logs => LOGS,
            Self::Connection => CONNECTION,
            Self::Settings => SETTINGS,
            Self::Dialogs => DIALOGS,
            Self::General => GENERAL,
        }
    }
}

fn relevant_group(context: HelpContext) -> HelpGroup {
    match context {
        HelpContext::Sources => HelpGroup::Sources,
        HelpContext::Logs => HelpGroup::Logs,
        HelpContext::ConfirmDelete
        | HelpContext::RoutingMode
        | HelpContext::GeoRegions
        | HelpContext::DnsSettings
        | HelpContext::ThemeSettings
        | HelpContext::ServiceRouting => HelpGroup::Dialogs,
    }
}

fn append_group(lines: &mut Vec<HelpLine>, group: HelpGroup) {
    if !lines.is_empty() {
        lines.push(HelpLine::Separator);
    }
    lines.push(HelpLine::Heading(group.title()));
    lines.extend(
        group
            .commands()
            .iter()
            .map(|&(key, action)| HelpLine::Command { key, action }),
    );
}

pub(crate) fn rows(context: HelpContext) -> Vec<HelpLine> {
    let relevant = relevant_group(context);
    let mut lines = Vec::new();
    append_group(&mut lines, HelpGroup::Navigation);
    append_group(&mut lines, relevant);
    for &group in BASE_ORDER {
        if group != relevant {
            append_group(&mut lines, group);
        }
    }
    lines
}

pub(crate) fn first_command(lines: &[HelpLine]) -> usize {
    lines.iter().position(|line| line.is_command()).unwrap_or(0)
}

pub(crate) fn last_command(lines: &[HelpLine]) -> usize {
    lines
        .iter()
        .rposition(|line| line.is_command())
        .unwrap_or(0)
}

pub(crate) fn next_command(lines: &[HelpLine], selected: usize) -> usize {
    lines
        .iter()
        .enumerate()
        .skip(selected.saturating_add(1))
        .find_map(|(index, line)| line.is_command().then_some(index))
        .unwrap_or_else(|| last_command(lines))
}

pub(crate) fn previous_command(lines: &[HelpLine], selected: usize) -> usize {
    lines
        .iter()
        .enumerate()
        .take(selected)
        .rev()
        .find_map(|(index, line)| line.is_command().then_some(index))
        .unwrap_or_else(|| first_command(lines))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headings(context: HelpContext) -> Vec<&'static str> {
        rows(context)
            .into_iter()
            .filter_map(|line| match line {
                HelpLine::Heading(title) => Some(title),
                HelpLine::Separator | HelpLine::Command { .. } => None,
            })
            .collect()
    }

    #[test]
    fn relevant_group_follows_navigation() {
        assert_eq!(
            headings(HelpContext::Sources),
            [
                "Navigation",
                "Sources",
                "Logs",
                "Connection",
                "Settings",
                "Dialogs",
                "General",
            ]
        );
        assert_eq!(
            headings(HelpContext::Logs),
            [
                "Navigation",
                "Logs",
                "Sources",
                "Connection",
                "Settings",
                "Dialogs",
                "General",
            ]
        );
        assert_eq!(
            headings(HelpContext::DnsSettings),
            [
                "Navigation",
                "Dialogs",
                "Sources",
                "Logs",
                "Connection",
                "Settings",
                "General",
            ]
        );
    }

    #[test]
    fn every_group_is_present_once() {
        for context in [
            HelpContext::Sources,
            HelpContext::Logs,
            HelpContext::ConfirmDelete,
            HelpContext::RoutingMode,
            HelpContext::GeoRegions,
            HelpContext::DnsSettings,
            HelpContext::ThemeSettings,
            HelpContext::ServiceRouting,
        ] {
            let headings = headings(context);
            assert_eq!(headings.len(), 7);
            for expected in [
                "Navigation",
                "Sources",
                "Logs",
                "Connection",
                "Settings",
                "Dialogs",
                "General",
            ] {
                assert_eq!(
                    headings.iter().filter(|&&title| title == expected).count(),
                    1
                );
            }
        }
    }

    #[test]
    fn navigation_skips_headings_and_stops_at_edges() {
        let lines = rows(HelpContext::Sources);
        let first = first_command(&lines);
        let last = last_command(&lines);
        assert!(lines[first].is_command());
        assert!(lines[last].is_command());
        assert_eq!(previous_command(&lines, first), first);
        assert_eq!(next_command(&lines, last), last);
    }

    #[test]
    fn groups_have_single_separators_between_them() {
        let lines = rows(HelpContext::Sources);
        assert!(!matches!(lines.first(), Some(HelpLine::Separator)));
        assert!(!matches!(lines.last(), Some(HelpLine::Separator)));
        assert_eq!(
            lines
                .iter()
                .filter(|line| matches!(line, HelpLine::Separator))
                .count(),
            6
        );
        assert!(
            lines
                .windows(2)
                .all(|pair| !matches!(pair, [HelpLine::Separator, HelpLine::Separator]))
        );
    }

    #[test]
    fn general_orders_quit_before_help() {
        assert_eq!(GENERAL[1], ("Ctrl+C", "Quit daemon"));
        assert_eq!(GENERAL[2], ("?", "Open or close help"));
    }
}
