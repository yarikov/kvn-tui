use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::Context;

use crate::app::msg::{IpcCommand, Msg, StateSnapshot};

/// Per-client write timeout. A wedged TUI must not block the daemon main loop
/// — anything slower than this is treated as a dead client and disconnected.
const BROADCAST_WRITE_TIMEOUT: Duration = Duration::from_millis(200);

/// Return the path to the Unix domain socket used for IPC.
pub fn socket_path() -> std::path::PathBuf {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        std::path::PathBuf::from(dir).join("kvn-tui.sock")
    } else {
        // getuid is async-signal-safe and has no preconditions; the unsafe is
        // a libc-binding artefact, not a real invariant.
        #[allow(unsafe_code)]
        let uid = unsafe { libc::getuid() };
        std::path::PathBuf::from("/tmp").join(format!("kvn-tui-{}.sock", uid))
    }
}

/// Remove the socket file.
pub fn cleanup_socket() {
    let _ = std::fs::remove_file(socket_path());
}

/// Check whether the daemon socket is accepting connections.
pub fn is_daemon_running() -> bool {
    UnixStream::connect(socket_path()).is_ok()
}

/// Poll for daemon readiness with exponential backoff (10ms → 320ms cap),
/// returning as soon as the socket accepts a connection. Replaces a fixed
/// `sleep(300ms)` that was too short on cold/slow systems and too long on
/// warm ones.
pub fn wait_for_daemon(timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    let mut delay = Duration::from_millis(10);
    let cap = Duration::from_millis(320);
    while start.elapsed() < timeout {
        if is_daemon_running() {
            return true;
        }
        thread::sleep(delay);
        delay = (delay * 2).min(cap);
    }
    is_daemon_running()
}

/// Daemon-side IPC server.
pub struct IpcServer {
    clients: Arc<Mutex<Vec<UnixStream>>>,
}

impl IpcServer {
    pub fn bind(tx: Sender<Msg>) -> anyhow::Result<Self> {
        let path = socket_path();
        if path.exists() {
            if UnixStream::connect(&path).is_ok() {
                anyhow::bail!("kvn-tui daemon is already running at {}", path.display());
            }
            std::fs::remove_file(&path)
                .with_context(|| format!("Failed to remove stale socket {}", path.display()))?;
        }
        let listener = UnixListener::bind(&path)?;
        // 0600: any local user knowing the socket path could otherwise send
        // IpcCommand (Quit / Key / Paste). XDG_RUNTIME_DIR is typically 0700
        // already, but the /tmp/kvn-tui-<uid>.sock fallback path is not.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .context("Failed to chmod IPC socket")?;
        let clients: Arc<Mutex<Vec<UnixStream>>> = Arc::new(Mutex::new(Vec::new()));
        let clients_clone = clients.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        let writer = match stream.try_clone() {
                            Ok(w) => w,
                            Err(e) => {
                                tracing::warn!("Failed to clone IPC stream for broadcast: {e}");
                                continue;
                            }
                        };
                        if let Err(e) = writer.set_write_timeout(Some(BROADCAST_WRITE_TIMEOUT)) {
                            tracing::warn!("Failed to set IPC write timeout: {e}");
                        }
                        let tx = tx.clone();
                        let clients = clients_clone.clone();
                        clients.lock().unwrap().push(writer);
                        thread::spawn(move || {
                            let reader = BufReader::new(stream);
                            for line in reader.lines() {
                                match line {
                                    Ok(line) => {
                                        if let Ok(cmd) = serde_json::from_str::<IpcCommand>(&line) {
                                            let _ = tx.send(Msg::IpcCommand(cmd));
                                        }
                                    }
                                    Err(_) => break,
                                }
                            }
                        });
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self { clients })
    }

    /// Send a state snapshot to every connected TUI client.
    ///
    /// Writes are performed *without* holding the clients mutex: one slow or
    /// wedged TUI must not stall the daemon main loop or other clients.
    /// Per-stream `set_write_timeout` provides the upper bound on how long
    /// a single client can hold us up.
    pub fn broadcast(&self, snapshot: &StateSnapshot) {
        let json = match serde_json::to_string(snapshot) {
            Ok(s) => s + "\n",
            Err(e) => {
                tracing::warn!("Failed to serialize state snapshot: {e}");
                return;
            }
        };

        // Dup the stream fds under a short lock so writes happen without it.
        // We tag each clone with the original fd so cleanup can identify dead
        // clients even if `clients` was mutated by accept-loop concurrently.
        let writers: Vec<(RawFd, UnixStream)> = {
            let guard = self.clients.lock().unwrap();
            guard
                .iter()
                .filter_map(|s| s.try_clone().ok().map(|c| (s.as_raw_fd(), c)))
                .collect()
        };

        let mut dead_fds: HashSet<RawFd> = HashSet::new();
        for (fd, mut client) in writers {
            if let Err(e) = client.write_all(json.as_bytes()) {
                tracing::debug!("Dropping IPC client (fd={fd}): {e}");
                dead_fds.insert(fd);
            }
        }

        if !dead_fds.is_empty() {
            let mut guard = self.clients.lock().unwrap();
            guard.retain(|s| !dead_fds.contains(&s.as_raw_fd()));
        }
    }
}

