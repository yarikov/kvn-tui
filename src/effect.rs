use crate::config::profile::{Profile, Settings};

#[derive(Debug, PartialEq)]
pub enum Effect {
    Connect(Profile, Settings),
    Reconnect(Profile, Settings),
    Disconnect,
    DownloadGeo,
    TailLogs,
    WriteState,
    SaveConfig,
    OpenEditor(usize),
    PasteClipboard,
    Quit,
}
