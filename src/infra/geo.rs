use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::app::msg::GeoResult;

const GEOIP_RU_URL: &str =
    "https://raw.githubusercontent.com/SagerNet/sing-geoip/rule-set/geoip-ru.srs";
const GEOSITE_RU_URL: &str =
    "https://raw.githubusercontent.com/SagerNet/sing-geosite/rule-set/geosite-category-ru.srs";

/// Metadata tracking ETags and update time for geo rule-sets.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct GeoMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    geoip_ru_etag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    geosite_ru_etag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_at: Option<DateTime<Utc>>,
}

/// Manages downloading and updating geoip/geosite rule-sets for sing-box.
pub struct GeoManager {
    geo_dir: PathBuf,
    metadata_path: PathBuf,
    agent: ureq::Agent,
}

impl GeoManager {
    /// Create a new GeoManager, ensuring the geo directory exists.
    pub fn new() -> Result<Self> {
        let geo_dir = crate::infra::paths::geo_dir();

        fs::create_dir_all(&geo_dir)
            .with_context(|| format!("Failed to create geo dir {:?}", geo_dir))?;

        let metadata_path = geo_dir.join("metadata.json");
        let agent = ureq::Agent::new_with_defaults();

        Ok(Self {
            geo_dir,
            metadata_path,
            agent,
        })
    }

    /// Return paths to local rule-set files.
    pub fn local_paths(&self) -> (PathBuf, PathBuf) {
        let geoip_ru = self.geo_dir.join("geoip-ru.srs");
        let geosite_ru = self.geo_dir.join("geosite-category-ru.srs");
        (geoip_ru, geosite_ru)
    }

    /// Return a human-readable string of the last update time, or None.
    pub fn last_updated(&self) -> Option<String> {
        #[cfg(test)]
        {
            if let Some(override_val) = TEST_LAST_UPDATED.lock().unwrap().clone() {
                return override_val;
            }
        }
        let meta = self.load_metadata().ok()?;
        meta.updated_at
            .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
    }

    /// Check whether rule-sets have updates available.
    /// Returns (geoip_ru_has_update, geosite_ru_has_update).
    pub fn check_update_available(&self) -> Result<(bool, bool)> {
        let meta = self.load_metadata().unwrap_or_default();
        let (geoip_ru, geosite_ru) = self.local_paths();

        let geoip_missing = !geoip_ru.exists();
        let geosite_missing = !geosite_ru.exists();

        let geoip_update = if geoip_missing {
            true
        } else {
            self.check_single(GEOIP_RU_URL, meta.geoip_ru_etag.as_deref())?
        };

        let geosite_update = if geosite_missing {
            true
        } else {
            self.check_single(GEOSITE_RU_URL, meta.geosite_ru_etag.as_deref())?
        };

        Ok((geoip_update, geosite_update))
    }

    /// Download both rule-sets and update metadata atomically.
    pub fn download_databases(&self) -> Result<bool> {
        let mut meta = self.load_metadata().unwrap_or_default();
        let (geoip_ru, geosite_ru) = self.local_paths();

        match self.download_file(GEOIP_RU_URL, &geoip_ru) {
            Ok(etag) => {
                meta.geoip_ru_etag = etag;
            }
            Err(e) => return Err(e).context("Failed to download geoip-ru.srs"),
        }

        match self.download_file(GEOSITE_RU_URL, &geosite_ru) {
            Ok(etag) => {
                meta.geosite_ru_etag = etag;
            }
            Err(e) => return Err(e).context("Failed to download geosite-category-ru.srs"),
        }

        meta.updated_at = Some(Utc::now());
        self.save_metadata(&meta)?;

        Ok(true)
    }

    /// Full update flow: check then download if needed.
    /// Returns typed result describing what happened.
    pub fn update_if_needed(&self) -> Result<GeoResult> {
        let (geoip_need, geosite_need) = self.check_update_available()?;

        if !geoip_need && !geosite_need {
            return Ok(GeoResult::UpToDate);
        }

        let updated = self.download_databases()?;
        if updated {
            let mut parts = Vec::new();
            if geoip_need {
                parts.push("geoip-ru".to_string());
            }
            if geosite_need {
                parts.push("geosite-category-ru".to_string());
            }
            Ok(GeoResult::Updated(parts))
        } else {
            Ok(GeoResult::UpToDate)
        }
    }

    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    fn load_metadata(&self) -> Result<GeoMetadata> {
        if !self.metadata_path.exists() {
            return Ok(GeoMetadata::default());
        }
        let text = fs::read_to_string(&self.metadata_path)
            .with_context(|| format!("Failed to read {:?}", self.metadata_path))?;
        let meta: GeoMetadata = serde_json::from_str(&text)
            .with_context(|| format!("Failed to parse {:?}", self.metadata_path))?;
        Ok(meta)
    }

    fn save_metadata(&self, meta: &GeoMetadata) -> Result<()> {
        let text = serde_json::to_string_pretty(meta)?;
        self.write_atomic(&self.metadata_path, text.as_bytes())?;
        Ok(())
    }

