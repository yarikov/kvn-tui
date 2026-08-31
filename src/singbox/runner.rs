use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{ChildStderr, Command, Stdio};
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::config::profile::{Profile, Settings};
use crate::singbox::config::{GeoAvailability, generate_config};
use crate::singbox::process_handle::ProcessHandle;

fn resolve_singbox_binary() -> String {
    std::env::var("SING_BOX_PATH").unwrap_or_else(|_| "sing-box".to_string())
}

static SINGBOX_BINARY: OnceLock<String> = OnceLock::new();

/// Path to the sing-box binary. Can be overridden by SING_BOX_PATH env variable.
fn singbox_binary() -> &'static str {
    SINGBOX_BINARY.get_or_init(resolve_singbox_binary)
}

/// Write the generated sing-box configuration to a temporary file.
fn write_config(profile: &Profile, settings: &Settings, geo: &GeoAvailability) -> Result<PathBuf> {
    let config =
        generate_config(profile, settings, geo).context("Failed to generate sing-box config")?;
    crate::paths::ensure_runtime_dir()?;
    let path = crate::paths::temp_singbox_config_path()?;

    crate::atomic_write::write(&path, serde_json::to_string_pretty(&config)?.as_bytes())
        .with_context(|| format!("Failed to write config to {:?}", path))?;

    Ok(path)
}

/// Validate the sing-box configuration by running `sing-box check`.
fn check_config(path: &PathBuf) -> Result<()> {
    let output = Command::new(singbox_binary())
        .arg("check")
        .arg("-c")
        .arg(path)
        .output()
        .with_context(|| format!("Failed to run {} check", singbox_binary()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("sing-box config validation failed: {}", stderr);
    }

    Ok(())
}

fn collect_geo_availability() -> GeoAvailability {
    let mut geo = GeoAvailability::default();
    let Ok(gm) = crate::geo::GeoManager::new() else {
        return geo;
    };
    for region in crate::config::profile::GeoRegion::ALL {
        let Some((geoip, geosite)) = gm.local_paths(region) else {
            continue;
        };
        if geoip.exists() && geosite.exists() {
            geo.regions.insert(region, (geoip, geosite));
        }
    }
    for service in crate::config::profile::RoutedService::ALL {
        if gm.has_service_databases(service) {
            geo.services
                .insert(service, gm.service_local_paths(service));
        }
    }
    geo
}

/// Start the sing-box process with the given profile.
/// Validates config first, then spawns the process and verifies it stays alive.
pub fn start(profile: &Profile, settings: &Settings) -> Result<ProcessHandle> {
    let geo = collect_geo_availability();
    let config_path = write_config(profile, settings, &geo)?;

    // Validate configuration before starting.
    check_config(&config_path)?;

    // We don't consume sing-box stdout; drop it to /dev/null so a verbose
    // logger can't fill the pipe buffer (~64K) and wedge the child on write.
    // stderr stays piped — on immediate exit we read it for diagnostics, and
    // on success we hand it to a drain thread (see `spawn_stderr_drain`).
    let mut child = Command::new(singbox_binary())
        .arg("run")
        .arg("-c")
        .arg(&config_path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to start sing-box (binary: {})", singbox_binary()))?;

    // Poll for either an immediate exit (config rejected, port busy, etc.)
    // or a stable run. The 300ms budget is the same heuristic as before —
    // sing-box that survives this window is considered up — but we now
    // detect an early failure within ~10ms instead of always waiting the
    // full window.
    let deadline = std::time::Instant::now() + Duration::from_millis(300);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stderr = String::new();
                if let Some(ref mut err) = child.stderr
                    && let Err(e) = err.read_to_string(&mut stderr)
                {
                    tracing::warn!("Failed to read sing-box stderr: {}", e);
                }
                anyhow::bail!(
                    "sing-box exited immediately (code: {:?}). stderr: {}",
                    status.code(),
                    stderr.trim()
                );
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    // Process survived the readiness window — drain stderr so
                    // the child doesn't block on a full pipe buffer, then hand
                    // the child off.
                    if let Some(stderr) = child.stderr.take() {
                        spawn_stderr_drain(stderr);
                    }
                    return Ok(ProcessHandle::new(child));
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => {
                anyhow::bail!("Failed to check sing-box status: {}", e);
            }
        }
    }
}

