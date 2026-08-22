use crate::app::model::{ConnectionState, Overlay, TrafficStats};
use crate::config::profile::{DnsStrategy, Profile, Settings, Subscription};
use crate::ui::styles::Theme;
use crossterm::event::KeyEvent;
use uuid::Uuid;

/// Structured error carried inside `Msg` variants.
///
/// `chain` is ordered outermost first, matching `anyhow::Error::chain()` —
/// element 0 is the high-level summary (suitable for the status bar), and
/// later elements are the root causes added through `.context(...)`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IpcError {
    pub chain: Vec<String>,
}

impl IpcError {
    #[cfg(test)]
    pub fn new<S: Into<String>>(msg: S) -> Self {
        Self {
            chain: vec![msg.into()],
        }
    }
}

impl std::fmt::Display for IpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, msg) in self.chain.iter().enumerate() {
            if i > 0 {
                write!(f, ": ")?;
            }
            write!(f, "{msg}")?;
        }
        Ok(())
    }
}

impl From<anyhow::Error> for IpcError {
    fn from(err: anyhow::Error) -> Self {
        Self {
            chain: err.chain().map(|c| c.to_string()).collect(),
        }
    }
}

pub enum Msg {
    Key(KeyEvent),
    Tick,
    Resize,
    GeoUpdated(GeoResult),
    GeoMetadataRefreshed {
        last_updated: Option<String>,
        last_checked_at: Option<chrono::DateTime<chrono::Local>>,
        retry_state: Option<crate::geo::GeoRetryState>,
    },
    SystemResumed,
    Connected {
        pid: u32,
        attempt_id: u64,
        /// The profile that actually connected. Carried through the connect
        /// flow because the cursor may have moved since the connect was
        /// issued — attribution must not depend on the selection.
        profile_id: Uuid,
    },
    ConnectFailed {
        attempt_id: u64,
        error: IpcError,
    },
    /// The managed sing-box child exited after passing its initial readiness
    /// check. Bound to the connection generation so a late notification from
    /// an old process cannot tear down a newer connection.
    SingBoxExited {
        attempt_id: u64,
        code: Option<i32>,
        signal: Option<i32>,
    },
    /// A service rule-set download pass finished (files may still be missing
    /// if individual downloads failed — absence degrades to "no rule for
    /// that service"). Consumed by the reducer to run the reconnect that a
    /// service-routing commit deferred until the rule-sets were fetched
    /// through the still-active tunnel (`Model::pending_service_reconnect`).
    ServiceRuleSetsReady,
    SubscriptionFetched {
        id: Uuid,
        result: Result<Vec<crate::config::profile::Profile>, IpcError>,
    },

    IpcCommand(IpcCommand),
    StateUpdate(Box<StateSnapshot>),
    ConfigReloaded(Box<Result<crate::config::profile::Config, IpcError>>),
    KillSwitchApplied {
        enabled: bool,
        error: Option<IpcError>,
    },
    /// Raw sample of cumulative byte counters from sing-box's Clash API,
    /// timestamped so the pure-layer can compute a per-second rate against
    /// the previous sample stored in `Model::traffic`.
    TrafficStatsUpdated {
        attempt_id: u64,
        request_id: u64,
        up_total: u64,
        down_total: u64,
        conn_count: usize,
        sampled_at_ms: u64,
    },
    /// New UI theme resolved from Omarchy's `theme.name` file (or a manual
    /// override). Carries a fully constructed [`Theme`] so the pure reducer
    /// can swap it in without performing any I/O.
    ThemeChanged(Theme),
    /// Result of a profile latency test initiated via `Effect::TestProfile`.
    /// `latency_ms` is `None` when the test timed out or the endpoint was
    /// unreachable.
    TestResult {
        id: Uuid,
        latency_ms: Option<u64>,
    },
}

#[derive(Debug)]
pub enum GeoResult {
    Updated {
        parts: Vec<String>,
        last_updated: Option<String>,
        checked_at: chrono::DateTime<chrono::Local>,
    },
    UpToDate {
        checked_at: Option<chrono::DateTime<chrono::Local>>,
    },
    Error {
        message: String,
        retry_state: Option<crate::geo::GeoRetryState>,
    },
}

