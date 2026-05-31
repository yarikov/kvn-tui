use std::sync::mpsc::Sender;
use crate::msg::Msg;

/// Spawn a blocking D-Bus listener that watches for systemd-logind
/// `PrepareForSleep` signals and notifies the caller on resume.
pub fn listen_blocking(tx: Sender<Msg>) {
    let Ok(conn) = zbus::blocking::Connection::system() else {
        tracing::warn!("Failed to connect to system D-Bus");
        return;
    };

    let rule = match zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.freedesktop.login1")
        .and_then(|b| b.interface("org.freedesktop.login1.Manager"))
        .and_then(|b| b.member("PrepareForSleep"))
        .map(|b| b.build())
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Failed to build D-Bus match rule: {}", e);
            return;
        }
    };

    let iter = match zbus::blocking::MessageIterator::for_match_rule(rule, &conn, Some(1)) {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!("Failed to create D-Bus message iterator: {}", e);
            return;
        }
    };

    for msg in iter.flatten() {
        if let Ok(going_to_sleep) = msg.body().deserialize::<bool>() {
            if !going_to_sleep {
                let _ = tx.send(Msg::SystemResumed);
            }
        }
    }
}
