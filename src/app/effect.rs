use crate::config::profile::{Profile, Settings};
use uuid::Uuid;

#[derive(Debug, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum Effect {
    Connect {
        profile: Profile,
        settings: Settings,
        attempt_id: u64,
    },
    Disconnect,
    DownloadGeo,
    DownloadGeoIfMissing,
    /// Fetch rule-sets for enabled service routes if absent. Emitted after a
    /// successful connect so the download rides the freshly opened tunnel —
    /// pre-tunnel the source may be unreachable (kill switch or ISP blocks).
    DownloadServiceRuleSetsIfMissing,
    RefreshGeoLastUpdated,
    WriteState,
    SaveConfig,
    UpdateSubscription {
        id: Uuid,
    },
    BroadcastState,
    Quit,
    AppendAppLog {
        level: String,
        message: String,
    },
    ReloadConfig,
    ApplyKillSwitch {
        enabled: bool,
    },
    /// Ask the daemon to scrape the Clash API once. IDs bind the asynchronous
    /// reply to the current connection and order overlapping requests.
    FetchTrafficStats {
        attempt_id: u64,
        request_id: u64,
    },
    /// Test a profile's reachability and measure latency via a temporary
    /// sing-box SOCKS5 inbound. Result is sent back as `Msg::TestResult`.
    TestProfile {
        id: Uuid,
    },
}
