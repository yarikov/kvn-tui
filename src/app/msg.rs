use crate::app::model::{ConnectionState, Overlay};
use crate::config::profile::{Profile, Settings};
use crossterm::event::KeyEvent;

pub enum Msg {
    Key(KeyEvent),
    Tick,
    Resize,
    LogLine(String),
    GeoUpdated(GeoResult),
    SystemResumed,
    Connected { pid: u32 },
    ConnectFailed(String),

    IpcCommand(IpcCommand),
    StateUpdate(StateSnapshot),
}

#[derive(Debug)]
pub enum GeoResult {
    Updated(Vec<String>),
    UpToDate,
    Error(String),
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
    ReloadConfig,
    Quit,
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
    pub geo_updating: bool,
    pub overlay: Overlay,
    pub profiles: Vec<Profile>,
    pub settings: Settings,
}
