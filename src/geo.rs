use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

use crate::app::msg::GeoResult;
use crate::config::profile::{GeoRegion, RoutedService};

/// One downloadable rule-set file (geoip *or* geosite) for a region.
pub(crate) struct GeoAsset {
    pub(crate) filename: &'static str,
    pub(crate) url: &'static str,
}

impl GeoAsset {
    /// sing-box rule-set tag — the filename without the `.srs` extension
    /// (`"geoip-ru.srs"` → `"geoip-ru"`). The tag is what's referenced from
    /// route rules in the generated sing-box config.
    pub(crate) fn tag(&self) -> &'static str {
        self.filename.trim_end_matches(".srs")
    }
}

/// Both rule-sets (geoip + geosite) a region downloads.
pub(crate) struct RegionAssets {
    pub(crate) geoip: GeoAsset,
    pub(crate) geosite: GeoAsset,
}

/// Single source of truth for which files / URLs back each region. Adding a
/// new country = add one match arm here plus a `GeoRegion::ALL` entry.
///
/// Returns `None` for `GeoRegion::Global`, which has no rule-sets.
pub(crate) fn region_assets(region: GeoRegion) -> Option<RegionAssets> {
    match region {
        GeoRegion::Global => None,
        GeoRegion::Ru => Some(RegionAssets {
            geoip: GeoAsset {
                filename: "geoip-ru.srs",
                url: "https://raw.githubusercontent.com/SagerNet/sing-geoip/rule-set/geoip-ru.srs",
            },
            geosite: GeoAsset {
                filename: "geosite-category-ru.srs",
                url: "https://raw.githubusercontent.com/SagerNet/sing-geosite/rule-set/geosite-category-ru.srs",
            },
        }),
        GeoRegion::Cn => Some(RegionAssets {
            geoip: GeoAsset {
                filename: "geoip-cn.srs",
                url: "https://raw.githubusercontent.com/SagerNet/sing-geoip/rule-set/geoip-cn.srs",
            },
            geosite: GeoAsset {
                filename: "geosite-cn.srs",
                url: "https://raw.githubusercontent.com/SagerNet/sing-geosite/rule-set/geosite-cn.srs",
            },
        }),
        GeoRegion::Ir => Some(RegionAssets {
            geoip: GeoAsset {
                filename: "geoip-ir.srs",
                url: "https://raw.githubusercontent.com/SagerNet/sing-geoip/rule-set/geoip-ir.srs",
            },
            geosite: GeoAsset {
                filename: "geosite-category-ir.srs",
                url: "https://raw.githubusercontent.com/SagerNet/sing-geosite/rule-set/geosite-category-ir.srs",
            },
        }),
    }
}

/// Rule-sets backing one service's routing override (`service_routes`).
/// Either side may be absent when no suitable upstream file exists.
pub(crate) struct ServiceAssets {
    pub(crate) geoip: Option<GeoAsset>,
    pub(crate) geosite: Option<GeoAsset>,
}

impl ServiceAssets {
    /// Defined assets in rule order: geosite first, then geoip — matching
    /// the order rule-set tags are referenced from generated route rules.
    pub(crate) fn present(&self) -> Vec<&GeoAsset> {
        [self.geosite.as_ref(), self.geoip.as_ref()]
            .into_iter()
            .flatten()
            .collect()
    }
}

/// Single source of truth for which files / URLs back each routable service.
///
/// Steam: the geosite file covers Steam domains; the "geoip" file is Valve's
/// AS32590 announced ranges, which catch content servers the Steam client
/// contacts by bare IP (no domain to match on). Local filenames are
/// normalized to the `geoip-`/`geosite-` scheme even though the ASN file's
/// upstream basename differs.
///
/// Telegram: clients mostly connect by bare IP, so the geoip side is what
/// actually matches; the geosite file catches web/API domains.
///
/// All service assets come from MetaCubeX/meta-rules-dat so the subsystem has
/// one provider and one branch layout (the regional RU/CN/IR assets are a
/// separate, SagerNet-backed pipeline).
pub(crate) fn service_assets(service: RoutedService) -> ServiceAssets {
    match service {
        RoutedService::Steam => ServiceAssets {
            geoip: Some(GeoAsset {
                filename: "geoip-steam.srs",
                url: "https://raw.githubusercontent.com/MetaCubeX/meta-rules-dat/sing/asn/AS32590.srs",
            }),
            geosite: Some(GeoAsset {
                filename: "geosite-steam.srs",
                url: "https://raw.githubusercontent.com/MetaCubeX/meta-rules-dat/sing/geo/geosite/steam.srs",
            }),
        },
        RoutedService::Telegram => ServiceAssets {
            geoip: Some(GeoAsset {
                filename: "geoip-telegram.srs",
                url: "https://raw.githubusercontent.com/MetaCubeX/meta-rules-dat/sing/geo/geoip/telegram.srs",
            }),
            geosite: Some(GeoAsset {
                filename: "geosite-telegram.srs",
                url: "https://raw.githubusercontent.com/MetaCubeX/meta-rules-dat/sing/geo/geosite/telegram.srs",
            }),
        },
    }
}

