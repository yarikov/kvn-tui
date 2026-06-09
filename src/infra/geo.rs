use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::app::msg::GeoResult;
use crate::config::profile::GeoRegion;

const GEOIP_RU_URL: &str =
    "https://raw.githubusercontent.com/SagerNet/sing-geoip/rule-set/geoip-ru.srs";
const GEOSITE_RU_URL: &str =
    "https://raw.githubusercontent.com/SagerNet/sing-geosite/rule-set/geosite-category-ru.srs";
const GEOIP_CN_URL: &str =
    "https://raw.githubusercontent.com/SagerNet/sing-geoip/rule-set/geoip-cn.srs";
const GEOSITE_CN_URL: &str =
    "https://raw.githubusercontent.com/SagerNet/sing-geosite/rule-set/geosite-cn.srs";
const GEOIP_IR_URL: &str =
    "https://raw.githubusercontent.com/SagerNet/sing-geoip/rule-set/geoip-ir.srs";
const GEOSITE_IR_URL: &str =
    "https://raw.githubusercontent.com/SagerNet/sing-geosite/rule-set/geosite-category-ir.srs";

/// Metadata tracking ETags and update time for geo rule-sets.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct GeoMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    geoip_ru_etag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    geosite_ru_etag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    geoip_cn_etag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    geosite_cn_etag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    geoip_ir_etag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    geosite_ir_etag: Option<String>,
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

    /// Return paths to local RU rule-set files.
    pub fn local_paths(&self) -> (PathBuf, PathBuf) {
        let geoip_ru = self.geo_dir.join("geoip-ru.srs");
        let geosite_ru = self.geo_dir.join("geosite-category-ru.srs");
        (geoip_ru, geosite_ru)
    }

    /// Return paths to local CN rule-set files.
    pub fn local_paths_cn(&self) -> (PathBuf, PathBuf) {
        let geoip_cn = self.geo_dir.join("geoip-cn.srs");
        let geosite_cn = self.geo_dir.join("geosite-cn.srs");
        (geoip_cn, geosite_cn)
    }

    /// Return paths to local IR rule-set files.
    pub fn local_paths_ir(&self) -> (PathBuf, PathBuf) {
        let geoip_ir = self.geo_dir.join("geoip-ir.srs");
        let geosite_ir = self.geo_dir.join("geosite-category-ir.srs");
        (geoip_ir, geosite_ir)
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

    /// Check whether rule-sets have updates available for the given region.
    /// Returns (geoip_has_update, geosite_has_update).
    pub fn check_update_available(&self, region: GeoRegion) -> Result<(bool, bool)> {
        let meta = self.load_metadata().unwrap_or_default();

        match region {
            GeoRegion::Global => Ok((false, false)),
            GeoRegion::Ru => {
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
            GeoRegion::Cn => {
                let (geoip_cn, geosite_cn) = self.local_paths_cn();
                let geoip_missing = !geoip_cn.exists();
                let geosite_missing = !geosite_cn.exists();

                let geoip_update = if geoip_missing {
                    true
                } else {
                    self.check_single(GEOIP_CN_URL, meta.geoip_cn_etag.as_deref())?
                };

                let geosite_update = if geosite_missing {
                    true
                } else {
                    self.check_single(GEOSITE_CN_URL, meta.geosite_cn_etag.as_deref())?
                };

                Ok((geoip_update, geosite_update))
            }
            GeoRegion::Ir => {
                let (geoip_ir, geosite_ir) = self.local_paths_ir();
                let geoip_missing = !geoip_ir.exists();
                let geosite_missing = !geosite_ir.exists();

                let geoip_update = if geoip_missing {
                    true
                } else {
                    self.check_single(GEOIP_IR_URL, meta.geoip_ir_etag.as_deref())?
                };

                let geosite_update = if geosite_missing {
                    true
                } else {
                    self.check_single(GEOSITE_IR_URL, meta.geosite_ir_etag.as_deref())?
                };

                Ok((geoip_update, geosite_update))
            }
        }
    }

    /// Download rule-sets for the given region and update metadata atomically.
    pub fn download_databases(&self, region: GeoRegion) -> Result<bool> {
        let mut meta = self.load_metadata().unwrap_or_default();

        match region {
            GeoRegion::Global => return Ok(false),
            GeoRegion::Ru => {
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
            }
            GeoRegion::Cn => {
                let (geoip_cn, geosite_cn) = self.local_paths_cn();

                match self.download_file(GEOIP_CN_URL, &geoip_cn) {
                    Ok(etag) => {
                        meta.geoip_cn_etag = etag;
                    }
                    Err(e) => return Err(e).context("Failed to download geoip-cn.srs"),
                }

                match self.download_file(GEOSITE_CN_URL, &geosite_cn) {
                    Ok(etag) => {
                        meta.geosite_cn_etag = etag;
                    }
                    Err(e) => return Err(e).context("Failed to download geosite-cn.srs"),
                }
            }
            GeoRegion::Ir => {
                let (geoip_ir, geosite_ir) = self.local_paths_ir();

                match self.download_file(GEOIP_IR_URL, &geoip_ir) {
                    Ok(etag) => {
                        meta.geoip_ir_etag = etag;
                    }
                    Err(e) => return Err(e).context("Failed to download geoip-ir.srs"),
                }

                match self.download_file(GEOSITE_IR_URL, &geosite_ir) {
                    Ok(etag) => {
                        meta.geosite_ir_etag = etag;
                    }
                    Err(e) => return Err(e).context("Failed to download geosite-category-ir.srs"),
                }
            }
        }

        meta.updated_at = Some(Utc::now());
        self.save_metadata(&meta)?;

        Ok(true)
    }

    /// Full update flow: check then download if needed.
    /// Returns typed result describing what happened.
    pub fn update_if_needed(&self, region: GeoRegion) -> Result<GeoResult> {
        if matches!(region, GeoRegion::Global) {
            return Ok(GeoResult::UpToDate);
        }

        let (geoip_need, geosite_need) = self.check_update_available(region)?;

        if !geoip_need && !geosite_need {
            return Ok(GeoResult::UpToDate);
        }

        let updated = self.download_databases(region)?;
        if updated {
            let mut parts = Vec::new();
            if geoip_need {
                parts.push(format!("geoip-{}", region.as_str()));
            }
            if geosite_need {
                parts.push(format!("geosite-{}", region.as_str()));
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
        let (geoip_cn, geosite_cn) = gm.local_paths_cn();
        assert!(geoip_cn.file_name().unwrap() == "geoip-cn.srs");
        assert!(geosite_cn.file_name().unwrap() == "geosite-cn.srs");
    }

    #[test]
    fn metadata_roundtrip() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let gm = GeoManager::new().unwrap();
        let meta = GeoMetadata {
            geoip_ru_etag: Some("etag1".to_string()),
            geosite_ru_etag: Some("etag2".to_string()),
            geoip_cn_etag: Some("etag3".to_string()),
            geosite_cn_etag: Some("etag4".to_string()),
            geoip_ir_etag: Some("etag5".to_string()),
            geosite_ir_etag: Some("etag6".to_string()),
            updated_at: Some(Utc::now()),
        };
        gm.save_metadata(&meta).unwrap();
        let loaded = gm.load_metadata().unwrap();
        assert_eq!(loaded.geoip_ru_etag, Some("etag1".to_string()));
        assert_eq!(loaded.geosite_ru_etag, Some("etag2".to_string()));
        assert_eq!(loaded.geoip_cn_etag, Some("etag3".to_string()));
        assert_eq!(loaded.geosite_cn_etag, Some("etag4".to_string()));
        assert_eq!(loaded.geoip_ir_etag, Some("etag5".to_string()));
        assert_eq!(loaded.geosite_ir_etag, Some("etag6".to_string()));
        assert!(loaded.updated_at.is_some());
    }

    #[test]
    fn load_metadata_missing_returns_default() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let gm = GeoManager::new().unwrap();
        let (geoip_ru, geosite_ru) = gm.local_paths();
        let (geoip_cn, geosite_cn) = gm.local_paths_cn();
        let _ = fs::remove_file(&gm.metadata_path);
        let meta = gm.load_metadata().unwrap();
        assert!(meta.geoip_ru_etag.is_none());
        assert!(meta.geosite_ru_etag.is_none());
        assert!(meta.geoip_cn_etag.is_none());
        assert!(meta.geosite_cn_etag.is_none());
        assert!(meta.geoip_ir_etag.is_none());
        assert!(meta.geosite_ir_etag.is_none());
        assert!(meta.updated_at.is_none());
        let _ = fs::remove_file(&geoip_ru);
        let _ = fs::remove_file(&geosite_ru);
        let _ = fs::remove_file(&geoip_cn);
        let _ = fs::remove_file(&geosite_cn);
    }

    #[test]
    fn write_atomic_creates_file() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
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
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
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

        let result = gm.download_databases(crate::config::profile::GeoRegion::Ru);
        assert!(result.is_ok(), "download failed: {:?}", result);
        assert!(result.unwrap(), "expected updated=true");

        assert!(geoip_ru.exists(), "geoip-ru.srs should exist");
        assert!(geosite_ru.exists(), "geosite-category-ru.srs should exist");

        let updated = gm.last_updated();
        assert!(updated.is_some(), "last_updated should be set");

        let result = gm
            .update_if_needed(crate::config::profile::GeoRegion::Ru)
            .unwrap();
        assert!(
            matches!(result, GeoResult::UpToDate),
            "unexpected result: {:?}",
            result
        );
    }
}