/// Commands sent from the TUI client to the daemon over the Unix socket.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "cmd")]
pub enum IpcCommand {
    Attach,
    Detach,
    Key {
        code: String,
        char: Option<char>,
        ctrl: bool,
    },
    Paste {
        text: String,
    },
    Copied {
        name: String,
        count: usize,
    },
    ReloadConfig,
    Quit,
    /// Client-side failure the daemon owns none of — e.g. the external editor
    /// path rejecting an edit. The daemon writes it into its model's status
    /// and overlay so the next broadcast surfaces it in the TUI.
    ClientError {
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipc_error_display_joins_chain_with_colons() {
        let err = IpcError {
            chain: vec!["top".into(), "middle".into(), "root cause".into()],
        };
        assert_eq!(err.to_string(), "top: middle: root cause");
    }

    #[test]
    fn ipc_error_display_single_element() {
        let err = IpcError::new("only");
        assert_eq!(err.to_string(), "only");
    }

    #[test]
    fn ipc_error_display_empty_chain() {
        let err = IpcError { chain: vec![] };
        assert_eq!(err.to_string(), "");
    }

    #[test]
    fn ipc_error_from_anyhow_captures_context_chain() {
        let err: anyhow::Error = anyhow::anyhow!("inner").context("middle").context("top");
        let ipc: IpcError = err.into();
        assert!(ipc.chain.first().unwrap().contains("top"));
        assert!(ipc.chain.last().unwrap().contains("inner"));
        assert!(ipc.chain.len() >= 3);
    }

    #[test]
    fn ipc_command_serde_roundtrip_each_variant() {
        let cmds = vec![
            IpcCommand::Attach,
            IpcCommand::Detach,
            IpcCommand::Key {
                code: "Char".into(),
                char: Some('a'),
                ctrl: false,
            },
            IpcCommand::Paste {
                text: "hello".into(),
            },
            IpcCommand::Copied {
                name: "A".into(),
                count: 2,
            },
            IpcCommand::ReloadConfig,
            IpcCommand::Quit,
        ];
        for cmd in cmds {
            let json = serde_json::to_string(&cmd).unwrap();
            let back: IpcCommand = serde_json::from_str(&json).unwrap();
            // Round-trip via Debug since IpcCommand isn't PartialEq.
            assert_eq!(format!("{:?}", cmd), format!("{:?}", back));
        }
    }

    #[test]
    fn ipc_error_serde_roundtrip() {
        let err = IpcError {
            chain: vec!["a".into(), "b".into()],
        };
        let json = serde_json::to_string(&err).unwrap();
        let back: IpcError = serde_json::from_str(&json).unwrap();
        assert_eq!(err, back);
    }
}

/// State snapshot pushed from the daemon to TUI clients.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StateSnapshot {
    pub connection: ConnectionState,
    pub status: String,
    pub status_is_error: bool,
    pub singbox_pid: Option<u32>,
    pub active_profile_id: Option<String>,
    pub selected: usize,
    pub routing_selected: usize,
    pub geo_region_selected: usize,
    pub dns_selected: usize,
    #[serde(default)]
    pub dns_strategy_draft: Option<DnsStrategy>,
    #[serde(default)]
    pub dns_fakeip_draft: Option<bool>,
    #[serde(default)]
    pub theme_selected: usize,
    #[serde(default)]
    pub theme_draft: Option<String>,
    #[serde(default)]
    pub service_routing_selected: usize,
    #[serde(default)]
    pub service_routing_draft: Option<
        std::collections::HashMap<
            crate::config::profile::RoutedService,
            crate::config::profile::ServiceRoute,
        >,
    >,
    pub geo_updating: bool,
    pub geo_last_updated: Option<String>,
    pub overlay: Overlay,
    pub profiles: Vec<Profile>,
    pub subscriptions: Vec<Subscription>,
    pub settings: Settings,
    #[serde(default)]
    pub traffic: TrafficStats,
    /// Latency results keyed by profile UUID string. `null` = error, number =
    /// ms. Absent key = not yet tested. Transient — not persisted to disk.
    #[serde(default)]
    pub profile_latencies: std::collections::HashMap<String, Option<u64>>,
    /// Profile UUIDs whose test is currently in-flight (spinner indicator).
    #[serde(default)]
    pub testing_profiles: Vec<String>,
}
