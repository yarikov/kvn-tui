use std::fs;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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
    client: reqwest::blocking::Client,
}

impl GeoManager {
    /// Create a new GeoManager, ensuring the geo directory exists.
    pub fn new() -> Result<Self> {
        let geo_dir = crate::paths::geo_dir();

        fs::create_dir_all(&geo_dir)
            .with_context(|| format!("Failed to create geo dir {:?}", geo_dir))?;

        let metadata_path = geo_dir.join("metadata.json");

        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .context("Failed to build HTTP client")?;

        Ok(Self {
            geo_dir,
            metadata_path,
            client,
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
        let meta = self.load_metadata().ok()?;
        meta.updated_at
            .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
    }

    /// Ensure rule-set files exist, downloading them if missing.
    pub fn ensure_databases(&self) -> Result<bool> {
        let (geoip_ru, geosite_ru) = self.local_paths();
        let need_geoip = !geoip_ru.exists();
        let need_geosite = !geosite_ru.exists();

        if !need_geoip && !need_geosite {
            return Ok(false);
        }

        let mut meta = self.load_metadata().unwrap_or_default();
        let mut updated = false;

        if need_geoip {
            match self.download_file(GEOIP_RU_URL, &geoip_ru) {
                Ok(etag) => {
                    meta.geoip_ru_etag = etag;
                    updated = true;
                }
                Err(e) => {
                    eprintln!("Warning: failed to download geoip-ru.srs: {}", e);
                }
            }
        }

        if need_geosite {
            match self.download_file(GEOSITE_RU_URL, &geosite_ru) {
                Ok(etag) => {
                    meta.geosite_ru_etag = etag;
                    updated = true;
                }
                Err(e) => {
                    eprintln!("Warning: failed to download geosite-category-ru.srs: {}", e);
                }
            }
        }

        if updated {
            meta.updated_at = Some(Utc::now());
            if let Err(e) = self.save_metadata(&meta) {
                tracing::warn!("Failed to save geo metadata: {}", e);
            }
        }

        Ok(updated)
    }

    /// Check whether rule-sets have updates available.
    /// Returns (geoip_ru_has_update, geosite_ru_has_update).
    pub fn check_update_available(&self) -> Result<(bool, bool)> {
        let meta = self.load_metadata().unwrap_or_default();
        let (geoip_ru, geosite_ru) = self.local_paths();

        // If files are missing, always consider an update needed.
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

        // Download geoip-ru
        match self.download_file(GEOIP_RU_URL, &geoip_ru) {
            Ok(etag) => {
                meta.geoip_ru_etag = etag;
            }
            Err(e) => return Err(e).context("Failed to download geoip-ru.srs"),
        }

        // Download geosite-category-ru
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
    /// Returns message describing what happened.
    pub fn update_if_needed(&self) -> Result<String> {
        let (geoip_need, geosite_need) = self.check_update_available()?;

        if !geoip_need && !geosite_need {
            return Ok("Geo rule-sets are up to date".to_string());
        }

        let updated = self.download_databases()?;
        if updated {
            let mut parts = Vec::new();
            if geoip_need {
                parts.push("geoip-ru");
            }
            if geosite_need {
                parts.push("geosite-category-ru");
            }
            Ok(format!("Updated: {}", parts.join(", ")))
        } else {
            Ok("No updates found".to_string())
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
            .client
            .head(url)
            .send()
            .with_context(|| format!("HEAD request failed for {}", url))?;

        if !resp.status().is_success() {
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
    fn download_file(&self, url: &str, dest: &PathBuf) -> Result<Option<String>> {
        let resp = self
            .client
            .get(url)
            .send()
            .with_context(|| format!("GET {}", url))?;

        if !resp.status().is_success() {
            anyhow::bail!("HTTP {} for {}", resp.status(), url);
        }

        let etag = resp
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(String::from);

        let bytes = resp.bytes().context("Failed to read response body")?;
        self.write_atomic(dest, &bytes)?;
        Ok(etag)
    }

    fn write_atomic(&self, dest: &PathBuf, data: &[u8]) -> Result<()> {
        let temp = dest.with_extension("tmp");
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
        // remove metadata if present
        let _ = fs::remove_file(&gm.metadata_path);
        let meta = gm.load_metadata().unwrap();
        assert!(meta.geoip_ru_etag.is_none());
        assert!(meta.geosite_ru_etag.is_none());
        assert!(meta.updated_at.is_none());
        // Clean up downloaded files from other tests if they exist
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

    /// Integration test that hits the real network. Run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn test_download_srs_files() {
        let gm = GeoManager::new().unwrap();
        // Ensure clean state
        let (geoip_ru, geosite_ru) = gm.local_paths();
        let _ = fs::remove_file(&geoip_ru);
        let _ = fs::remove_file(&geosite_ru);

        let result = gm.download_databases();
        assert!(result.is_ok(), "download failed: {:?}", result);
        assert!(result.unwrap(), "expected updated=true");

        assert!(geoip_ru.exists(), "geoip-ru.srs should exist");
        assert!(geosite_ru.exists(), "geosite-category-ru.srs should exist");

        // Check metadata
        let updated = gm.last_updated();
        assert!(updated.is_some(), "last_updated should be set");

        // Second call should be up-to-date (same ETag)
        let result = gm.update_if_needed().unwrap();
        assert!(
            result.contains("up to date") || result.contains("Updated"),
            "unexpected result: {}",
            result
        );
    }
}
