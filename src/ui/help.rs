use crate::app::model::{HelpContext, HelpMode, HelpState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HelpRow {
    pub context: &'static str,
    pub key: &'static str,
    pub action: &'static str,
}

const GLOBAL: &[(&str, &str)] = &[("?", "Show help"), ("Ctrl+C", "Quit daemon")];

const SOURCES: &[(&str, &str)] = &[
    ("Enter", "Connect to selected profile"),
    ("p", "Paste from clipboard"),
    ("y", "Yank selected source"),
    ("d", "Delete selected source"),
    ("u", "Update subscription or geo"),
    ("i", "Cycle subscription auto-update"),
    ("e", "Open profiles.json in $EDITOR"),
    ("t", "Test selected profile latency"),
    ("T", "Test all profiles"),
    ("r", "Reconnect"),
    ("s", "Disconnect"),
    ("a", "Toggle auto-connect"),
    ("K", "Toggle kill switch"),
    ("m", "Routing mode"),
    ("o", "Geo region"),
    ("D", "DNS settings"),
    ("S", "Service routing"),
    ("C", "Theme picker"),
    ("I", "Cycle geo auto-update"),
    ("h/l / Left/Right", "Focus Sources / Logs"),
    ("j / Down", "Move down"),
    ("k / Up", "Move up"),
    ("gg", "Go to first"),
    ("G", "Go to last"),
    ("q / Esc", "Detach TUI"),
];

const LOGS: &[(&str, &str)] = &[
    ("Shift+V", "Select multiple logs"),
    ("y", "Yank selected log(s)"),
    ("Esc", "Cancel log selection"),
    ("r", "Reconnect"),
    ("s", "Disconnect"),
    ("a", "Toggle auto-connect"),
    ("K", "Toggle kill switch"),
    ("m", "Routing mode"),
    ("o", "Geo region"),
    ("D", "DNS settings"),
    ("S", "Service routing"),
    ("C", "Theme picker"),
    ("I", "Cycle geo auto-update"),
    ("h/l / Left/Right", "Focus Sources / Logs"),
    ("j / Down", "Move down"),
    ("k / Up", "Move up"),
    ("gg", "Go to buffer top"),
    ("G", "Go to buffer bottom"),
    ("q / Esc", "Detach TUI"),
];

const CONFIRM_DELETE: &[(&str, &str)] = &[("y / Enter", "Confirm deletion"), ("q / Esc", "Cancel")];

const PICKER: &[(&str, &str)] = &[
    ("Enter", "Confirm"),
    ("j / Down", "Move down"),
    ("k / Up", "Move up"),
    ("gg", "Go to first"),
    ("G", "Go to last"),
    ("q / Esc", "Cancel"),
];

const DNS: &[(&str, &str)] = &[
    ("h/l / Left/Right", "Change selected value"),
    ("Enter", "Confirm"),
    ("j / Down", "Move down"),
    ("k / Up", "Move up"),
    ("gg", "Go to first"),
    ("G", "Go to last"),
    ("q / Esc", "Cancel"),
];

const SERVICES: &[(&str, &str)] = &[
    ("h/l / Left/Right", "Change selected route"),
    ("Enter", "Confirm all changes"),
    ("j / Down", "Move down"),
    ("k / Up", "Move up"),
    ("gg", "Go to first"),
    ("G", "Go to last"),
    ("q / Esc", "Cancel"),
];

/// Compact, purpose-grouped catalog for the All view. This is intentionally
/// curated separately from the contextual lists: repeating j/k and the main
/// pane shortcuts under every overlay makes the complete reference noisy.
const ALL_ROWS: &[HelpRow] = &[
    HelpRow {
        context: "Sources",
        key: "Enter",
        action: "Connect to selected profile",
    },
    HelpRow {
        context: "Sources",
        key: "p",
        action: "Paste from clipboard",
    },
    HelpRow {
        context: "Sources",
        key: "y",
        action: "Yank selected source",
    },
    HelpRow {
        context: "Sources",
        key: "d",
        action: "Delete selected source",
    },
    HelpRow {
        context: "Sources",
        key: "u",
        action: "Update subscription or geo",
    },
    HelpRow {
        context: "Sources",
        key: "i",
        action: "Cycle subscription auto-update",
    },
    HelpRow {
        context: "Sources",
        key: "e",
        action: "Open profiles.json in $EDITOR",
    },
    HelpRow {
        context: "Sources",
        key: "t / T",
        action: "Test selected / all profiles",
    },
    HelpRow {
        context: "Logs",
        key: "Shift+V",
        action: "Select multiple logs",
    },
    HelpRow {
        context: "Logs",
        key: "y",
        action: "Yank selected log(s)",
    },
    HelpRow {
        context: "DNS / Services",
        key: "h/l / Left/Right",
        action: "Change selected value",
    },
    HelpRow {
        context: "Confirm Delete",
        key: "y / Enter",
        action: "Confirm deletion",
    },
    HelpRow {
        context: "Overlays",
        key: "Enter",
        action: "Confirm selection or changes",
    },
    HelpRow {
        context: "Main panes",
        key: "r",
        action: "Reconnect",
    },
    HelpRow {
        context: "Main panes",
        key: "s",
        action: "Disconnect",
    },
    HelpRow {
        context: "Main panes",
        key: "a",
        action: "Toggle auto-connect",
    },
    HelpRow {
        context: "Main panes",
        key: "K",
        action: "Toggle kill switch",
    },
    HelpRow {
        context: "Main panes",
        key: "m",
        action: "Routing mode",
    },
    HelpRow {
        context: "Main panes",
        key: "o",
        action: "Geo region",
    },
    HelpRow {
        context: "Main panes",
        key: "D",
        action: "DNS settings",
    },
    HelpRow {
        context: "Main panes",
        key: "S",
        action: "Service routing",
    },
    HelpRow {
        context: "Main panes",
        key: "C",
        action: "Theme picker",
    },
    HelpRow {
        context: "Main panes",
        key: "I",
        action: "Cycle geo auto-update",
    },
    HelpRow {
        context: "Main panes",
        key: "h/l / Left/Right",
        action: "Focus Sources / Logs",
    },
    HelpRow {
        context: "Lists / pickers",
        key: "j/k / Up/Down",
        action: "Navigate",
    },
    HelpRow {
        context: "Lists / pickers",
        key: "gg/G",
        action: "Jump to list/buffer edges",
    },
    HelpRow {
        context: "Main panes",
        key: "q / Esc",
        action: "Detach; Esc cancels visual",
    },
    HelpRow {
        context: "Overlays",
        key: "q / Esc",
        action: "Cancel",
    },
    HelpRow {
        context: "Global",
        key: "?",
        action: "Show help",
    },
    HelpRow {
        context: "Global",
        key: "Ctrl+C",
        action: "Quit daemon",
    },
];

fn label(context: HelpContext) -> &'static str {
    match context {
        HelpContext::Sources => "Sources",
        HelpContext::Logs => "Logs",
        HelpContext::ConfirmDelete => "Confirm Delete",
        HelpContext::RoutingMode => "Routing Mode",
        HelpContext::GeoRegions => "Geo Region",
        HelpContext::DnsSettings => "DNS",
        HelpContext::ThemeSettings => "Theme",
        HelpContext::ServiceRouting => "Service Routing",
    }
}

