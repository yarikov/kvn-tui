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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_handle_lifecycle() {
        let child = std::process::Command::new("sleep")
            .arg("10")
            .spawn()
            .unwrap();
        let pid = child.id();
        let mut handle = ProcessHandle::new(child);
        assert_eq!(handle.pid, pid);
        handle.kill_and_wait().unwrap();
    }
}