/// Metadata tracking ETags and update time for geo rule-sets.
#[derive(Debug, Clone, Default, Serialize)]
struct GeoMetadata {
    /// Maps filename (e.g. `"geoip-ru.srs"`) → ETag returned by the server on
    /// the last successful download. Used to short-circuit re-downloads.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    etags: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    updated_at: HashMap<GeoRegion, DateTime<Local>>,
    /// Last successful HTTP check, including checks that found no changes.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    checked_at: HashMap<GeoRegion, DateTime<Local>>,
}

/// Deserialization shape that also accepts the legacy per-country etag fields
/// written by older builds. Folded into the modern map-shaped `GeoMetadata`.
#[derive(Debug, Default, Deserialize)]
struct GeoMetadataRaw {
    #[serde(default)]
    etags: HashMap<String, String>,
    #[serde(default)]
    updated_at: HashMap<GeoRegion, DateTime<Local>>,
    #[serde(default)]
    checked_at: HashMap<GeoRegion, DateTime<Local>>,
    #[serde(default)]
    geoip_ru_etag: Option<String>,
    #[serde(default)]
    geosite_ru_etag: Option<String>,
    #[serde(default)]
    geoip_cn_etag: Option<String>,
    #[serde(default)]
    geosite_cn_etag: Option<String>,
    #[serde(default)]
    geoip_ir_etag: Option<String>,
    #[serde(default)]
    geosite_ir_etag: Option<String>,
}

impl From<GeoMetadataRaw> for GeoMetadata {
    fn from(raw: GeoMetadataRaw) -> Self {
        let mut etags = raw.etags;
        let mut promote = |filename: &str, value: Option<String>| {
            if let Some(v) = value {
                etags.entry(filename.to_string()).or_insert(v);
            }
        };
        promote("geoip-ru.srs", raw.geoip_ru_etag);
        promote("geosite-category-ru.srs", raw.geosite_ru_etag);
        promote("geoip-cn.srs", raw.geoip_cn_etag);
        promote("geosite-cn.srs", raw.geosite_cn_etag);
        promote("geoip-ir.srs", raw.geoip_ir_etag);
        promote("geosite-category-ir.srs", raw.geosite_ir_etag);
        GeoMetadata {
            etags,
            updated_at: raw.updated_at,
            checked_at: raw.checked_at,
        }
    }
}

/// Serializes read-modify-write cycles on `metadata.json`. The daemon runs
/// downloads on detached threads (periodic geo updates, region-change fetch,
/// post-connect Steam fetch); without this, two interleaved
/// `load_metadata → save_metadata` cycles lose each other's ETags, forcing
/// spurious re-downloads. Atomic renames prevent corruption, not lost updates.
static METADATA_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Manages downloading and updating geoip/geosite rule-sets for sing-box.
pub struct GeoManager {
    geo_dir: PathBuf,
    metadata_path: PathBuf,
    agent: ureq::Agent,
}

impl GeoManager {
    /// Create a new GeoManager, ensuring the geo directory exists.
    pub fn new() -> Result<Self> {
        let geo_dir = crate::paths::geo_dir();

        fs::create_dir_all(&geo_dir)
            .with_context(|| format!("Failed to create geo dir {:?}", geo_dir))?;

        let metadata_path = geo_dir.join("metadata.json");
        // Bounded setup-phase timeouts so a stalled GitHub connection can't
        // wedge the geo update threads forever. Deliberately no body timeout
        // (`timeout_recv_body` is cumulative, not idle): a multi-hundred-KB
        // `.srs` on a slow pre-VPN link can legitimately take many minutes,
        // and a transfer that is still making progress must not be aborted.
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_resolve(Some(std::time::Duration::from_secs(15)))
            .timeout_connect(Some(std::time::Duration::from_secs(15)))
            .timeout_recv_response(Some(std::time::Duration::from_secs(30)))
            .build()
            .into();

        Ok(Self {
            geo_dir,
            metadata_path,
            agent,
        })
    }