fn commands(context: HelpContext) -> &'static [(&'static str, &'static str)] {
    match context {
        HelpContext::Sources => SOURCES,
        HelpContext::Logs => LOGS,
        HelpContext::ConfirmDelete => CONFIRM_DELETE,
        HelpContext::RoutingMode | HelpContext::GeoRegions | HelpContext::ThemeSettings => PICKER,
        HelpContext::DnsSettings => DNS,
        HelpContext::ServiceRouting => SERVICES,
    }
}

fn append(
    rows: &mut Vec<HelpRow>,
    context: &'static str,
    commands: &[(&'static str, &'static str)],
) {
    rows.extend(commands.iter().map(|&(key, action)| HelpRow {
        context,
        key,
        action,
    }));
}

pub(crate) fn rows(state: HelpState, can_cancel_geo: bool) -> Vec<HelpRow> {
    let mut rows = Vec::new();
    match state.mode {
        HelpMode::Context => {
            append(&mut rows, label(state.context), commands(state.context));
            if state.context == HelpContext::GeoRegions && !can_cancel_geo {
                rows.retain(|row| row.key != "q / Esc");
            }
            append(&mut rows, "Global", GLOBAL);
        }
        HelpMode::All => {
            rows.extend_from_slice(ALL_ROWS);
        }
    }
    rows
}

