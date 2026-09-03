use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use anyhow::{Context, Result};

struct TempFileCleanup<'a> {
    path: &'a Path,
    armed: bool,
}

impl TempFileCleanup<'_> {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TempFileCleanup<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(self.path);
        }
    }
}

/// Write `data` to `dest` atomically and durably.
///
/// Writes to `<dest>.tmp`, fsyncs the data to disk, renames over `dest`, then
/// fsyncs the parent directory so the rename itself survives a crash. Without
/// the parent-dir fsync, on ext4 with the default `data=ordered` the rename
/// metadata can be lost after a power cut even though the file contents are
/// persisted — leaving the user with an empty or stale `dest`.
pub fn write(dest: &Path, data: &[u8]) -> Result<()> {
    write_inner(dest, data, None)
}

/// Atomically write `data` only while the destination still has the bytes the
/// caller previously read. The comparison happens after the temporary file is
/// durable and immediately before rename.
pub fn write_if_unchanged(dest: &Path, data: &[u8], expected: Option<&[u8]>) -> Result<()> {
    write_inner(dest, data, Some(expected))
}

fn write_inner(dest: &Path, data: &[u8], expected: Option<Option<&[u8]>>) -> Result<()> {
    let dir = dest
        .parent()
        .with_context(|| format!("Atomic write: dest {:?} has no parent", dest))?;
    let name = dest
        .file_name()
        .with_context(|| format!("Atomic write: dest {:?} has no file name", dest))?;
    let temp = dir.join(format!("{}.tmp", name.to_string_lossy()));
    let mut temp_cleanup = TempFileCleanup {
        path: &temp,
        armed: true,
    };

    {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp)
            .with_context(|| format!("Failed to create temp file {:?}", temp))?;
        // `mode` applies at creation time, avoiding a window where a new
        // secret-bearing file has umask-derived permissions. chmod as well so
        // a pre-existing temp file from an interrupted write is tightened.
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .with_context(|| format!("Failed to chmod temp file {:?}", temp))?;
        file.write_all(data)
            .with_context(|| format!("Failed to write temp file {:?}", temp))?;
        file.sync_all()
            .with_context(|| format!("Failed to fsync temp file {:?}", temp))?;
    }

    if let Some(expected) = expected {
        let actual = match fs::read(dest) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error).context("Failed to verify destination revision");
            }
        };
        if actual.as_deref() != expected {
            anyhow::bail!("destination changed since it was read");
        }
    }

    fs::rename(&temp, dest)
        .with_context(|| format!("Failed to rename {:?} -> {:?}", temp, dest))?;
    temp_cleanup.disarm();

    // Persist the rename itself. Best-effort: some filesystems (tmpfs, certain
    // FUSE mounts) return errors here even though the rename is safe in
    // practice — we don't want to fail the whole save for that.
    match fs::File::open(dir) {
        Ok(handle) => {
            if let Err(e) = handle.sync_all() {
                tracing::warn!("fsync of parent dir {:?} failed: {}", dir, e);
            }
        }
        Err(e) => {
            tracing::warn!("open of parent dir {:?} for fsync failed: {}", dir, e);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_persists_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.json");
        write(&path, b"hello world").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello world");
    }

    #[test]
    fn atomic_write_removes_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.json");
        write(&path, b"payload").unwrap();
        let temp = dir.path().join("file.json.tmp");
        assert!(!temp.exists(), "temp file must not linger after rename");
    }

    #[test]
    fn atomic_write_overwrites_existing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.json");
        fs::write(&path, b"old").unwrap();
        write(&path, b"new").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
    }

    #[test]
    fn atomic_write_removes_temp_file_when_rename_fails() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("destination");
        fs::create_dir(&destination).unwrap();

        assert!(write(&destination, b"payload").is_err());
        assert!(!dir.path().join("destination.tmp").exists());
    }

    #[test]
    fn atomic_write_fails_when_parent_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing/file.json");
        assert!(write(&path, b"data").is_err());
    }

    #[test]
    fn atomic_write_sets_0600_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        write(&path, b"secret").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600, got {:o}", mode);
    }

    #[test]
    fn atomic_write_tightens_permissions_on_existing_loose_file() {
        // Upgrade path: a pre-existing file written by an older build with
        // umask-derived 0644 must end up at 0600 after the next save.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        fs::write(&path, b"old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        write(&path, b"new").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600, got {:o}", mode);
    }

    #[test]
    fn conditional_write_rejects_changed_destination() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        fs::write(&path, b"newer").unwrap();
        assert!(write_if_unchanged(&path, b"ours", Some(b"older")).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"newer");
        assert!(!dir.path().join("profiles.json.tmp").exists());
    }

    #[test]
    fn conditional_write_accepts_matching_destination() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        fs::write(&path, b"old").unwrap();
        write_if_unchanged(&path, b"new", Some(b"old")).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
    }
}