/// TUI client-side IPC connection.
pub struct IpcClient {
    stream: UnixStream,
}

impl IpcClient {
    pub fn connect() -> anyhow::Result<Self> {
        let stream = UnixStream::connect(socket_path())?;
        stream.set_nonblocking(false)?;
        Ok(Self { stream })
    }

    pub fn send(&mut self, cmd: &IpcCommand) -> anyhow::Result<()> {
        let json = serde_json::to_string(cmd)? + "\n";
        self.stream.write_all(json.as_bytes())?;
        self.stream.flush()?;
        Ok(())
    }

    /// Spawn a background thread that reads state snapshots from the daemon
    /// and forwards them into the given mpsc channel.
    pub fn spawn_reader(&self, tx: Sender<Msg>) -> anyhow::Result<()> {
        let stream = self
            .stream
            .try_clone()
            .context("Failed to clone IPC socket for snapshot reader")?;
        thread::spawn(move || {
            let reader = BufReader::new(stream);
            for line in reader.lines() {
                match line {
                    Ok(line) => {
                        if let Ok(snapshot) = serde_json::from_str::<StateSnapshot>(&line) {
                            let _ = tx.send(Msg::StateUpdate(Box::new(snapshot)));
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::model::{ConnectionState, Overlay, TrafficStats};
    use crate::app::msg::StateSnapshot;
    use crate::config::profile::Settings;
    use std::sync::mpsc::channel;
    use std::time::Instant;

    fn drain_one(rx: &std::sync::mpsc::Receiver<Msg>, timeout: Duration) -> Option<Msg> {
        let deadline = Instant::now() + timeout;
        loop {
            match rx.try_recv() {
                Ok(msg) => return Some(msg),
                Err(_) if Instant::now() >= deadline => return None,
                Err(_) => thread::sleep(Duration::from_millis(5)),
            }
        }
    }

    fn sample_snapshot() -> StateSnapshot {
        StateSnapshot {
            connection: ConnectionState::Idle,
            status: "ok".into(),
            status_is_error: false,
            singbox_pid: None,
            active_profile_id: None,
            selected: 0,
            routing_selected: 0,
            geo_region_selected: 0,
            dns_selected: 0,
            dns_strategy_draft: None,
            theme_selected: 0,
            theme_draft: None,
            service_routing_selected: 0,
            service_routing_draft: None,
            geo_updating: false,
            geo_last_updated: None,
            overlay: Overlay::None,
            profiles: vec![],
            subscriptions: vec![],
            settings: Settings::default(),
            traffic: TrafficStats::default(),
            profile_latencies: Default::default(),
            testing_profiles: Default::default(),
        }
    }

    /// End-to-end: client → server command, server → client broadcast.
    /// Pins XDG_RUNTIME_DIR to a tempdir so we don't collide with a real
    /// daemon's socket.
    #[test]
    fn ipc_roundtrip_command_and_broadcast() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", tmp.path()) };
        cleanup_socket();

        let (server_tx, server_rx) = channel::<Msg>();
        let server = IpcServer::bind(server_tx).expect("server bind");

        let mut client = IpcClient::connect().expect("client connect");
        let (client_tx, client_rx) = channel::<Msg>();
        client.spawn_reader(client_tx).expect("reader spawn");

        // Client → server.
        client.send(&IpcCommand::Quit).expect("send quit");
        match drain_one(&server_rx, Duration::from_secs(2)) {
            Some(Msg::IpcCommand(IpcCommand::Quit)) => {}
            Some(_) => panic!("expected Msg::IpcCommand(Quit), got a different Msg variant"),
            None => panic!("timed out waiting for Msg::IpcCommand(Quit)"),
        }

        // Server → client. Broadcast happens off the accept thread, so give
        // the client side a moment to register before we send.
        thread::sleep(Duration::from_millis(50));
        server.broadcast(&sample_snapshot());
        match drain_one(&client_rx, Duration::from_secs(2)) {
            Some(Msg::StateUpdate(snap)) => {
                assert_eq!(snap.status, "ok");
                assert!(matches!(snap.connection, ConnectionState::Idle));
            }
            Some(_) => panic!("expected Msg::StateUpdate, got a different Msg variant"),
            None => panic!("timed out waiting for Msg::StateUpdate"),
        }

        cleanup_socket();
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };
    }

    #[test]
    fn socket_path_uses_xdg_runtime_dir_when_set() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("XDG_RUNTIME_DIR");
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", tmp.path()) };
        let p = socket_path();
        assert!(p.starts_with(tmp.path()));
        assert_eq!(p.file_name().unwrap(), "kvn-tui.sock");
        unsafe {
            match prev {
                Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
                None => std::env::remove_var("XDG_RUNTIME_DIR"),
            }
        }
    }

    #[test]
    fn socket_path_falls_back_to_tmp_with_uid_when_no_xdg() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let prev = std::env::var_os("XDG_RUNTIME_DIR");
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };
        let p = socket_path();
        assert!(p.starts_with("/tmp"));
        let name = p.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("kvn-tui-"));
        assert!(name.ends_with(".sock"));
        unsafe {
            if let Some(v) = prev {
                std::env::set_var("XDG_RUNTIME_DIR", v);
            }
        }
    }

    #[test]
    fn ipc_socket_has_0600_permissions() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", tmp.path()) };
        cleanup_socket();

        let (server_tx, _server_rx) = channel::<Msg>();
        let _server = IpcServer::bind(server_tx).expect("server bind");
        let mode = std::fs::metadata(socket_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "expected 0600, got {:o}", mode);

        cleanup_socket();
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };
    }

    #[test]
    fn second_server_does_not_replace_live_socket() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", tmp.path()) };
        cleanup_socket();

        let (first_tx, _first_rx) = channel::<Msg>();
        let _first = IpcServer::bind(first_tx).expect("first server bind");
        let (second_tx, _second_rx) = channel::<Msg>();
        let err = match IpcServer::bind(second_tx) {
            Ok(_) => panic!("second server must be rejected"),
            Err(err) => err,
        };

        assert!(err.to_string().contains("already running"));
        assert!(UnixStream::connect(socket_path()).is_ok());

        cleanup_socket();
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };
    }

    #[test]
    fn server_replaces_stale_socket() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", tmp.path()) };
        std::fs::write(socket_path(), b"stale").unwrap();

        let (server_tx, _server_rx) = channel::<Msg>();
        let _server = IpcServer::bind(server_tx).expect("replace stale socket");

        assert!(UnixStream::connect(socket_path()).is_ok());

        cleanup_socket();
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };
    }

    #[test]
    fn is_daemon_running_returns_false_when_no_socket() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("XDG_RUNTIME_DIR");
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", tmp.path()) };
        assert!(!is_daemon_running());
        unsafe {
            match prev {
                Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
                None => std::env::remove_var("XDG_RUNTIME_DIR"),
            }
        }
    }

    #[test]
    fn wait_for_daemon_times_out_without_server() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("XDG_RUNTIME_DIR");
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", tmp.path()) };
        let started = std::time::Instant::now();
        assert!(!wait_for_daemon(Duration::from_millis(50)));
        // Sanity: it must have actually polled and not returned instantly.
        assert!(started.elapsed() >= Duration::from_millis(40));
        unsafe {
            match prev {
                Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
                None => std::env::remove_var("XDG_RUNTIME_DIR"),
            }
        }
    }

    #[test]
    fn server_ignores_malformed_command_json() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", tmp.path()) };
        cleanup_socket();

        let (server_tx, server_rx) = channel::<Msg>();
        let _server = IpcServer::bind(server_tx).expect("server bind");

        let mut stream = UnixStream::connect(socket_path()).expect("client connect");
        stream
            .write_all(b"this is not json\n")
            .expect("write garbage");
        stream.flush().unwrap();

        // Server must NOT forward a Msg for an unparseable line.
        assert!(drain_one(&server_rx, Duration::from_millis(200)).is_none());

        cleanup_socket();
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };
    }

    #[test]
    fn broadcast_drops_dead_clients() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", tmp.path()) };
        cleanup_socket();

        let (server_tx, _server_rx) = channel::<Msg>();
        let server = IpcServer::bind(server_tx).expect("server bind");

        // Connect a client and immediately drop it without reading anything.
        {
            let _client = IpcClient::connect().expect("connect");
        }
        // Give the accept thread time to register the client.
        thread::sleep(Duration::from_millis(50));

        // First broadcast may succeed (kernel buffers the line); a subsequent
        // shutdown means at least one will encounter EPIPE. Loop a few times
        // so the dead client is reaped.
        for _ in 0..5 {
            server.broadcast(&sample_snapshot());
        }
        // After reaping there should be zero live clients tracked.
        let live = server.clients.lock().unwrap().len();
        assert_eq!(live, 0, "expected dead client to be reaped");

        cleanup_socket();
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };
    }
}