/// Forward sing-box stderr lines to the `singbox` tracing target so verbose
/// output never wedges the child. The thread exits naturally on EOF (i.e.
/// when sing-box is killed and the pipe closes).
fn spawn_stderr_drain(stderr: ChildStderr) {
    thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            match line {
                Ok(line) => tracing::info!(target: "singbox", "{line}"),
                Err(_) => break,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::profile::Profile;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn singbox_binary_resolution() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        // Default (no env override)
        unsafe { std::env::remove_var("SING_BOX_PATH") };
        assert_eq!(resolve_singbox_binary(), "sing-box");

        // With env override
        unsafe { std::env::set_var("SING_BOX_PATH", "/usr/local/bin/sing-box") };
        assert_eq!(resolve_singbox_binary(), "/usr/local/bin/sing-box");
        unsafe { std::env::remove_var("SING_BOX_PATH") };
    }

    #[test]
    fn write_config_creates_valid_json() {
        // temp_singbox_config_path() reads XDG_RUNTIME_DIR, which other tests
        // point at short-lived tempdirs — hold ENV_LOCK so the path stays valid.
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let _runtime = crate::test_helpers::EnvVarGuard::set("XDG_RUNTIME_DIR", runtime.path());
        let profile = Profile::new_vless(
            "Test".to_string(),
            "1.2.3.4".to_string(),
            443,
            "uuid".to_string(),
        );
        let settings = Settings::default();
        let path = write_config(&profile, &settings, &GeoAvailability::default()).unwrap();
        assert!(path.exists());

        let contents = fs::read_to_string(&path).unwrap();
        let json: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert!(json.get("log").is_some());
        assert!(json.get("outbounds").is_some());
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        // Clean up
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn collect_geo_availability_is_empty_when_no_files() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let geo = collect_geo_availability();
        assert!(geo.regions.is_empty());
    }

    #[test]
    fn collect_geo_availability_populates_each_region_independently() {
        use crate::config::profile::GeoRegion;
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let gm = crate::geo::GeoManager::new().unwrap();

        // Add one region at a time, asserting that prior regions remain
        // available and unconfigured regions stay absent.
        let mut written = Vec::new();
        for region in [GeoRegion::Ru, GeoRegion::Cn, GeoRegion::Ir] {
            let (geoip, geosite) = gm.local_paths(region).unwrap();
            fs::write(&geoip, b"x").unwrap();
            fs::write(&geosite, b"x").unwrap();
            written.push(region);

            let geo = collect_geo_availability();
            for r in [GeoRegion::Ru, GeoRegion::Cn, GeoRegion::Ir] {
                assert_eq!(
                    geo.regions.contains_key(&r),
                    written.contains(&r),
                    "region {:?} after writing {:?}",
                    r,
                    written
                );
            }
        }
    }

    #[test]
    fn collect_geo_availability_skips_region_when_only_one_file_present() {
        use crate::config::profile::GeoRegion;
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let gm = crate::geo::GeoManager::new().unwrap();
        let (geoip_ru, _) = gm.local_paths(GeoRegion::Ru).unwrap();
        fs::write(&geoip_ru, b"x").unwrap();
        // geosite missing → Ru must stay absent.
        let geo = collect_geo_availability();
        assert!(!geo.regions.contains_key(&GeoRegion::Ru));
    }

    #[test]
    fn collect_geo_availability_includes_service_when_all_files_present() {
        use crate::config::profile::RoutedService;

        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let gm = crate::geo::GeoManager::new().unwrap();
        let paths = gm.service_local_paths(RoutedService::Steam);

        assert!(
            !collect_geo_availability()
                .services
                .contains_key(&RoutedService::Steam)
        );
        fs::write(&paths[0].1, b"x").unwrap();
        assert!(
            !collect_geo_availability()
                .services
                .contains_key(&RoutedService::Steam),
            "geoip missing"
        );
        fs::write(&paths[1].1, b"x").unwrap();
        assert_eq!(
            collect_geo_availability()
                .services
                .get(&RoutedService::Steam),
            Some(&paths)
        );
    }

    /// Validation against the real `sing-box` binary. Skipped automatically
    /// when the binary is not on PATH so CI without sing-box stays green.
    #[test]
    fn check_config_accepts_a_minimal_profile() {
        // Same XDG_RUNTIME_DIR dependency as write_config_creates_valid_json.
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let _runtime = crate::test_helpers::EnvVarGuard::set("XDG_RUNTIME_DIR", runtime.path());
        if Command::new("sh")
            .args(["-c", "command -v sing-box >/dev/null 2>&1"])
            .status()
            .map(|s| !s.success())
            .unwrap_or(true)
        {
            eprintln!("skipping: sing-box not on PATH");
            return;
        }
        let profile =
            Profile::new_vless("T".to_string(), "1.1.1.1".to_string(), 443, "u".to_string());
        let settings = Settings::default();
        let path = write_config(&profile, &settings, &GeoAvailability::default()).unwrap();
        check_config(&path).expect("sing-box rejected a minimal vless profile");
        let _ = fs::remove_file(&path);
    }
}
