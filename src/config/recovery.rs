use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};

const RETENTION_PER_KIND: usize = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RecoveryKind {
    Conflict,
    InvalidEdit,
    Archive,
}

impl RecoveryKind {
    const ALL: [Self; 3] = [Self::Conflict, Self::InvalidEdit, Self::Archive];

    fn prefix(self) -> &'static str {
        match self {
            Self::Conflict => "profiles.json.conflict-",
            Self::InvalidEdit => "profiles.json.conflict-invalid-",
            Self::Archive => "profiles.json.invalid-",
        }
    }
}

fn classify(name: &str) -> Option<RecoveryKind> {
    // `conflict-invalid` also starts with `conflict-`, so test it first.
    if has_generated_suffix(name, RecoveryKind::InvalidEdit.prefix()) {
        Some(RecoveryKind::InvalidEdit)
    } else if has_generated_suffix(name, RecoveryKind::Conflict.prefix()) {
        Some(RecoveryKind::Conflict)
    } else if has_generated_suffix(name, RecoveryKind::Archive.prefix()) {
        Some(RecoveryKind::Archive)
    } else {
        None
    }
}

fn has_generated_suffix(name: &str, prefix: &str) -> bool {
    let Some(suffix) = name.strip_prefix(prefix) else {
        return false;
    };
    if suffix.len() <= 37 {
        return false;
    }
    let (timestamp_and_separator, uuid) = suffix.split_at(suffix.len() - 36);
    let Some(timestamp) = timestamp_and_separator.strip_suffix('-') else {
        return false;
    };
    let timestamp_shape = matches!(timestamp.len(), 15 | 21)
        && timestamp.as_bytes().get(8) == Some(&b'T')
        && timestamp
            .bytes()
            .enumerate()
            .all(|(index, byte)| index == 8 || byte.is_ascii_digit());
    timestamp_shape && uuid::Uuid::parse_str(uuid).is_ok()
}

fn recovery_dir(profiles_path: &Path) -> Result<PathBuf> {
    let config_dir = profiles_path.parent().context("Invalid profiles path")?;
    let dir = config_dir.join("recovery");
    fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create recovery directory {}", dir.display()))?;
    let metadata = fs::symlink_metadata(&dir)
        .with_context(|| format!("Failed to inspect recovery directory {}", dir.display()))?;
    ensure!(
        metadata.file_type().is_dir(),
        "Recovery path is not a directory"
    );
    #[allow(unsafe_code)]
    let uid = unsafe { libc::getuid() };
    ensure!(
        metadata.uid() == uid,
        "Recovery directory is not owned by the current user"
    );
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("Failed to protect recovery directory {}", dir.display()))?;
    Ok(dir)
}

fn managed_files(dir: &Path, kind: RecoveryKind) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir)
        .with_context(|| format!("Failed to read recovery directory {}", dir.display()))?
    {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if classify(&name) != Some(kind) || !entry.file_type()?.is_file() {
            continue;
        }
        files.push(entry.path());
    }
    Ok(files)
}

fn prune(dir: &Path, kind: RecoveryKind, protected: Option<&Path>) -> Result<()> {
    let mut files = managed_files(dir, kind)?;
    files.sort();
    let remove_count = files.len().saturating_sub(RETENTION_PER_KIND);
    for path in files
        .into_iter()
        .filter(|path| protected != Some(path.as_path()))
        .take(remove_count)
    {
        if let Err(error) = fs::remove_file(&path) {
            tracing::warn!("Failed to remove old recovery file {:?}: {}", path, error);
        }
    }
    Ok(())
}

fn recovery_path(dir: &Path, kind: RecoveryKind) -> PathBuf {
    dir.join(format!(
        "{}{}-{}",
        kind.prefix(),
        chrono::Local::now().format("%Y%m%dT%H%M%S%6f"),
        uuid::Uuid::new_v4()
    ))
}

fn refresh(path: &Path, dir: &Path, kind: RecoveryKind) -> PathBuf {
    let refreshed = recovery_path(dir, kind);
    if let Err(error) = fs::rename(path, &refreshed) {
        tracing::warn!("Failed to refresh recovery file {:?}: {}", path, error);
        return path.to_path_buf();
    }
    match fs::File::open(dir) {
        Ok(handle) => {
            if let Err(error) = handle.sync_all() {
                tracing::warn!("Failed to fsync recovery directory {:?}: {}", dir, error);
            }
        }
        Err(error) => {
            tracing::warn!("Failed to open recovery directory {:?}: {}", dir, error);
        }
    }
    refreshed
}

/// Enforce retention for files already in the recovery directory.
pub(crate) fn maintain() -> Result<()> {
    let profiles_path =
        crate::paths::profiles_path().context("Failed to determine profiles path")?;
    let dir = recovery_dir(&profiles_path)?;
    for kind in RecoveryKind::ALL {
        prune(&dir, kind, None)?;
    }
    Ok(())
}

