use anyhow::{Context, Result};
use std::process::ExitStatus;

pub struct ProcessHandle {
    child: std::process::Child,
    pub pid: u32,
}

impl ProcessHandle {
    pub fn new(child: std::process::Child) -> Self {
        let pid = child.id();
        Self { child, pid }
    }

    /// Check whether the child has exited without blocking. A returned status
    /// means the OS process has also been reaped by `Child::try_wait`.
    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        self.child
            .try_wait()
            .context("Failed to check sing-box process status")
    }

    pub fn kill_and_wait(&mut self) -> Result<()> {
        self.child
            .kill()
            .context("Failed to kill sing-box process")?;
        if let Err(e) = self.child.wait() {
            tracing::warn!("Failed to wait for sing-box process: {}", e);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_handle_lifecycle() {
        // spawn() resolves `sleep` via PATH, i.e. reads the process env —
        // racing the ENV_LOCK-guarded tests that call env::set_var, which
        // intermittently fails the lookup with ENOENT. Serialize with them.
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let child = std::process::Command::new("sleep")
            .arg("10")
            .spawn()
            .unwrap();
        let pid = child.id();
        let mut handle = ProcessHandle::new(child);
        assert_eq!(handle.pid, pid);
        handle.kill_and_wait().unwrap();
    }

    #[test]
    fn try_wait_reports_natural_exit() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let child = std::process::Command::new("sh")
            .args(["-c", "exit 7"])
            .spawn()
            .unwrap();
        let mut handle = ProcessHandle::new(child);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let status = loop {
            if let Some(status) = handle.try_wait().unwrap() {
                break status;
            }
            assert!(std::time::Instant::now() < deadline, "child did not exit");
            std::thread::yield_now();
        };
        assert_eq!(status.code(), Some(7));
    }
}
