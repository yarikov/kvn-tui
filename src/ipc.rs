use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;

use crate::app::msg::{IpcCommand, Msg, StateSnapshot};

/// Return the path to the Unix domain socket used for IPC.
pub fn socket_path() -> std::path::PathBuf {
    if let Some(user) = crate::infra::user_env::sudo_user() {
        if let Some(home) = crate::infra::user_env::home_dir(&user) {
            return home.join(".config").join("kvn-tui").join("kvn-tui.sock");
        }
    }
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        std::path::PathBuf::from(dir).join("kvn-tui.sock")
    } else {
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

/// Daemon-side IPC server.
pub struct IpcServer {
    clients: Arc<Mutex<Vec<UnixStream>>>,
}

impl IpcServer {
    pub fn bind(tx: Sender<Msg>) -> anyhow::Result<Self> {
        let path = socket_path();
        if path.exists() {
            std::fs::remove_file(&path).ok();
        }
        let listener = UnixListener::bind(&path)?;
        let clients: Arc<Mutex<Vec<UnixStream>>> = Arc::new(Mutex::new(Vec::new()));
        let clients_clone = clients.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        let tx = tx.clone();
                        let clients = clients_clone.clone();
                        clients.lock().unwrap().push(stream.try_clone().unwrap());
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
    pub fn broadcast(&self, snapshot: &StateSnapshot) {
        let json = match serde_json::to_string(snapshot) {
            Ok(s) => s + "\n",
            Err(_) => return,
        };
        let mut clients = self.clients.lock().unwrap();
        let mut to_remove = Vec::new();
        for (idx, client) in clients.iter_mut().enumerate() {
            if client.write_all(json.as_bytes()).is_err() {
                to_remove.push(idx);
            }
        }
        for idx in to_remove.into_iter().rev() {
            clients.remove(idx);
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
    pub fn spawn_reader(&self, tx: Sender<Msg>) {
        let stream = self.stream.try_clone().expect("dup unix stream");
        thread::spawn(move || {
            let reader = BufReader::new(stream);
            for line in reader.lines() {
                match line {
                    Ok(line) => {
                        if let Ok(snapshot) = serde_json::from_str::<StateSnapshot>(&line) {
                            let _ = tx.send(Msg::StateUpdate(snapshot));
                        }
                    }
                    Err(_) => break,
                }
            }
        });
    }
}