/// Preserve bytes for manual recovery, deduplicating within the same category.
pub(crate) fn preserve(kind: RecoveryKind, contents: &[u8]) -> Result<PathBuf> {
    let profiles_path =
        crate::paths::profiles_path().context("Failed to determine profiles path")?;
    preserve_at(&profiles_path, kind, contents)
}

pub(crate) fn preserve_at(
    profiles_path: &Path,
    kind: RecoveryKind,
    contents: &[u8],
) -> Result<PathBuf> {
    let dir = recovery_dir(profiles_path)?;

    for path in managed_files(&dir, kind)? {
        if fs::read(&path).is_ok_and(|existing| existing == contents) {
            let refreshed = refresh(&path, &dir, kind);
            prune(&dir, kind, Some(&refreshed))?;
            return Ok(refreshed);
        }
    }

    let path = recovery_path(&dir, kind);
    crate::atomic_write::write(&path, contents)
        .with_context(|| format!("Failed to preserve recovery file {}", path.display()))?;
    prune(&dir, kind, Some(&path))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (
        std::sync::MutexGuard<'static, ()>,
        crate::test_helpers::EnvVarGuard,
        tempfile::TempDir,
    ) {
        let guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let env = crate::test_helpers::EnvVarGuard::set("XDG_CONFIG_HOME", dir.path());
        (guard, env, dir)
    }

    #[test]
    fn preserve_refreshes_duplicates_and_keeps_three_per_kind() {
        let (_guard, _env, root) = setup();
        let first = preserve(RecoveryKind::Conflict, b"same").unwrap();
        let refreshed = preserve(RecoveryKind::Conflict, b"same").unwrap();
        assert_ne!(refreshed, first);
        assert!(!first.exists());
        assert_eq!(fs::read(&refreshed).unwrap(), b"same");
        for value in 0..4 {
            preserve(RecoveryKind::Conflict, format!("value-{value}").as_bytes()).unwrap();
        }
        let dir = root.path().join("kvn-tui/recovery");
        let files = managed_files(&dir, RecoveryKind::Conflict).unwrap();
        assert_eq!(files.len(), 3);
        assert!(
            files
                .iter()
                .any(|path| fs::read(path).unwrap() == b"value-3")
        );
    }

    #[test]
    fn categories_are_pruned_independently() {
        let (_guard, _env, root) = setup();
        for kind in RecoveryKind::ALL {
            for value in 0..4 {
                preserve(kind, format!("{kind:?}-{value}").as_bytes()).unwrap();
            }
        }
        let dir = root.path().join("kvn-tui/recovery");
        for kind in RecoveryKind::ALL {
            assert_eq!(managed_files(&dir, kind).unwrap().len(), 3);
        }
    }

    #[test]
    fn refreshed_duplicate_is_not_removed_by_retention() {
        let (_guard, _env, root) = setup();
        let config_dir = root.path().join("kvn-tui");
        let profiles_path = config_dir.join("profiles.json");
        let dir = recovery_dir(&profiles_path).unwrap();
        let ids = [
            "11111111-1111-4111-8111-111111111111",
            "22222222-2222-4222-8222-222222222222",
            "33333333-3333-4333-8333-333333333333",
            "44444444-4444-4444-8444-444444444444",
        ];
        let mut paths = Vec::new();
        for (index, id) in ids.into_iter().enumerate() {
            let path = dir.join(format!("profiles.json.conflict-20260903T00000{index}-{id}"));
            fs::write(&path, format!("value-{index}")).unwrap();
            paths.push(path);
        }

        let preserved = preserve_at(&profiles_path, RecoveryKind::Conflict, b"value-0").unwrap();

        assert_ne!(preserved, paths[0]);
        assert!(!paths[0].exists());
        assert!(preserved.exists());
        assert_eq!(
            managed_files(&dir, RecoveryKind::Conflict).unwrap().len(),
            3
        );
        assert!(!paths[1].exists());
    }

    #[test]
    fn maintain_does_not_touch_sidecars_outside_recovery_directory() {
        let (_guard, _env, root) = setup();
        let config_dir = root.path().join("kvn-tui");
        fs::create_dir_all(&config_dir).unwrap();
        let id = "741f491f-f801-4218-a80a-679db1ee9b9a";
        let conflict = config_dir.join(format!("profiles.json.conflict-20260903T000000-{id}"));
        let backup = config_dir.join("profiles.json.backup-2026-09-03");
        fs::write(&conflict, "conflict").unwrap();
        fs::write(&backup, "backup").unwrap();

        maintain().unwrap();

        assert!(conflict.exists());
        assert!(
            !config_dir
                .join("recovery")
                .join(conflict.file_name().unwrap())
                .exists()
        );
        assert!(backup.exists());
    }

    #[test]
    fn recovery_directory_is_private() {
        let (_guard, _env, root) = setup();
        preserve(RecoveryKind::Archive, b"secret").unwrap();
        let dir = root.path().join("kvn-tui/recovery");
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let file = managed_files(&dir, RecoveryKind::Archive)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(
            fs::metadata(file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
