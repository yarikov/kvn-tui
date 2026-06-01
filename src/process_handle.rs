use anyhow::{Context, Result};

pub struct ProcessHandle {
    child: std::process::Child,
    pub pid: u32,
}

impl ProcessHandle {
    pub fn new(child: std::process::Child) -> Self {
        let pid = child.id();
        Self { child, pid }
    }

    pub fn kill_and_wait(&mut self) -> Result<()> {
        self.child.kill().context("Failed to kill sing-box process")?;
        if let Err(e) = self.child.wait() {
            tracing::warn!("Failed to wait for sing-box process: {}", e);
        }
        Ok(())
    }
}
