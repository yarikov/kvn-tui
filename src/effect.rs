use crate::config::profile::{Profile, Settings};

#[derive(Debug, PartialEq)]
pub enum Effect {
    Connect {
        profile: Profile,
        settings: Settings,
        force_restart: bool,
    },
    Disconnect,
    DownloadGeo,
    TailLogs,
    WriteState,
    SaveConfig,
    OpenEditor(usize),
    PasteClipboard,
    Quit,
}
