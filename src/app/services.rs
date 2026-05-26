use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

/// Suspend detection threshold in seconds.
const SUSPEND_THRESHOLD_SECS: u64 = 30;

use crate::geo::GeoManager;

/// Tails the sing-box log file and returns new lines.
pub struct LogTailer {
    path: PathBuf,
    pos: u64,
}

impl LogTailer {
    pub fn new(path: PathBuf) -> Self {
        Self { path, pos: 0 }
    }

    pub fn set_path(&mut self, path: PathBuf) {
        self.path = path;
        self.pos = 0;
    }

    pub fn tail(&mut self) -> Vec<String> {
        let Ok(mut file) = File::open(&self.path) else {
            return Vec::new();
        };
        let Ok(metadata) = file.metadata() else {
            return Vec::new();
        };
        let file_len = metadata.len();

        // If file shrank (rotated), reset position.
        if self.pos > file_len {
            self.pos = 0;
        }

        let mut lines = Vec::new();
        if file.seek(SeekFrom::Start(self.pos)).is_ok() {
            let reader = BufReader::new(file);
            for line in reader.lines().map_while(Result::ok) {
                if !line.trim().is_empty() {
                    lines.push(format!("[sing-box] {}", line));
                }
            }
            self.pos = file_len;
        }
        lines
    }
}

/// Background geo database updates.
pub struct GeoUpdater {
    tx: Sender<String>,
    rx: Receiver<String>,
}

impl GeoUpdater {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Self { tx, rx }
    }

    /// Check for completed update results.
    pub fn poll(&mut self) -> Vec<String> {
        let mut results = Vec::new();
        while let Ok(msg) = self.rx.try_recv() {
            results.push(msg);
        }
        results
    }

    /// Trigger a background geo database update.
    pub fn trigger(&self) {
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let geo_manager = match GeoManager::new() {
                Ok(g) => g,
                Err(e) => {
                    if let Err(e) = tx.send(format!("Error: {}", e)) {
                        tracing::warn!("Geo update send failed: {}", e);
                    }
                    return;
                }
            };

            match geo_manager.update_if_needed() {
                Ok(msg) => {
                    if let Err(e) = tx.send(msg) {
                        tracing::warn!("Geo update send failed: {}", e);
                    }
                }
                Err(e) => {
                    if let Err(e) = tx.send(format!("Error: {}", e)) {
                        tracing::warn!("Geo update send failed: {}", e);
                    }
                }
            }
        });
    }
}

/// Watches for system suspend/resume events.
pub struct SuspendWatcher {
    rx: Option<Receiver<()>>,
    last_tick: Instant,
}

impl SuspendWatcher {
    pub fn new() -> Self {
        let (stx, srx) = channel();
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                let _ = crate::suspend::listen(stx).await;
            });
        }
        Self {
            rx: Some(srx),
            last_tick: Instant::now(),
        }
    }

    /// Create a dummy watcher for tests (no D-Bus listener).
    #[cfg(test)]
    pub fn test_new() -> Self {
        let (_stx, srx) = channel();
        Self {
            rx: Some(srx),
            last_tick: Instant::now(),
        }
    }

    /// Check if the system resumed from suspend.
    /// Returns `true` when a resume event is detected and reconnect should happen.
    pub fn check(&mut self, connected: bool) -> bool {
        const SUSPEND_THRESHOLD: Duration = Duration::from_secs(SUSPEND_THRESHOLD_SECS);
        let elapsed = self.last_tick.elapsed();
        self.last_tick = Instant::now();

        // Method 1: D-Bus signal from logind.
        if let Some(ref rx) = self.rx {
            if rx.try_recv().is_ok() && connected {
                return true;
            }
        }

        // Method 2: time-gap heuristic (fallback if D-Bus failed).
        elapsed > SUSPEND_THRESHOLD && connected
    }
}