    /// Return `(geoip, geosite)` local paths for `region`, or `None` for
    /// regions without rule-sets (`Global`). Paths are computed; the files
    /// may not exist yet.
    pub fn local_paths(&self, region: GeoRegion) -> Option<(PathBuf, PathBuf)> {
        region_assets(region).map(|a| {
            (
                self.geo_dir.join(a.geoip.filename),
                self.geo_dir.join(a.geosite.filename),
            )
        })
    }

    /// Return whether local rule-set files for the given region are present.
    /// `Global` always returns `true` (nothing to download).
    pub fn has_databases(&self, region: GeoRegion) -> bool {
        match region_assets(region) {
            None => true,
            Some(a) => {
                self.geo_dir.join(a.geoip.filename).exists()
                    && self.geo_dir.join(a.geosite.filename).exists()
            }
        }
    }

    /// Return `(tag, local_path)` for each of `service`'s defined rule-sets,
    /// in rule order (geosite first). Paths are computed; the files may not
    /// exist yet.
    pub(crate) fn service_local_paths(
        &self,
        service: RoutedService,
    ) -> Vec<(&'static str, PathBuf)> {
        service_assets(service)
            .present()
            .into_iter()
            .map(|a| (a.tag(), self.geo_dir.join(a.filename)))
            .collect()
    }

    /// Return whether every rule-set file defined for `service` is present.
    pub fn has_service_databases(&self, service: RoutedService) -> bool {
        self.service_local_paths(service)
            .iter()
            .all(|(_, path)| path.exists())
    }

    /// Check and, if stale or missing, download `service`'s rule-sets.
    /// Returns `true` when at least one file was (re)downloaded.
    pub fn update_service_if_needed(&self, service: RoutedService) -> Result<bool> {
        let _guard = METADATA_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        self.update_assets_if_needed(&service_assets(service).present())
    }

    /// ETag-checked download of a set of assets. Every asset is attempted
    /// even when an earlier one fails (a transient error on one file must
    /// not starve its siblings), and ETags are persisted for whatever
    /// succeeded — otherwise a partial failure would force a spurious
    /// re-download of the successful file next time. The first error is
    /// still returned. Callers must hold [`METADATA_LOCK`].
    fn update_assets_if_needed(&self, assets: &[&GeoAsset]) -> Result<bool> {
        let mut meta = self.load_metadata().unwrap_or_default();
        let mut updated = false;
        let mut first_err: Option<anyhow::Error> = None;
        for asset in assets {
            let dest = self.geo_dir.join(asset.filename);
            let needed = if dest.exists() {
                match self.check_single(
                    asset.url,
                    meta.etags.get(asset.filename).map(String::as_str),
                ) {
                    Ok(n) => n,
                    Err(e) => {
                        if first_err.is_none() {
                            first_err = Some(e);
                        }
                        continue;
                    }
                }
            } else {
                true
            };
            if !needed {
                continue;
            }
            match self.download_file(asset.url, &dest) {
                Ok(etag) => {
                    if let Some(e) = etag {
                        meta.etags.insert(asset.filename.to_string(), e);
                    }
                    updated = true;
                }
                Err(e) => {
                    if first_err.is_none() {
                        first_err =
                            Some(e.context(format!("Failed to download {}", asset.filename)));
                    }
                }
            }
        }
        if updated {
            self.save_metadata(&meta)?;
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(updated),
        }
    }

    /// Return a human-readable string of the last update time for the given region, or None.
    pub fn last_updated(&self, region: GeoRegion) -> Option<String> {
        if matches!(region, GeoRegion::Global) {
            return None;
        }
        let meta = self.load_metadata().ok()?;
        meta.updated_at
            .get(&region)
            .map(|dt| dt.format("%d %b %H:%M").to_string())
    }

