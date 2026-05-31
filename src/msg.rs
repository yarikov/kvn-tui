use crossterm::event::KeyEvent;
use std::process::Child;
use crate::config::profile::Config;

pub enum Msg {
    Key(KeyEvent),
    Tick,
    Resize,
    LogLine(String),
    GeoUpdated(GeoResult),
    SystemResumed,
    Connected(Child),
    ConnectFailed(String),
    ClipboardRead(Result<String, String>),
    EditorClosed(Result<Config, String>),
}

#[derive(Debug)]
pub enum GeoResult {
    Updated(Vec<String>),
    UpToDate,
    Error(String),
}