pub(crate) fn title(context: HelpContext) -> &'static str {
    label(context)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(context: HelpContext, mode: HelpMode) -> HelpState {
        HelpState {
            context,
            mode,
            selected: 0,
        }
    }

    #[test]
    fn contextual_rows_only_include_active_context_and_global_commands() {
        let sources = rows(state(HelpContext::Sources, HelpMode::Context), true);
        assert_eq!(sources.first().map(|row| row.key), Some("Enter"));
        assert!(sources.iter().any(|row| row.key == "p"));
        assert!(!sources.iter().any(|row| row.key == "Shift+V"));

        let logs = rows(state(HelpContext::Logs, HelpMode::Context), true);
        assert_eq!(logs.first().map(|row| row.key), Some("Shift+V"));
        assert!(logs.iter().any(|row| row.key == "Shift+V"));
        assert!(logs.iter().any(|row| row.key == "D"));
        assert!(logs.iter().any(|row| row.key == "a"));
        assert!(logs.iter().any(|row| row.key == "K"));
        assert!(logs.iter().any(|row| row.key == "r"));
        assert!(logs.iter().any(|row| row.key == "s"));
        assert!(logs.iter().any(|row| row.key == "I"));
        assert!(!logs.iter().any(|row| row.key == "p"));
        assert!(logs.iter().any(|row| row.context == "Global"));

        let dns = rows(state(HelpContext::DnsSettings, HelpMode::Context), true);
        assert_eq!(dns.first().map(|row| row.key), Some("h/l / Left/Right"));
        let picker = rows(state(HelpContext::ThemeSettings, HelpMode::Context), true);
        assert_eq!(picker.first().map(|row| row.key), Some("Enter"));
    }

    #[test]
    fn all_rows_are_compact_and_cover_command_categories() {
        use std::collections::HashSet;

        let all = rows(state(HelpContext::Sources, HelpMode::All), true);
        assert_eq!(all.first().map(|row| row.context), Some("Sources"));
        assert_eq!(all.first().map(|row| row.key), Some("Enter"));
        assert_eq!(all.last().map(|row| row.key), Some("Ctrl+C"));
        for context in [
            "Global",
            "Lists / pickers",
            "Main panes",
            "Sources",
            "Logs",
            "Confirm Delete",
            "Overlays",
            "DNS / Services",
        ] {
            assert!(all.iter().any(|row| row.context == context), "{context}");
        }
        assert_eq!(all.iter().filter(|row| row.key == "m").count(), 1);
        assert_eq!(
            all.iter().filter(|row| row.key == "j/k / Up/Down").count(),
            1
        );
        assert!(all.len() < SOURCES.len() + LOGS.len());
        let index = |context| all.iter().position(|row| row.context == context).unwrap();
        assert!(index("Sources") < index("Logs"));
        assert!(index("Logs") < index("Main panes"));
        assert!(index("Main panes") < index("Lists / pickers"));
        assert!(index("Lists / pickers") < index("Global"));
        let mut unique = HashSet::new();
        assert!(all.iter().all(|row| unique.insert((row.key, row.action))));
    }

    #[test]
    fn required_geo_selection_omits_cancel_in_context_mode() {
        let required = rows(state(HelpContext::GeoRegions, HelpMode::Context), false);
        assert!(!required.iter().any(|row| row.key == "q / Esc"));

        let optional = rows(state(HelpContext::GeoRegions, HelpMode::Context), true);
        assert!(optional.iter().any(|row| row.key == "q / Esc"));
    }
}
