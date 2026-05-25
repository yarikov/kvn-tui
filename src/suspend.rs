use std::sync::mpsc::Sender;

use futures_util::StreamExt;
use zbus::{Connection, MatchRule, MessageStream, Result};

/// Spawn an async D-Bus listener that watches for systemd-logind
/// `PrepareForSleep` signals and notifies the caller on resume.
pub async fn listen(tx: Sender<()>) -> Result<()> {
    let connection = Connection::system().await?;

    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.freedesktop.login1")?
        .interface("org.freedesktop.login1.Manager")?
        .member("PrepareForSleep")?
        .build();

    let mut stream = MessageStream::for_match_rule(rule, &connection, Some(1)).await?;

    while let Some(msg) = stream.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(_) => continue,
        };

        if let Ok(going_to_sleep) = msg.body().deserialize::<bool>() {
            if !going_to_sleep {
                let _ = tx.send(());
            }
        }
    }

    Ok(())
}