    /// Return the last successful update check for a region.
    pub fn last_checked_at(&self, region: GeoRegion) -> Option<DateTime<Local>> {
        if matches!(region, GeoRegion::Global) {
            return None;
        }
        self.load_metadata().ok()?.checked_at.get(&region).copied()
    }

    /// Check whether rule-sets have updates available for the given region.
    /// Returns `(geoip_has_update, geosite_has_update)`. For `Global`, both
    /// are `false`.
    pub fn check_update_available(&self, region: GeoRegion) -> Result<(bool, bool)> {
        let Some(assets) = region_assets(region) else {
            return Ok((false, false));
        };
        let meta = self.load_metadata().unwrap_or_default();
        let needs = |asset: &GeoAsset| -> Result<bool> {
            let local = self.geo_dir.join(asset.filename);
            if !local.exists() {
                return Ok(true);
            }
            self.check_single(
                asset.url,
                meta.etags.get(asset.filename).map(String::as_str),
            )
        };
        Ok((needs(&assets.geoip)?, needs(&assets.geosite)?))
    }

    /// Download rule-sets for the given region and update metadata atomically.
    /// `Global` is a no-op returning `Ok(false)`. Locked entry point kept for
    /// tests; production code reaches downloads via `update_if_needed`.
    #[cfg(test)]
    pub fn download_databases(&self, region: GeoRegion) -> Result<bool> {
        let _guard = METADATA_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        self.download_databases_inner(region)
    }

    /// Body of [`Self::download_databases`]; callers must hold
    /// [`METADATA_LOCK`]. Shares the asset orchestration with the service
    /// rule-sets, so a partial failure keeps the successful file's ETag
    /// instead of forcing a re-download next cycle; the region timestamps
    /// are only stamped on a fully successful pass.
    fn download_databases_inner(&self, region: GeoRegion) -> Result<bool> {
        let Some(assets) = region_assets(region) else {
            return Ok(false);
        };
        let updated = self.update_assets_if_needed(&[&assets.geoip, &assets.geosite])?;
        if updated {
            let mut meta = self.load_metadata().unwrap_or_default();
            let now = Local::now();
            meta.updated_at.insert(region, now);
            meta.checked_at.insert(region, now);
            self.save_metadata(&meta)?;
        }
        Ok(updated)
    }