    fn check_single(&self, url: &str, saved_etag: Option<&str>) -> Result<bool> {
        let resp = self
            .agent
            .head(url)
            .call()
            .with_context(|| format!("HEAD request failed for {}", url))?;

        if resp.status() != 200 {
            return Ok(true); // assume update needed if we can't check
        }

        let remote_etag = resp.headers().get("etag").and_then(|v| v.to_str().ok());

        match (saved_etag, remote_etag) {
            (Some(saved), Some(remote)) => Ok(saved != remote),
            (None, _) => Ok(true),
            _ => Ok(true),
        }
    }

    /// Download a file and return its ETag on success.
    fn download_file(&self, url: &str, dest: &Path) -> Result<Option<String>> {
        let resp = self
            .agent
            .get(url)
            .call()
            .with_context(|| format!("GET {}", url))?;

        if resp.status() != 200 {
            anyhow::bail!("HTTP {} for {}", resp.status(), url);
        }

        let etag = resp
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(String::from);

        let bytes = resp
            .into_body()
            .read_to_vec()
            .context("Failed to read response body")?;
        self.write_atomic(dest, &bytes)?;
        Ok(etag)
    }

    fn write_atomic(&self, dest: &Path, data: &[u8]) -> Result<()> {
        let name = dest.file_name().unwrap_or_default();
        let temp = dest.with_file_name(format!("{}.tmp", name.to_string_lossy()));
        let mut file = fs::File::create(&temp)
            .with_context(|| format!("Failed to create temp file {:?}", temp))?;
        file.write_all(data)
            .with_context(|| format!("Failed to write temp file {:?}", temp))?;
        drop(file);
        fs::rename(&temp, dest)
            .with_context(|| format!("Failed to rename {:?} -> {:?}", temp, dest))?;
        Ok(())
    }
}

#[cfg(test)]
static TEST_LAST_UPDATED: std::sync::Mutex<Option<Option<String>>> = std::sync::Mutex::new(None);

#[cfg(test)]
/// Override `GeoManager::last_updated` for tests.
pub fn set_test_last_updated(value: Option<String>) {
    *TEST_LAST_UPDATED.lock().unwrap() = Some(value);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_paths_are_inside_geo_dir() {
        let gm = GeoManager::new().unwrap();
        let (geoip_ru, geosite_ru) = gm.local_paths();
        assert!(geoip_ru.file_name().unwrap() == "geoip-ru.srs");
        assert!(geosite_ru.file_name().unwrap() == "geosite-category-ru.srs");
    }

    #[test]
    fn metadata_roundtrip() {
        let gm = GeoManager::new().unwrap();
        let meta = GeoMetadata {
            geoip_ru_etag: Some("etag1".to_string()),
            geosite_ru_etag: Some("etag2".to_string()),
            updated_at: Some(Utc::now()),
        };
        gm.save_metadata(&meta).unwrap();
        let loaded = gm.load_metadata().unwrap();
        assert_eq!(loaded.geoip_ru_etag, Some("etag1".to_string()));
        assert_eq!(loaded.geosite_ru_etag, Some("etag2".to_string()));
        assert!(loaded.updated_at.is_some());
    }

    #[test]
    fn load_metadata_missing_returns_default() {
        let gm = GeoManager::new().unwrap();
        let (geoip_ru, geosite_ru) = gm.local_paths();
        let _ = fs::remove_file(&gm.metadata_path);
        let meta = gm.load_metadata().unwrap();
        assert!(meta.geoip_ru_etag.is_none());
        assert!(meta.geosite_ru_etag.is_none());
        assert!(meta.updated_at.is_none());
        let _ = fs::remove_file(&geoip_ru);
        let _ = fs::remove_file(&geosite_ru);
    }

    #[test]
    fn write_atomic_creates_file() {
        let gm = GeoManager::new().unwrap();
        let dest = gm.geo_dir.join("test_atomic.txt");
        let _ = fs::remove_file(&dest);
        gm.write_atomic(&dest, b"hello world").unwrap();
        assert!(dest.exists());
        let contents = fs::read_to_string(&dest).unwrap();
        assert_eq!(contents, "hello world");
        let _ = fs::remove_file(&dest);
    }

    #[test]
    fn write_atomic_preserves_srs_extension() {
        let gm = GeoManager::new().unwrap();
        let dest = gm.geo_dir.join("geoip-ru.srs");
        let _ = fs::remove_file(&dest);
        gm.write_atomic(&dest, b"data").unwrap();
        assert!(dest.exists());
        // Temp file should have been geoip-ru.srs.tmp, not geoip-ru.tmp
        let temp = gm.geo_dir.join("geoip-ru.srs.tmp");
        assert!(!temp.exists());
        let _ = fs::remove_file(&dest);
    }

    /// Integration test that hits the real network. Run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn test_download_srs_files() {
        let gm = GeoManager::new().unwrap();
        let (geoip_ru, geosite_ru) = gm.local_paths();
        let _ = fs::remove_file(&geoip_ru);
        let _ = fs::remove_file(&geosite_ru);

        let result = gm.download_databases();
        assert!(result.is_ok(), "download failed: {:?}", result);
        assert!(result.unwrap(), "expected updated=true");

        assert!(geoip_ru.exists(), "geoip-ru.srs should exist");
        assert!(geosite_ru.exists(), "geosite-category-ru.srs should exist");

        let updated = gm.last_updated();
        assert!(updated.is_some(), "last_updated should be set");

        let result = gm.update_if_needed().unwrap();
        assert!(
            matches!(result, GeoResult::UpToDate),
            "unexpected result: {:?}",
            result
        );
    }
}