    /// Full update flow: check then download if needed.
    /// Returns typed result describing what happened.
    pub fn update_if_needed(&self, region: GeoRegion) -> Result<GeoResult> {
        if matches!(region, GeoRegion::Global) {
            return Ok(GeoResult::UpToDate { checked_at: None });
        }
        let _guard = METADATA_LOCK.lock().unwrap_or_else(|p| p.into_inner());

        let (geoip_need, geosite_need) = self.check_update_available(region)?;

        if !geoip_need && !geosite_need {
            let checked_at = Local::now();
            let mut meta = self.load_metadata().unwrap_or_default();
            meta.checked_at.insert(region, checked_at);
            self.save_metadata(&meta)?;
            return Ok(GeoResult::UpToDate {
                checked_at: Some(checked_at),
            });
        }

        let updated = self.download_databases_inner(region)?;
        if updated {
            let mut parts = Vec::new();
            if geoip_need {
                parts.push(format!("geoip-{}", region.as_str()));
            }
            if geosite_need {
                parts.push(format!("geosite-{}", region.as_str()));
            }
            let last_updated = self.last_updated(region);
            Ok(GeoResult::Updated {
                parts,
                last_updated,
                checked_at: self.last_checked_at(region).unwrap_or_else(Local::now),
            })
        } else {
            Ok(GeoResult::UpToDate {
                checked_at: Some(Local::now()),
            })
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
        let raw: GeoMetadataRaw = serde_json::from_str(&text)
            .with_context(|| format!("Failed to parse {:?}", self.metadata_path))?;
        Ok(raw.into())
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
        crate::atomic_write::write(dest, data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regions that actually have rule-sets (all of `GeoRegion::ALL` except
    /// `Global`). Used to parametrize per-country tests.
    const RULE_REGIONS: [GeoRegion; 3] = [GeoRegion::Ru, GeoRegion::Cn, GeoRegion::Ir];

    #[test]
    fn region_assets_some_for_rule_regions_none_for_global() {
        assert!(region_assets(GeoRegion::Global).is_none());
        for region in RULE_REGIONS {
            let a = region_assets(region).expect("rule region has assets");
            assert!(a.geoip.filename.starts_with("geoip-"));
            assert!(a.geosite.filename.starts_with("geosite-"));
            assert!(a.geoip.url.contains(a.geoip.filename));
            assert!(a.geosite.url.contains(a.geosite.filename));
        }
    }

    #[test]
    fn geo_asset_tag_strips_srs_extension() {
        let a = region_assets(GeoRegion::Ru).unwrap();
        assert_eq!(a.geoip.tag(), "geoip-ru");
        assert_eq!(a.geosite.tag(), "geosite-category-ru");
    }

    #[test]
    fn service_assets_tags_and_urls() {
        let steam = service_assets(RoutedService::Steam);
        let tags: Vec<&str> = steam.present().iter().map(|a| a.tag()).collect();
        assert_eq!(tags, ["geosite-steam", "geoip-steam"]);
        // The ASN file's upstream basename differs from the local filename.
        assert!(steam.geoip.as_ref().unwrap().url.ends_with("AS32590.srs"));

        let telegram = service_assets(RoutedService::Telegram);
        let tags: Vec<&str> = telegram.present().iter().map(|a| a.tag()).collect();
        assert_eq!(tags, ["geosite-telegram", "geoip-telegram"]);

        for service in RoutedService::ALL {
            for asset in service_assets(service).present() {
                assert!(asset.url.starts_with("https://"), "{}", asset.url);
                assert!(asset.filename.ends_with(".srs"), "{}", asset.filename);
            }
        }
    }

    #[test]
    fn service_local_paths_and_presence() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let gm = GeoManager::new().unwrap();
        let paths = gm.service_local_paths(RoutedService::Steam);
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0].0, "geosite-steam");
        assert_eq!(paths[1].0, "geoip-steam");
        assert!(paths.iter().all(|(_, p)| p.starts_with(&gm.geo_dir)));

        assert!(!gm.has_service_databases(RoutedService::Steam));
        fs::write(&paths[0].1, b"x").unwrap();
        assert!(
            !gm.has_service_databases(RoutedService::Steam),
            "only geosite present"
        );
        fs::write(&paths[1].1, b"x").unwrap();
        assert!(gm.has_service_databases(RoutedService::Steam));
    }

    /// Minimal scripted HTTP/1.1 server for exercising the download paths
    /// without the network. `handler(method, path)` returns
    /// `(status, etag, body)`. Serves at most `max_requests` connections.
    fn spawn_stub_http(
        max_requests: usize,
        handler: impl Fn(&str, &str) -> (u16, Option<String>, Vec<u8>) + Send + 'static,
    ) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for _ in 0..max_requests {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut head = Vec::new();
                let mut buf = [0u8; 2048];
                loop {
                    let n = stream.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    head.extend_from_slice(&buf[..n]);
                    if head.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let text = String::from_utf8_lossy(&head);
                let mut parts = text.split_whitespace();
                let method = parts.next().unwrap_or("").to_string();
                let path = parts.next().unwrap_or("").to_string();
                let (status, etag, body) = handler(&method, &path);
                let mut resp = format!(
                    "HTTP/1.1 {status} Stub\r\nContent-Length: {}\r\nConnection: close\r\n",
                    body.len()
                );
                if let Some(e) = etag {
                    resp.push_str(&format!("ETag: {e}\r\n"));
                }
                resp.push_str("\r\n");
                let _ = stream.write_all(resp.as_bytes());
                if method != "HEAD" {
                    let _ = stream.write_all(&body);
                }
                let _ = stream.flush();
            }
        });
        format!("http://{addr}")
    }

    fn stub_assets(base: &str) -> Vec<GeoAsset> {
        let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
        vec![
            GeoAsset {
                filename: "geoip-stub.srs",
                url: leak(format!("{base}/geoip")),
            },
            GeoAsset {
                filename: "geosite-stub.srs",
                url: leak(format!("{base}/geosite")),
            },
        ]
    }

    #[test]
    fn update_assets_partial_failure_persists_successful_etag() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let gm = GeoManager::new().unwrap();
        let base = spawn_stub_http(4, |_, path| {
            if path == "/geoip" {
                (200, Some("etag-ok".to_string()), b"AAA".to_vec())
            } else {
                (500, None, Vec::new())
            }
        });
        let assets = stub_assets(&base);

        let refs: Vec<&GeoAsset> = assets.iter().collect();
        let err = gm.update_assets_if_needed(&refs).unwrap_err().to_string();
        assert!(err.contains("geosite-stub.srs"), "{err}");
        // The successful first download and its ETag must survive the
        // failure of the second, or the next attempt re-downloads it.
        assert!(gm.geo_dir.join("geoip-stub.srs").exists());
        assert!(!gm.geo_dir.join("geosite-stub.srs").exists());
        let meta = gm.load_metadata().unwrap();
        assert_eq!(
            meta.etags.get("geoip-stub.srs").map(String::as_str),
            Some("etag-ok")
        );
        assert!(!meta.etags.contains_key("geosite-stub.srs"));
    }

    #[test]
    fn update_assets_downloads_both_when_missing_then_skips_via_etag() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let gm = GeoManager::new().unwrap();
        let base = spawn_stub_http(8, |_, _| (200, Some("same".to_string()), b"DATA".to_vec()));
        let assets = stub_assets(&base);

        let refs: Vec<&GeoAsset> = assets.iter().collect();
        assert!(gm.update_assets_if_needed(&refs).unwrap());
        assert_eq!(
            fs::read(gm.geo_dir.join("geoip-stub.srs")).unwrap(),
            b"DATA"
        );
        assert_eq!(
            fs::read(gm.geo_dir.join("geosite-stub.srs")).unwrap(),
            b"DATA"
        );
        // Second run: files exist and ETags match — HEAD only, no downloads.
        assert!(!gm.update_assets_if_needed(&refs).unwrap());
    }

    #[test]
    fn update_assets_check_error_does_not_starve_missing_sibling() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let gm = GeoManager::new().unwrap();
        let base = spawn_stub_http(2, |_, _| (200, Some("e1".to_string()), b"NEW".to_vec()));
        let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
        // First asset: file present, but its HEAD check fails at transport
        // level (closed port). Second asset: file missing, live server.
        let assets = [
            GeoAsset {
                filename: "geoip-stub.srs",
                url: "http://127.0.0.1:1/dead",
            },
            GeoAsset {
                filename: "geosite-stub.srs",
                url: leak(format!("{base}/geosite")),
            },
        ];
        fs::write(gm.geo_dir.join("geoip-stub.srs"), b"OLD").unwrap();

        let refs: Vec<&GeoAsset> = assets.iter().collect();
        // The check error is surfaced…
        let err = gm.update_assets_if_needed(&refs).unwrap_err().to_string();
        assert!(err.contains("HEAD request failed"), "{err}");
        // …but the missing sibling was still fetched this same cycle.
        assert_eq!(
            fs::read(gm.geo_dir.join("geosite-stub.srs")).unwrap(),
            b"NEW"
        );
        let meta = gm.load_metadata().unwrap();
        assert_eq!(
            meta.etags.get("geosite-stub.srs").map(String::as_str),
            Some("e1")
        );
    }

    /// Integration test that hits the real network. Run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn test_download_service_srs_files() {
        let gm = GeoManager::new().unwrap();
        for service in RoutedService::ALL {
            for (_, path) in gm.service_local_paths(service) {
                let _ = fs::remove_file(path);
            }

            assert!(
                gm.update_service_if_needed(service).unwrap(),
                "expected download for {service:?}"
            );
            assert!(gm.has_service_databases(service));
            // Second run must be an ETag-checked no-op.
            assert!(!gm.update_service_if_needed(service).unwrap());
        }
    }

    #[test]
    fn local_paths_match_region_assets() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let gm = GeoManager::new().unwrap();
        assert!(gm.local_paths(GeoRegion::Global).is_none());
        for region in RULE_REGIONS {
            let (geoip, geosite) = gm.local_paths(region).unwrap();
            let a = region_assets(region).unwrap();
            assert_eq!(geoip.file_name().unwrap(), a.geoip.filename);
            assert_eq!(geosite.file_name().unwrap(), a.geosite.filename);
            assert!(geoip.starts_with(&gm.geo_dir));
            assert!(geosite.starts_with(&gm.geo_dir));
        }
    }

    #[test]
    fn metadata_roundtrip_via_map() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let gm = GeoManager::new().unwrap();
        let now = Local::now();
        let mut etags = HashMap::new();
        let mut updated_at = HashMap::new();
        for region in RULE_REGIONS {
            let a = region_assets(region).unwrap();
            etags.insert(
                a.geoip.filename.to_string(),
                format!("etag-{}-ip", region.as_str()),
            );
            etags.insert(
                a.geosite.filename.to_string(),
                format!("etag-{}-site", region.as_str()),
            );
            updated_at.insert(region, now);
        }
        let meta = GeoMetadata {
            etags,
            updated_at,
            checked_at: HashMap::new(),
        };
        gm.save_metadata(&meta).unwrap();
        let loaded = gm.load_metadata().unwrap();
        for region in RULE_REGIONS {
            let a = region_assets(region).unwrap();
            assert_eq!(
                loaded.etags.get(a.geoip.filename),
                Some(&format!("etag-{}-ip", region.as_str()))
            );
            assert_eq!(
                loaded.etags.get(a.geosite.filename),
                Some(&format!("etag-{}-site", region.as_str()))
            );
            assert!(loaded.updated_at.contains_key(&region));
        }
        assert!(loaded.checked_at.is_empty());
    }

    #[test]
    fn checked_at_roundtrips_per_region() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let gm = GeoManager::new().unwrap();
        let ru_checked = Local::now() - chrono::Duration::try_hours(2).unwrap();
        let cn_checked = Local::now();
        let mut meta = GeoMetadata::default();
        meta.checked_at.insert(GeoRegion::Ru, ru_checked);
        meta.checked_at.insert(GeoRegion::Cn, cn_checked);
        gm.save_metadata(&meta).unwrap();

        assert_eq!(gm.last_checked_at(GeoRegion::Ru), Some(ru_checked));
        assert_eq!(gm.last_checked_at(GeoRegion::Cn), Some(cn_checked));
        assert_eq!(gm.last_checked_at(GeoRegion::Ir), None);
        assert_eq!(gm.last_checked_at(GeoRegion::Global), None);
    }

    #[test]
    fn metadata_migrates_legacy_per_country_etag_fields() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let gm = GeoManager::new().unwrap();
        // Hand-crafted legacy shape (no `etags` map, just the named fields).
        let legacy = r#"{
            "geoip_ru_etag": "legacy-ru-ip",
            "geosite_ru_etag": "legacy-ru-site",
            "geoip_cn_etag": "legacy-cn-ip",
            "geoip_ir_etag": "legacy-ir-ip"
        }"#;
        fs::write(&gm.metadata_path, legacy).unwrap();
        let loaded = gm.load_metadata().unwrap();
        assert_eq!(
            loaded.etags.get("geoip-ru.srs").map(String::as_str),
            Some("legacy-ru-ip")
        );
        assert_eq!(
            loaded
                .etags
                .get("geosite-category-ru.srs")
                .map(String::as_str),
            Some("legacy-ru-site")
        );
        assert_eq!(
            loaded.etags.get("geoip-cn.srs").map(String::as_str),
            Some("legacy-cn-ip")
        );
        assert_eq!(
            loaded.etags.get("geoip-ir.srs").map(String::as_str),
            Some("legacy-ir-ip")
        );
        // Absent legacy fields stay absent.
        assert!(!loaded.etags.contains_key("geosite-cn.srs"));
        assert!(!loaded.etags.contains_key("geosite-category-ir.srs"));
    }

    #[test]
    fn metadata_map_wins_over_legacy_field_when_both_present() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let gm = GeoManager::new().unwrap();
        let mixed = r#"{
            "etags": { "geoip-ru.srs": "modern" },
            "geoip_ru_etag": "legacy"
        }"#;
        fs::write(&gm.metadata_path, mixed).unwrap();
        let loaded = gm.load_metadata().unwrap();
        assert_eq!(
            loaded.etags.get("geoip-ru.srs").map(String::as_str),
            Some("modern")
        );
    }

    #[test]
    fn load_metadata_missing_returns_default() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let gm = GeoManager::new().unwrap();
        let _ = fs::remove_file(&gm.metadata_path);
        let meta = gm.load_metadata().unwrap();
        assert!(meta.etags.is_empty());
        assert!(meta.updated_at.is_empty());
        assert!(meta.checked_at.is_empty());
    }

    #[test]
    fn has_databases_reflects_file_presence_per_region() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let gm = GeoManager::new().unwrap();
        assert!(gm.has_databases(GeoRegion::Global));
        for region in RULE_REGIONS {
            let (geoip, geosite) = gm.local_paths(region).unwrap();
            let _ = fs::remove_file(&geoip);
            let _ = fs::remove_file(&geosite);

            assert!(!gm.has_databases(region), "missing files: {:?}", region);
            fs::write(&geoip, b"x").unwrap();
            assert!(
                !gm.has_databases(region),
                "only geoip present: {:?}",
                region
            );
            fs::write(&geosite, b"x").unwrap();
            assert!(gm.has_databases(region), "both present: {:?}", region);

            let _ = fs::remove_file(&geoip);
            let _ = fs::remove_file(&geosite);
        }
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
        let temp = gm.geo_dir.join("geoip-ru.srs.tmp");
        assert!(!temp.exists());
        let _ = fs::remove_file(&dest);
    }

    #[test]
    fn last_updated_returns_none_for_global() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let gm = GeoManager::new().unwrap();
        assert!(gm.last_updated(GeoRegion::Global).is_none());
    }

    #[test]
    fn last_updated_returns_none_when_metadata_missing() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let gm = GeoManager::new().unwrap();
        let _ = fs::remove_file(&gm.metadata_path);
        for region in RULE_REGIONS {
            assert!(gm.last_updated(region).is_none());
        }
    }

    #[test]
    fn last_updated_returns_formatted_string_after_save() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let gm = GeoManager::new().unwrap();
        let mut updated_at = HashMap::new();
        let dt = Local::now();
        updated_at.insert(GeoRegion::Ru, dt);
        let meta = GeoMetadata {
            updated_at,
            ..GeoMetadata::default()
        };
        gm.save_metadata(&meta).unwrap();
        let formatted = gm.last_updated(GeoRegion::Ru).unwrap();
        assert_eq!(formatted.len(), 12);
        assert_eq!(formatted, dt.format("%d %b %H:%M").to_string());
        assert!(gm.last_updated(GeoRegion::Cn).is_none());
    }

    #[test]
    fn check_update_available_global_returns_no_updates() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let gm = GeoManager::new().unwrap();
        let (a, b) = gm.check_update_available(GeoRegion::Global).unwrap();
        assert!(!a);
        assert!(!b);
    }

    #[test]
    fn download_databases_global_is_noop() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let gm = GeoManager::new().unwrap();
        assert!(!gm.download_databases(GeoRegion::Global).unwrap());
        assert!(!gm.metadata_path.exists());
    }

    #[test]
    fn update_if_needed_global_is_up_to_date() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let gm = GeoManager::new().unwrap();
        let result = gm.update_if_needed(GeoRegion::Global).unwrap();
        assert!(matches!(result, GeoResult::UpToDate { .. }));
    }

    #[test]
    fn load_metadata_parse_error_is_propagated() {
        let _guard = crate::test_helpers::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
        let gm = GeoManager::new().unwrap();
        fs::write(&gm.metadata_path, b"not valid json {{").unwrap();
        let err = gm.load_metadata().unwrap_err().to_string();
        assert!(err.contains("Failed to parse"));
    }

    /// Integration test that hits the real network. Run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn test_download_srs_files() {
        let gm = GeoManager::new().unwrap();
        let (geoip_ru, geosite_ru) = gm.local_paths(GeoRegion::Ru).unwrap();
        let _ = fs::remove_file(&geoip_ru);
        let _ = fs::remove_file(&geosite_ru);

        let result = gm.download_databases(GeoRegion::Ru);
        assert!(result.is_ok(), "download failed: {:?}", result);
        assert!(result.unwrap(), "expected updated=true");

        assert!(geoip_ru.exists(), "geoip-ru.srs should exist");
        assert!(geosite_ru.exists(), "geosite-category-ru.srs should exist");

        let updated = gm.last_updated(GeoRegion::Ru);
        assert!(updated.is_some(), "last_updated should be set");

        let result = gm.update_if_needed(GeoRegion::Ru).unwrap();
        assert!(
            matches!(result, GeoResult::UpToDate { .. }),
            "unexpected result: {:?}",
            result
        );
    }
}
