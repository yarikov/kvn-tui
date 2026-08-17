use std::collections::HashMap;

use anyhow::Result;
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

mod dns;
mod share_link;

pub use dns::{DnsConfig, DnsRule, DnsServer, DnsStrategy};
pub use share_link::{SUPPORTED_SHARE_SCHEMES, encode_share_link, parse_share_link};

/// Supported VPN protocols.
///
/// This enum is the discriminant for [`ProtocolConfig`] and is also used as
/// a lightweight label for UI rendering. Per-protocol fields live on the
/// corresponding [`ProtocolConfig`] variant, not on this enum.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Vless,
    Vmess,
    Trojan,
    Shadowsocks,
    Hysteria2,
    Tuic,
    Shadowtls,
    Anytls,
    Socks,
    Http,
    Ssh,
}

impl Protocol {
    /// Lowercase identifier used in JSON serialization and internal dispatch.
    pub fn as_str(self) -> &'static str {
        match self {
            Protocol::Vless => "vless",
            Protocol::Vmess => "vmess",
            Protocol::Trojan => "trojan",
            Protocol::Shadowsocks => "shadowsocks",
            Protocol::Hysteria2 => "hysteria2",
            Protocol::Tuic => "tuic",
            Protocol::Shadowtls => "shadowtls",
            Protocol::Anytls => "anytls",
            Protocol::Socks => "socks",
            Protocol::Http => "http",
            Protocol::Ssh => "ssh",
        }
    }

    /// Short label for the UI protocol column (fits within 6 characters).
    pub fn ui_label(self) -> &'static str {
        match self {
            Protocol::Shadowsocks => "ss",
            Protocol::Hysteria2 => "hy2",
            Protocol::Shadowtls => "stls",
            other => other.as_str(),
        }
    }
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Selected geo region for rule-set downloads and routing mode availability.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum GeoRegion {
    Global,
    Ru,
    Cn,
    Ir,
}

impl GeoRegion {
    /// All regions in their canonical UI / cycle order. Single source of truth
    /// for region listings — adding a country here propagates to overlays,
    /// key navigation, and runner availability scans.
    pub const ALL: [GeoRegion; 4] = [
        GeoRegion::Ru,
        GeoRegion::Cn,
        GeoRegion::Ir,
        GeoRegion::Global,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            GeoRegion::Global => "global",
            GeoRegion::Ru => "ru",
            GeoRegion::Cn => "cn",
            GeoRegion::Ir => "ir",
        }
    }

    /// Uppercase two-letter code used in user-facing labels ("Bypass RU").
    pub fn code_upper(&self) -> &'static str {
        match self {
            GeoRegion::Global => "GLOBAL",
            GeoRegion::Ru => "RU",
            GeoRegion::Cn => "CN",
            GeoRegion::Ir => "IR",
        }
    }
}

/// Routing mode for geoip/geosite rules. Generic over the active geo region so
/// adding a country requires no new variants — `RoutingMode::Bypass(GeoRegion::Br)`
/// just works.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RoutingMode {
    #[default]
    Global,
    Bypass(GeoRegion),
    Only(GeoRegion),
}

impl RoutingMode {
    /// Return the list of routing modes available for the given geo region.
    pub fn available(region: Option<GeoRegion>) -> Vec<RoutingMode> {
        match region {
            Some(r) if !matches!(r, GeoRegion::Global) => vec![
                RoutingMode::Global,
                RoutingMode::Bypass(r),
                RoutingMode::Only(r),
            ],
            _ => vec![RoutingMode::Global],
        }
    }
}

impl std::fmt::Display for RoutingMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RoutingMode::Global => f.write_str("Global"),
            RoutingMode::Bypass(r) => write!(f, "Bypass {}", r.code_upper()),
            RoutingMode::Only(r) => write!(f, "Only {}", r.code_upper()),
        }
    }
}

// Custom (de)serialization preserves the legacy on-disk shape
// ("global", "bypass_ru", "only_cn", ...) so existing profiles.json files load.
impl Serialize for RoutingMode {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            RoutingMode::Global => s.serialize_str("global"),
            RoutingMode::Bypass(r) => s.serialize_str(&format!("bypass_{}", r.as_str())),
            RoutingMode::Only(r) => s.serialize_str(&format!("only_{}", r.as_str())),
        }
    }
}

impl<'de> Deserialize<'de> for RoutingMode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let s = String::deserialize(d)?;
        if s == "global" {
            return Ok(RoutingMode::Global);
        }
        let parse_region = |code: &str| -> Option<GeoRegion> {
            GeoRegion::ALL.iter().copied().find(|r| r.as_str() == code)
        };
        if let Some(code) = s.strip_prefix("bypass_") {
            return parse_region(code).map(RoutingMode::Bypass).ok_or_else(|| {
                D::Error::custom(format!("unknown region in routing mode `{}`", s))
            });
        }
        if let Some(code) = s.strip_prefix("only_") {
            return parse_region(code).map(RoutingMode::Only).ok_or_else(|| {
                D::Error::custom(format!("unknown region in routing mode `{}`", s))
            });
        }
        Err(D::Error::custom(format!("unknown routing mode `{}`", s)))
    }
}

/// REALITY security settings for XTLS Vision.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RealitySettings {
    #[serde(rename = "public_key")]
    pub public_key: String,
    #[serde(rename = "short_id")]
    pub short_id: String,
    #[serde(rename = "server_name")]
    pub server_name: String,
    #[serde(rename = "spider_x")]
    pub spider_x: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Security {
    #[default]
    None,
    Reality,
    Tls,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransportType {
    Grpc,
    Ws,
    Http,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum Flow {
    #[default]
    None,
    #[serde(rename = "xtls-rprx-vision")]
    XtlsRprxVision,
}

/// TLS Encrypted Client Hello (ECH) configuration.
///
/// Maps onto sing-box's `tls.ech` block. When `config` is empty, sing-box
/// fetches the `ECHConfigList` from DNS HTTPS RR for the target server.
/// Mutually exclusive with REALITY (validated by [`Config::validate`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct EchSettings {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub config: Vec<String>,
}

/// Shared TLS configuration for protocols that carry a TLS layer
/// (VMess, Trojan, ShadowTLS, AnyTLS, Hysteria2, TUIC).
///
/// VLESS keeps its TLS-related fields flat on [`VlessConfig`] for
/// backward compatibility with existing `profiles.json` files.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TlsCommon {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_name: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub insecure: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alpn: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utls_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reality: Option<RealitySettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ech: Option<EchSettings>,
}

impl TlsCommon {
    /// REALITY and ECH cannot be enabled simultaneously — REALITY uses its
    /// own SNI-cloaking mechanism that conflicts with ECH's `ECHConfigList`.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.reality.is_some() && self.ech.as_ref().is_some_and(|e| e.enabled) {
            anyhow::bail!("tls.reality and tls.ech are mutually exclusive");
        }
        Ok(())
    }
}

/// Transport layer configuration (ws / grpc / http / httpupgrade).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TransportConfig {
    #[serde(rename = "type")]
    pub kind: TransportType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub headers: HashMap<String, String>,
}

/// VMess encryption cipher. Sing-box 1.12 still accepts `auto`; we forbid
/// the legacy stream cipher `aes-128-cfb`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum VmessSecurity {
    #[default]
    Auto,
    None,
    Zero,
    #[serde(rename = "aes-128-gcm")]
    Aes128Gcm,
    #[serde(rename = "chacha20-poly1305")]
    Chacha20Poly1305,
}

impl VmessSecurity {
    #[allow(dead_code)] // consumed by per-protocol outbound builders landing in PR2
    pub fn as_str(self) -> &'static str {
        match self {
            VmessSecurity::Auto => "auto",
            VmessSecurity::None => "none",
            VmessSecurity::Zero => "zero",
            VmessSecurity::Aes128Gcm => "aes-128-gcm",
            VmessSecurity::Chacha20Poly1305 => "chacha20-poly1305",
        }
    }
}

/// Shadowsocks AEAD-2022 + AEAD ciphers supported by sing-box 1.12.
/// Legacy stream ciphers (e.g. `aes-128-cfb`) are intentionally excluded.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ShadowsocksCipher {
    #[default]
    #[serde(rename = "chacha20-ietf-poly1305")]
    Chacha20IetfPoly1305,
    #[serde(rename = "aes-128-gcm")]
    Aes128Gcm,
    #[serde(rename = "aes-256-gcm")]
    Aes256Gcm,
    #[serde(rename = "2022-blake3-aes-128-gcm")]
    Blake3Aes128Gcm,
    #[serde(rename = "2022-blake3-aes-256-gcm")]
    Blake3Aes256Gcm,
    #[serde(rename = "2022-blake3-chacha20-poly1305")]
    Blake3Chacha20Poly1305,
    None,
}

impl ShadowsocksCipher {
    #[allow(dead_code)] // consumed by per-protocol outbound builders landing in PR2
    pub fn as_str(self) -> &'static str {
        match self {
            ShadowsocksCipher::Chacha20IetfPoly1305 => "chacha20-ietf-poly1305",
            ShadowsocksCipher::Aes128Gcm => "aes-128-gcm",
            ShadowsocksCipher::Aes256Gcm => "aes-256-gcm",
            ShadowsocksCipher::Blake3Aes128Gcm => "2022-blake3-aes-128-gcm",
            ShadowsocksCipher::Blake3Aes256Gcm => "2022-blake3-aes-256-gcm",
            ShadowsocksCipher::Blake3Chacha20Poly1305 => "2022-blake3-chacha20-poly1305",
            ShadowsocksCipher::None => "none",
        }
    }
}

/// Hysteria2 obfuscation. Sing-box 1.12+ supports the `salamander` type
/// (legacy top-level `obfs_password` is rejected).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Hysteria2Obfs {
    #[serde(rename = "type")]
    pub kind: Hysteria2ObfsType,
    pub password: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Hysteria2ObfsType {
    #[default]
    Salamander,
}

/// TUIC v5 congestion control algorithm.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum TuicCongestion {
    #[default]
    Bbr,
    Cubic,
    NewReno,
}

impl TuicCongestion {
    #[allow(dead_code)] // consumed by per-protocol outbound builders landing in PR2
    pub fn as_str(self) -> &'static str {
        match self {
            TuicCongestion::Bbr => "bbr",
            TuicCongestion::Cubic => "cubic",
            TuicCongestion::NewReno => "new_reno",
        }
    }
}

/// TUIC v5 UDP relay mode.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum TuicUdpRelayMode {
    #[default]
    Native,
    Quic,
}

impl TuicUdpRelayMode {
    #[allow(dead_code)] // consumed by per-protocol outbound builders landing in PR2
    pub fn as_str(self) -> &'static str {
        match self {
            TuicUdpRelayMode::Native => "native",
            TuicUdpRelayMode::Quic => "quic",
        }
    }
}

/// ShadowTLS protocol version. v1/v2 are deprecated; v3 is the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShadowtlsVersion {
    V1,
    V2,
    #[default]
    V3,
}

impl ShadowtlsVersion {
    pub fn as_u8(self) -> u8 {
        match self {
            ShadowtlsVersion::V1 => 1,
            ShadowtlsVersion::V2 => 2,
            ShadowtlsVersion::V3 => 3,
        }
    }
}

impl Serialize for ShadowtlsVersion {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_u8(self.as_u8())
    }
}

impl<'de> Deserialize<'de> for ShadowtlsVersion {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let v = u8::deserialize(de)?;
        match v {
            1 => Ok(Self::V1),
            2 => Ok(Self::V2),
            3 => Ok(Self::V3),
            other => Err(serde::de::Error::custom(format!(
                "unknown ShadowTLS version {}",
                other
            ))),
        }
    }
}

/// SOCKS proxy version.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SocksVersion {
    #[serde(rename = "4")]
    V4,
    #[serde(rename = "4a")]
    V4a,
    #[default]
    #[serde(rename = "5")]
    V5,
}

/// VLESS-specific profile configuration.
///
/// TLS parameters live on the shared [`TlsCommon`] via `#[serde(flatten)]`,
/// so legacy `profiles.json` entries with top-level `reality`/`ech` keys
/// deserialize directly into `tls.reality` / `tls.ech` (same wire shape).
/// The pre-v2 top-level `fingerprint` key needs explicit migration into
/// `tls.utls_fingerprint`; see [`Config::migrate`] (`migrate_v1_to_v2`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct VlessConfig {
    pub uuid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<Flow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security: Option<Security>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport_type: Option<TransportType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport_service_name: Option<String>,
    /// Pre-v2 alias of `tls.utls_fingerprint`. Read from the legacy JSON
    /// key `"fingerprint"`, never written. `migrate_v1_to_v2` moves it
    /// into `tls.utls_fingerprint` and clears this field.
    #[serde(default, rename = "fingerprint", skip_serializing)]
    pub legacy_fingerprint: Option<String>,
    #[serde(default, flatten)]
    pub tls: TlsCommon,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct VmessConfig {
    pub uuid: String,
    #[serde(default)]
    pub alter_id: u32,
    #[serde(default)]
    pub security: VmessSecurity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global_padding: Option<bool>,
    #[serde(default, flatten)]
    pub tls: TlsCommon,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<TransportConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TrojanConfig {
    pub password: String,
    #[serde(default, flatten)]
    pub tls: TlsCommon,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<TransportConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct ShadowsocksConfig {
    pub method: ShadowsocksCipher,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Hysteria2Config {
    pub password: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub up_mbps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub down_mbps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub obfs: Option<Hysteria2Obfs>,
    #[serde(default, flatten)]
    pub tls: TlsCommon,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TuicConfig {
    pub uuid: String,
    pub password: String,
    #[serde(default)]
    pub congestion_control: TuicCongestion,
    #[serde(default)]
    pub udp_relay_mode: TuicUdpRelayMode,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub zero_rtt_handshake: bool,
    #[serde(default, flatten)]
    pub tls: TlsCommon,
}

/// ShadowTLS-wrapped Shadowsocks. The sing-box `shadowtls` outbound is
/// a TLS-camouflage wrapper that does not perform any traffic ciphering on
/// its own; an inner Shadowsocks outbound chained via `detour` carries the
/// actual data. We model both halves in one profile so the user supplies
/// the ShadowTLS password (v3) plus the inner SS method/password once.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ShadowtlsConfig {
    #[serde(default)]
    pub version: ShadowtlsVersion,
    /// ShadowTLS v3 client password. Unused for v1/v2.
    pub password: String,
    /// Inner Shadowsocks cipher used by the detour outbound.
    #[serde(default)]
    pub method: ShadowsocksCipher,
    /// Inner Shadowsocks password used by the detour outbound.
    #[serde(default)]
    pub ss_password: String,
    #[serde(default, flatten)]
    pub tls: TlsCommon,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AnytlsConfig {
    pub password: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_session_check_interval: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_session_timeout: Option<String>,
    #[serde(default, flatten)]
    pub tls: TlsCommon,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct SocksConfig {
    #[serde(default)]
    pub version: SocksVersion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct HttpConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default, flatten)]
    pub tls: TlsCommon,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct SshConfig {
    pub user: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private_key_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private_key_passphrase: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub host_key: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub host_key_algorithms: Vec<String>,
}

/// Protocol-specific profile configuration.
///
/// The `protocol` discriminant is serialized at the same JSON level as the
/// other [`Profile`] fields via `#[serde(flatten)]` (internally-tagged enum).
/// For VLESS this preserves the historic `profiles.json` shape exactly.
///
/// Structs that carry `#[serde(flatten)] tls: TlsCommon` (Vless/Vmess/Trojan/
/// Hysteria2/Tuic/Shadowtls/Anytls/Http) cannot use `#[serde(deny_unknown_fields)]`
/// — serde silently disables the check whenever `flatten` is present, since it
/// can no longer tell which fields "belong" to the parent versus the flattened
/// child. Typos inside those variants therefore still deserialize as `None`.
/// Structs without a flattened tls block do enforce `deny_unknown_fields`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "protocol", rename_all = "lowercase")]
pub enum ProtocolConfig {
    Vless(VlessConfig),
    Vmess(VmessConfig),
    Trojan(TrojanConfig),
    Shadowsocks(ShadowsocksConfig),
    Hysteria2(Hysteria2Config),
    Tuic(TuicConfig),
    Shadowtls(ShadowtlsConfig),
    Anytls(AnytlsConfig),
    Socks(SocksConfig),
    Http(HttpConfig),
    Ssh(SshConfig),
}

impl ProtocolConfig {
    pub fn protocol(&self) -> Protocol {
        match self {
            ProtocolConfig::Vless(_) => Protocol::Vless,
            ProtocolConfig::Vmess(_) => Protocol::Vmess,
            ProtocolConfig::Trojan(_) => Protocol::Trojan,
            ProtocolConfig::Shadowsocks(_) => Protocol::Shadowsocks,
            ProtocolConfig::Hysteria2(_) => Protocol::Hysteria2,
            ProtocolConfig::Tuic(_) => Protocol::Tuic,
            ProtocolConfig::Shadowtls(_) => Protocol::Shadowtls,
            ProtocolConfig::Anytls(_) => Protocol::Anytls,
            ProtocolConfig::Socks(_) => Protocol::Socks,
            ProtocolConfig::Http(_) => Protocol::Http,
            ProtocolConfig::Ssh(_) => Protocol::Ssh,
        }
    }

    fn tls_common(&self) -> Option<&TlsCommon> {
        match self {
            ProtocolConfig::Vmess(c) => Some(&c.tls),
            ProtocolConfig::Trojan(c) => Some(&c.tls),
            ProtocolConfig::Hysteria2(c) => Some(&c.tls),
            ProtocolConfig::Tuic(c) => Some(&c.tls),
            ProtocolConfig::Shadowtls(c) => Some(&c.tls),
            ProtocolConfig::Anytls(c) => Some(&c.tls),
            ProtocolConfig::Http(c) => Some(&c.tls),
            // VLESS keeps reality/ech flat on VlessConfig; no shared block.
            ProtocolConfig::Vless(_)
            | ProtocolConfig::Shadowsocks(_)
            | ProtocolConfig::Socks(_)
            | ProtocolConfig::Ssh(_) => None,
        }
    }

    fn validate(&self) -> anyhow::Result<()> {
        match self {
            ProtocolConfig::Vless(c) => {
                if c.uuid.trim().is_empty() {
                    anyhow::bail!("vless.uuid must not be empty");
                }
                c.tls
                    .validate()
                    .map_err(|e| anyhow::anyhow!("vless: {e}"))?;
            }
            ProtocolConfig::Vmess(c) => {
                if c.uuid.trim().is_empty() {
                    anyhow::bail!("vmess.uuid must not be empty");
                }
            }
            ProtocolConfig::Trojan(c) => {
                if c.password.is_empty() {
                    anyhow::bail!("trojan.password must not be empty");
                }
            }
            ProtocolConfig::Shadowsocks(c) => {
                if c.password.is_empty() {
                    anyhow::bail!("shadowsocks.password must not be empty");
                }
            }
            ProtocolConfig::Hysteria2(c) => {
                if c.password.is_empty() {
                    anyhow::bail!("hysteria2.password must not be empty");
                }
            }
            ProtocolConfig::Tuic(c) => {
                if c.uuid.trim().is_empty() {
                    anyhow::bail!("tuic.uuid must not be empty");
                }
                if c.password.is_empty() {
                    anyhow::bail!("tuic.password must not be empty");
                }
            }
            ProtocolConfig::Shadowtls(c) => {
                if c.version == ShadowtlsVersion::V3 && c.password.is_empty() {
                    anyhow::bail!("shadowtls.password must not be empty for v3");
                }
                if c.ss_password.is_empty() {
                    anyhow::bail!(
                        "shadowtls.ss_password must not be empty (inner Shadowsocks detour)"
                    );
                }
            }
            ProtocolConfig::Anytls(c) => {
                if c.password.is_empty() {
                    anyhow::bail!("anytls.password must not be empty");
                }
            }
            ProtocolConfig::Socks(_) | ProtocolConfig::Http(_) => {}
            ProtocolConfig::Ssh(c) => {
                if c.user.trim().is_empty() {
                    anyhow::bail!("ssh.user must not be empty");
                }
            }
        }
        if let Some(tls) = self.tls_common() {
            tls.validate()?;
        }
        Ok(())
    }
}

/// Single VPN profile. The `protocol` discriminant and protocol-specific
/// fields are flattened into [`ProtocolConfig`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Profile {
    #[serde(default = "Uuid::new_v4")]
    pub id: Uuid,
    pub name: String,
    pub address: String,
    pub port: u16,
    #[serde(flatten)]
    pub config: ProtocolConfig,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_id: Option<Uuid>,
}

impl Profile {
    /// Create a new VLESS profile with a generated UUID. Other protocols
    /// gain dedicated constructors as their share-link parsers land.
    pub fn new_vless(name: String, address: String, port: u16, uuid: String) -> Self {
        Self {
            id: Uuid::new_v4(),
            name,
            address,
            port,
            config: ProtocolConfig::Vless(VlessConfig {
                uuid,
                ..VlessConfig::default()
            }),
            tags: Vec::new(),
            subscription_id: None,
        }
    }

    /// Protocol discriminant.
    pub fn protocol(&self) -> Protocol {
        self.config.protocol()
    }

    /// Short label for the UI protocol column (≤6 chars).
    pub fn protocol_label(&self) -> &'static str {
        self.protocol().ui_label()
    }

    /// Deeper semantic validation on top of the per-field non-empty checks
    /// enforced by `Config::validate`. Verifies:
    /// - `port != 0`
    /// - `address` parses as an IPv4/IPv6 literal or a valid hostname
    /// - protocol UUIDs (VLESS/VMess/TUIC) parse as [`Uuid`]
    /// - `security=Reality` requires a populated `reality` block
    pub fn validate_semantic(&self) -> anyhow::Result<()> {
        if self.port == 0 {
            anyhow::bail!("port must not be 0");
        }
        validate_host(&self.address)?;
        match &self.config {
            ProtocolConfig::Vless(cfg) => {
                Uuid::parse_str(cfg.uuid.trim()).map_err(|e| {
                    anyhow::anyhow!("vless.uuid {:?} is not a valid UUID: {e}", cfg.uuid)
                })?;
                if cfg.security == Some(Security::Reality) && cfg.tls.reality.is_none() {
                    anyhow::bail!("vless.security=reality requires a `reality` block");
                }
            }
            ProtocolConfig::Vmess(cfg) => {
                Uuid::parse_str(cfg.uuid.trim()).map_err(|e| {
                    anyhow::anyhow!("vmess.uuid {:?} is not a valid UUID: {e}", cfg.uuid)
                })?;
            }
            ProtocolConfig::Tuic(cfg) => {
                Uuid::parse_str(cfg.uuid.trim()).map_err(|e| {
                    anyhow::anyhow!("tuic.uuid {:?} is not a valid UUID: {e}", cfg.uuid)
                })?;
            }
            ProtocolConfig::Trojan(_)
            | ProtocolConfig::Shadowsocks(_)
            | ProtocolConfig::Hysteria2(_)
            | ProtocolConfig::Shadowtls(_)
            | ProtocolConfig::Anytls(_)
            | ProtocolConfig::Socks(_)
            | ProtocolConfig::Http(_)
            | ProtocolConfig::Ssh(_) => {}
        }
        Ok(())
    }

    /// Stable key identifying the credentials behind this profile,
    /// used by the subscription importer to detect duplicates.
    pub fn dedup_key(&self) -> String {
        match &self.config {
            ProtocolConfig::Vless(c) => format!("vless:{}", c.uuid),
            ProtocolConfig::Vmess(c) => format!("vmess:{}", c.uuid),
            ProtocolConfig::Trojan(c) => {
                format!("trojan:{}@{}:{}", c.password, self.address, self.port)
            }
            ProtocolConfig::Shadowsocks(c) => {
                format!("ss:{}@{}:{}", c.password, self.address, self.port)
            }
            ProtocolConfig::Hysteria2(c) => {
                format!("hy2:{}@{}:{}", c.password, self.address, self.port)
            }
            ProtocolConfig::Tuic(c) => format!("tuic:{}", c.uuid),
            ProtocolConfig::Shadowtls(c) => {
                format!("shadowtls:{}@{}:{}", c.password, self.address, self.port)
            }
            ProtocolConfig::Anytls(c) => {
                format!("anytls:{}@{}:{}", c.password, self.address, self.port)
            }
            ProtocolConfig::Socks(c) => format!(
                "socks:{}@{}:{}",
                c.username.as_deref().unwrap_or(""),
                self.address,
                self.port
            ),
            ProtocolConfig::Http(c) => format!(
                "http:{}@{}:{}",
                c.username.as_deref().unwrap_or(""),
                self.address,
                self.port
            ),
            ProtocolConfig::Ssh(c) => format!("ssh:{}@{}:{}", c.user, self.address, self.port),
        }
    }
}

/// Auto-update schedule for a subscription.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionAutoUpdate {
    #[default]
    Off,
    Every1h,
    Every12h,
    Every1d,
    Every7d,
}

impl SubscriptionAutoUpdate {
    /// Return the interval in minutes.
    pub fn interval_minutes(self) -> u64 {
        match self {
            Self::Off => 0,
            Self::Every1h => 60,
            Self::Every12h => 720,
            Self::Every1d => 1440,
            Self::Every7d => 10080,
        }
    }

    /// Cycle to the next schedule.
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::Every1h,
            Self::Every1h => Self::Every12h,
            Self::Every12h => Self::Every1d,
            Self::Every1d => Self::Every7d,
            Self::Every7d => Self::Off,
        }
    }

    /// Short label for the schedule, e.g. "✕" or "🗘 1h".
    pub fn label(self) -> String {
        match self {
            Self::Off => "✕".to_string(),
            _ => format!("🗘 {}", self.interval_label()),
        }
    }

    /// Short interval label without icon.
    pub fn interval_label(self) -> String {
        match self {
            Self::Off => "off".to_string(),
            Self::Every1h => "1h".to_string(),
            Self::Every12h => "12h".to_string(),
            Self::Every1d => "1d".to_string(),
            Self::Every7d => "7d".to_string(),
        }
    }
}

/// A subscription URL that can be refreshed to import a set of profiles.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Subscription {
    #[serde(default = "Uuid::new_v4")]
    pub id: Uuid,
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub auto_update: SubscriptionAutoUpdate,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_updated: Option<DateTime<Local>>,
}

#[test]
fn subscription_auto_update_cycles_and_labels() {
    assert_eq!(SubscriptionAutoUpdate::Off.interval_minutes(), 0);
    assert_eq!(SubscriptionAutoUpdate::Every1h.interval_minutes(), 60);
    assert_eq!(SubscriptionAutoUpdate::Every12h.interval_minutes(), 720);
    assert_eq!(SubscriptionAutoUpdate::Every1d.interval_minutes(), 1440);
    assert_eq!(SubscriptionAutoUpdate::Every7d.interval_minutes(), 10080);

    assert_eq!(
        SubscriptionAutoUpdate::Off.next(),
        SubscriptionAutoUpdate::Every1h
    );
    assert_eq!(
        SubscriptionAutoUpdate::Every7d.next(),
        SubscriptionAutoUpdate::Off
    );

    assert_eq!(SubscriptionAutoUpdate::Off.label(), "✕");
    assert_eq!(SubscriptionAutoUpdate::Every1h.label(), "🗘 1h");
}

#[test]
fn geo_auto_update_cycles_intervals_and_labels() {
    let schedules = [
        (GeoAutoUpdate::Off, 0, "off"),
        (GeoAutoUpdate::Every12h, 720, "12h"),
        (GeoAutoUpdate::Every1d, 1_440, "1d"),
        (GeoAutoUpdate::Every3d, 4_320, "3d"),
        (GeoAutoUpdate::Every7d, 10_080, "7d"),
    ];
    for (schedule, minutes, label) in schedules {
        assert_eq!(schedule.interval_minutes(), minutes);
        assert_eq!(schedule.label(), label);
    }
    assert_eq!(GeoAutoUpdate::Off.next(), GeoAutoUpdate::Every12h);
    assert_eq!(GeoAutoUpdate::Every12h.next(), GeoAutoUpdate::Every1d);
    assert_eq!(GeoAutoUpdate::Every1d.next(), GeoAutoUpdate::Every3d);
    assert_eq!(GeoAutoUpdate::Every3d.next(), GeoAutoUpdate::Every7d);
    assert_eq!(GeoAutoUpdate::Every7d.next(), GeoAutoUpdate::Off);
}

/// Background update schedule for geo rule-sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GeoAutoUpdate {
    #[default]
    Off,
    #[serde(rename = "every_12h")]
    Every12h,
    #[serde(rename = "every_1d")]
    Every1d,
    #[serde(rename = "every_3d")]
    Every3d,
    #[serde(rename = "every_7d")]
    Every7d,
}

impl GeoAutoUpdate {
    pub fn interval_minutes(self) -> u64 {
        match self {
            Self::Off => 0,
            Self::Every12h => 720,
            Self::Every1d => 1_440,
            Self::Every3d => 4_320,
            Self::Every7d => 10_080,
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::Every12h,
            Self::Every12h => Self::Every1d,
            Self::Every1d => Self::Every3d,
            Self::Every3d => Self::Every7d,
            Self::Every7d => Self::Off,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Every12h => "12h",
            Self::Every1d => "1d",
            Self::Every3d => "3d",
            Self::Every7d => "7d",
        }
    }
}

/// Routing override for one well-known service, applied ahead of the
/// regional geo rules so it wins in every routing mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ServiceRoute {
    /// No override — the service follows the regional routing mode.
    #[default]
    Disabled,
    /// Force the service through the VPN tunnel, even under `Bypass`.
    Proxy,
    /// Send the service out the `direct` outbound (real network location),
    /// even under `Only`. Deliberately bypasses the tunnel — and the kill
    /// switch, which allowlists the `direct` outbound's fwmark by design.
    Direct,
}

impl ServiceRoute {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Disabled => "Disabled",
            Self::Proxy => "Proxy",
            Self::Direct => "Direct",
        }
    }

    /// Cycle order in the TUI overlay: Disabled → Proxy → Direct → Disabled.
    pub const fn next(self) -> Self {
        match self {
            Self::Disabled => Self::Proxy,
            Self::Proxy => Self::Direct,
            Self::Direct => Self::Disabled,
        }
    }

    pub const fn prev(self) -> Self {
        match self {
            Self::Disabled => Self::Direct,
            Self::Proxy => Self::Disabled,
            Self::Direct => Self::Proxy,
        }
    }
}

/// Services with predefined rule-sets that can be routed individually (see
/// [`ServiceRoute`]). Adding a service = a variant here, an `ALL` entry, and
/// a descriptor arm in `geo::service_assets`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutedService {
    Steam,
    Telegram,
}

impl RoutedService {
    /// Display / rule-generation order. Iterate this instead of the
    /// `service_routes` map — `HashMap` iteration order is nondeterministic,
    /// which would make the generated sing-box config unstable across runs.
    pub const ALL: [Self; 2] = [Self::Steam, Self::Telegram];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Steam => "Steam",
            Self::Telegram => "Telegram",
        }
    }
}

/// Geo-region and routing-mode preferences.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct GeoRouting {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_region: Option<GeoRegion>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub selected_region_modes: HashMap<GeoRegion, RoutingMode>,
    #[serde(default)]
    pub auto_update: GeoAutoUpdate,
    /// Per-service routing overrides. An absent entry means
    /// [`ServiceRoute::Disabled`] — overrides are strictly opt-in.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub service_routes: HashMap<RoutedService, ServiceRoute>,
}

impl GeoRouting {
    /// Return the active routing mode for the current region.
    /// Falls back to `Global` when no region is selected or no mode is stored.
    pub fn mode(&self) -> RoutingMode {
        self.current_region
            .and_then(|r| self.selected_region_modes.get(&r).copied())
            .unwrap_or(RoutingMode::Global)
    }

    /// Change the active geo region.
    pub fn set_region(&mut self, region: GeoRegion) {
        self.current_region = Some(region);
    }

    /// Store the routing mode for the current region.
    pub fn set_mode(&mut self, mode: RoutingMode) {
        if let Some(region) = self.current_region {
            self.selected_region_modes.insert(region, mode);
        }
    }

    /// Return the routing override for `service` (absent = `Disabled`).
    pub fn service_route(&self, service: RoutedService) -> ServiceRoute {
        self.service_routes
            .get(&service)
            .copied()
            .unwrap_or_default()
    }

    /// Services with an active (non-`Disabled`) routing override, in
    /// [`RoutedService::ALL`] order.
    pub fn enabled_services(&self) -> Vec<RoutedService> {
        RoutedService::ALL
            .into_iter()
            .filter(|s| self.service_route(*s) != ServiceRoute::Disabled)
            .collect()
    }

    /// Return routing modes available for the current region.
    pub fn available_modes(&self) -> Vec<RoutingMode> {
        RoutingMode::available(self.current_region)
    }
}

/// Application settings stored alongside profiles.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_profile: Option<Uuid>,
    #[serde(default = "default_tun_interface")]
    pub tun_interface: String,
    /// Legacy field, superseded by `dns.strategy`. Kept for one release so
    /// existing config files still load; on save we re-emit it from `dns.strategy`
    /// to avoid splitting the source of truth.
    #[serde(default = "default_dns_strategy")]
    pub dns_strategy: DnsStrategy,
    #[serde(default)]
    pub dns: DnsConfig,
    #[serde(default)]
    pub geo_routing: GeoRouting,
    #[serde(default)]
    pub auto_connect: bool,
    #[serde(default)]
    pub kill_switch: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_connected_profile: Option<Uuid>,
    /// Active UI theme slug. The literal `"omarchy"` is a sentinel that
    /// means "follow Omarchy's active XDG state/config theme.name"; any other value
    /// names a bundled palette (see `src/ui/palette.rs`).
    #[serde(default = "default_theme")]
    pub theme: String,
    /// Tracing filter applied at startup. Accepted values:
    /// `trace`, `debug`, `info`, `warn`, `error`. Anything else falls back
    /// to `info` at read time. The `RUST_LOG` env var, if set, wins over
    /// this field. Edited only via the JSON editor (`e` in the TUI).
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

/// Accept `address` if it parses as a bare IPv4/IPv6 literal or as a hostname.
/// sing-box wants the on-wire form (unbracketed for IPv6), so we try
/// [`IpAddr`](std::net::IpAddr) first and fall back to [`url::Host::parse`]
/// for domain names. Bracketed IPv6 (`[::1]`) is accepted via `Host::parse`.
fn validate_host(address: &str) -> anyhow::Result<()> {
    use std::net::IpAddr;
    if address.parse::<IpAddr>().is_ok() {
        return Ok(());
    }
    url::Host::parse(address)
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("address {:?} is not a valid IP or hostname: {e}", address))
}

fn default_tun_interface() -> String {
    "tun0".to_string()
}

fn default_dns_strategy() -> DnsStrategy {
    DnsStrategy::PreferIpv4
}

/// Default theme slug for fresh installs. Works on every distro because
/// `tokyo-night` is one of the bundled palettes; on Omarchy users can
/// switch to `"omarchy"` via the in-TUI picker to auto-follow the system.
pub fn default_theme() -> String {
    "tokyo-night".to_string()
}

pub fn default_log_level() -> String {
    "info".to_string()
}

/// Canonical log-level values accepted by both `tracing_subscriber::EnvFilter`
/// and sing-box's `log.level`. Also used as the allow-list by
/// [`Settings::validate`].
pub const LOG_LEVELS: &[&str] = &["trace", "debug", "info", "warn", "error"];

/// Linux IFNAMSIZ − 1 (the kernel reserves one byte for the terminator).
const MAX_TUN_INTERFACE_LEN: usize = 15;

/// Sentinel value in `settings.theme` that means "follow the active Omarchy
/// theme". Any other value must match one of the bundled palette slugs.
pub const OMARCHY_THEME_SENTINEL: &str = "omarchy";

// Names-only table generated by `build.rs` from `themes/*.toml`. Kept as a
// separate include (rather than referencing `ui::palette::BUNDLED`) because
// `ui` depends on `config`, so a dep in the other direction would be circular.
include!(concat!(env!("OUT_DIR"), "/bundled_theme_names.rs"));

/// Map a `settings.log_level` value to one of the five canonical levels
/// (`trace`/`debug`/`info`/`warn`/`error`). Anything else returns `"info"`.
/// Used both by the tracing filter in `main.rs` and by the sing-box config
/// generator, so the level the user sets in the JSON applies to both.
///
/// This fallback remains a runtime safety net for env-injected values; values
/// coming from `profiles.json` are additionally rejected by
/// [`Settings::validate`] before they reach this function.
pub fn normalized_log_level(level: &str) -> &'static str {
    match level {
        "trace" => "trace",
        "debug" => "debug",
        "info" => "info",
        "warn" => "warn",
        "error" => "error",
        _ => "info",
    }
}

fn is_safe_slug_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

impl Settings {
    /// Reject settings values that would either fail immediately at sing-box
    /// startup or fall back silently at runtime. Called from
    /// [`Config::validate`].
    pub fn validate(&self) -> anyhow::Result<()> {
        let tun = self.tun_interface.trim();
        if tun.is_empty() {
            anyhow::bail!("settings.tun_interface must not be empty");
        }
        if tun.len() > MAX_TUN_INTERFACE_LEN {
            anyhow::bail!(
                "settings.tun_interface {:?} exceeds Linux IFNAMSIZ limit ({} chars)",
                self.tun_interface,
                MAX_TUN_INTERFACE_LEN,
            );
        }
        if !tun.chars().all(is_safe_slug_char) {
            anyhow::bail!(
                "settings.tun_interface {:?} contains disallowed characters (allowed: a-z, A-Z, 0-9, `-`, `_`)",
                self.tun_interface,
            );
        }

        if self.theme != OMARCHY_THEME_SENTINEL
            && !BUNDLED_THEME_NAMES.contains(&self.theme.as_str())
        {
            anyhow::bail!(
                "settings.theme {:?} is not a bundled palette slug (expected {:?} or one of {} bundled themes)",
                self.theme,
                OMARCHY_THEME_SENTINEL,
                BUNDLED_THEME_NAMES.len(),
            );
        }

        if !LOG_LEVELS.contains(&self.log_level.as_str()) {
            anyhow::bail!(
                "settings.log_level {:?} is not one of {:?}",
                self.log_level,
                LOG_LEVELS,
            );
        }

        Ok(())
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            default_profile: None,
            tun_interface: default_tun_interface(),
            dns_strategy: default_dns_strategy(),
            dns: DnsConfig::default(),
            geo_routing: GeoRouting::default(),
            auto_connect: false,
            kill_switch: false,
            last_connected_profile: None,
            theme: default_theme(),
            log_level: default_log_level(),
        }
    }
}

/// Current schema version for `profiles.json`. Bumped on every breaking
/// change to the persisted shape; new migrations go in `Config::migrate`.
pub const CURRENT_SCHEMA_VERSION: u32 = 2;

fn default_schema_version() -> u32 {
    // Files written before the version was introduced are treated as v0
    // and run through the v0 → v1 migration on load.
    0
}

/// Root configuration file structure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Schema version of the persisted file. See [`CURRENT_SCHEMA_VERSION`].
    /// Absent in pre-versioned files; defaults to 0 in that case so the load
    /// path runs the v0 → v1 migration.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub profiles: Vec<Profile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subscriptions: Vec<Subscription>,
    #[serde(default)]
    pub settings: Settings,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            profiles: Vec::new(),
            subscriptions: Vec::new(),
            settings: Settings::default(),
        }
    }
}

impl Config {
    /// Resolve the selected profile index from `settings.default_profile`.
    /// Returns the index of the default profile if it exists, otherwise `0`.
    pub fn resolve_selected(&self) -> usize {
        self.settings
            .default_profile
            .and_then(|id| self.profiles.iter().position(|p| p.id == id))
            .unwrap_or(0)
    }

    /// Validate semantic constraints that serde cannot enforce.
    ///
    /// Checks:
    /// - Each profile has non-empty `name`, `address`, and `uuid`.
    /// - `settings.default_profile` references an existing profile if set.
    /// - DNS server tags are non-empty and unique; `dns.final_server` and every
    ///   `dns.rules[*].server` reference an existing tag; when `fakeip_enabled`
    ///   at least one server is of type `fakeip`.
    pub fn validate(&self) -> anyhow::Result<()> {
        for (idx, profile) in self.profiles.iter().enumerate() {
            let num = idx + 1;
            if profile.name.trim().is_empty() {
                anyhow::bail!("Profile {num}: name must not be empty");
            }
            if profile.address.trim().is_empty() {
                anyhow::bail!("Profile {num}: address must not be empty");
            }
            if let Err(e) = profile.config.validate() {
                anyhow::bail!("Profile {num}: {e}");
            }
            if let Err(e) = profile.validate_semantic() {
                anyhow::bail!("Profile {num}: {e}");
            }
        }

        if let Some(id) = self.settings.default_profile
            && !self.profiles.iter().any(|p| p.id == id)
        {
            anyhow::bail!("settings.default_profile ({id}) references a non-existent profile");
        }

        self.settings.validate()?;
        self.settings.dns.validate()?;

        Ok(())
    }

    /// Apply schema migrations needed to bring the loaded config up to
    /// [`CURRENT_SCHEMA_VERSION`]. Called by `load_config_at` after deserialise.
    /// Each migration step is idempotent.
    ///
    /// Files written by a newer kvn-tui version (higher `schema_version` than
    /// this build knows about) are rejected here — loading them would silently
    /// drop future-only fields; the user must upgrade the client instead.
    pub fn migrate(&mut self) -> anyhow::Result<()> {
        if self.schema_version > CURRENT_SCHEMA_VERSION {
            anyhow::bail!(
                "config schema_version {} is newer than this build supports (max {}); upgrade kvn-tui",
                self.schema_version,
                CURRENT_SCHEMA_VERSION,
            );
        }
        if self.schema_version == 0 {
            self.migrate_v0_to_v1();
            self.schema_version = 1;
        }
        if self.schema_version == 1 {
            self.migrate_v1_to_v2();
            self.schema_version = 2;
        }
        debug_assert_eq!(self.schema_version, CURRENT_SCHEMA_VERSION);
        Ok(())
    }

    /// v0 → v1: promote the legacy `Settings.dns_strategy` field into
    /// `Settings.dns.strategy`. Idempotent.
    fn migrate_v0_to_v1(&mut self) {
        if self.settings.dns.strategy == DnsStrategy::default()
            && self.settings.dns_strategy != DnsStrategy::default()
        {
            self.settings.dns.strategy = self.settings.dns_strategy.clone();
        }
        // Keep both fields in sync going forward; `dns.strategy` is the source.
        self.settings.dns_strategy = self.settings.dns.strategy.clone();
    }

    /// v1 → v2: promote the legacy top-level VLESS `fingerprint` field into
    /// `cfg.tls.utls_fingerprint`. The pre-v2 sibling fields `reality` and
    /// `ech` already deserialize straight into `cfg.tls.*` via
    /// `#[serde(flatten)]` (identical key names), so they need no code path
    /// here. Idempotent: `.take()` clears the legacy slot on the first run.
    fn migrate_v1_to_v2(&mut self) {
        for profile in &mut self.profiles {
            if let ProtocolConfig::Vless(cfg) = &mut profile.config
                && let Some(fp) = cfg.legacy_fingerprint.take()
                && cfg.tls.utls_fingerprint.is_none()
            {
                cfg.tls.utls_fingerprint = Some(fp);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_display() {
        assert_eq!(format!("{}", Protocol::Vless), "vless");
    }

    #[test]
    fn routing_mode_display() {
        assert_eq!(RoutingMode::Global.to_string(), "Global");
        assert_eq!(RoutingMode::Bypass(GeoRegion::Ru).to_string(), "Bypass RU");
        assert_eq!(RoutingMode::Only(GeoRegion::Ru).to_string(), "Only RU");
        assert_eq!(RoutingMode::Bypass(GeoRegion::Cn).to_string(), "Bypass CN");
        assert_eq!(RoutingMode::Only(GeoRegion::Cn).to_string(), "Only CN");
        assert_eq!(RoutingMode::Bypass(GeoRegion::Ir).to_string(), "Bypass IR");
        assert_eq!(RoutingMode::Only(GeoRegion::Ir).to_string(), "Only IR");
    }

    #[test]
    fn geo_region_serializes_to_global() {
        let json = serde_json::to_string(&GeoRegion::Global).unwrap();
        assert_eq!(json, r#""global""#);
    }

    #[test]
    fn routing_mode_serde_round_trip_matches_legacy_wire_format() {
        // Every variant maps to its on-disk string and back, preserving
        // backward compatibility with profiles.json files written by builds
        // that used the flat enum (`bypass_ru`, `only_cn`, etc.).
        let cases: &[(RoutingMode, &str)] = &[
            (RoutingMode::Global, r#""global""#),
            (RoutingMode::Bypass(GeoRegion::Ru), r#""bypass_ru""#),
            (RoutingMode::Only(GeoRegion::Ru), r#""only_ru""#),
            (RoutingMode::Bypass(GeoRegion::Cn), r#""bypass_cn""#),
            (RoutingMode::Only(GeoRegion::Cn), r#""only_cn""#),
            (RoutingMode::Bypass(GeoRegion::Ir), r#""bypass_ir""#),
            (RoutingMode::Only(GeoRegion::Ir), r#""only_ir""#),
        ];
        for (mode, wire) in cases {
            assert_eq!(
                serde_json::to_string(mode).unwrap(),
                *wire,
                "ser {:?}",
                mode
            );
            let parsed: RoutingMode = serde_json::from_str(wire).unwrap();
            assert_eq!(parsed, *mode, "de {}", wire);
        }
    }

    #[test]
    fn routing_mode_deserialize_rejects_unknown_strings() {
        assert!(serde_json::from_str::<RoutingMode>(r#""bogus""#).is_err());
        assert!(serde_json::from_str::<RoutingMode>(r#""bypass_xx""#).is_err());
        assert!(serde_json::from_str::<RoutingMode>(r#""only_""#).is_err());
    }

    #[test]
    fn geo_region_all_contains_every_variant() {
        // Smoke test: GeoRegion::ALL must enumerate all enum variants.
        // The match-statement below fails to compile if a variant is added
        // to the enum without being listed in ALL.
        for r in GeoRegion::ALL {
            match r {
                GeoRegion::Global | GeoRegion::Ru | GeoRegion::Cn | GeoRegion::Ir => {}
            }
        }
        assert_eq!(GeoRegion::ALL.len(), 4);
    }

    #[test]
    fn routing_mode_available() {
        assert_eq!(RoutingMode::available(None), vec![RoutingMode::Global]);
        for region in [GeoRegion::Ru, GeoRegion::Cn, GeoRegion::Ir] {
            assert_eq!(
                RoutingMode::available(Some(region)),
                vec![
                    RoutingMode::Global,
                    RoutingMode::Bypass(region),
                    RoutingMode::Only(region),
                ],
                "region {:?}",
                region,
            );
        }
        assert_eq!(
            RoutingMode::available(Some(GeoRegion::Global)),
            vec![RoutingMode::Global]
        );
    }

    fn vless_cfg(profile: &Profile) -> &VlessConfig {
        match &profile.config {
            ProtocolConfig::Vless(c) => c,
            other => panic!("expected VLESS, got {:?}", other.protocol()),
        }
    }

    #[test]
    fn profile_new_defaults() {
        let p = Profile::new_vless(
            "test".to_string(),
            "1.2.3.4".to_string(),
            443,
            "uuid-here".to_string(),
        );
        assert_eq!(p.name, "test");
        assert_eq!(p.protocol(), Protocol::Vless);
        assert_eq!(p.address, "1.2.3.4");
        assert_eq!(p.port, 443);
        let cfg = vless_cfg(&p);
        assert_eq!(cfg.uuid, "uuid-here");
        assert!(cfg.flow.is_none());
        assert!(cfg.security.is_none());
        assert!(cfg.tls.reality.is_none());
        assert!(cfg.transport_type.is_none());
        assert!(cfg.transport_service_name.is_none());
        assert!(cfg.tls.utls_fingerprint.is_none());
        assert!(cfg.tls.ech.is_none());
        assert!(cfg.legacy_fingerprint.is_none());
        assert!(p.tags.is_empty());
        assert_ne!(p.id, Uuid::nil());
    }

    #[test]
    fn settings_default() {
        let s = Settings::default();
        assert_eq!(s.tun_interface, "tun0");
        assert_eq!(s.dns_strategy, DnsStrategy::PreferIpv4);
        assert!(s.default_profile.is_none());
        assert!(!s.auto_connect);
        assert!(!s.kill_switch);
        assert!(s.last_connected_profile.is_none());
        assert!(s.geo_routing.current_region.is_none());
        assert!(s.geo_routing.selected_region_modes.is_empty());
        assert_eq!(s.geo_routing.auto_update, GeoAutoUpdate::Off);
        assert_eq!(s.geo_routing.mode(), RoutingMode::Global);
        assert_eq!(s.log_level, "info");
    }

    #[test]
    fn settings_log_level_round_trips_through_json() {
        let s = Settings {
            log_level: "debug".to_string(),
            ..Settings::default()
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"log_level\":\"debug\""));
        let restored: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.log_level, "debug");
    }

    #[test]
    fn normalized_log_level_passes_canonical_levels_through() {
        for level in ["trace", "debug", "info", "warn", "error"] {
            assert_eq!(normalized_log_level(level), level);
        }
    }

    #[test]
    fn normalized_log_level_falls_back_to_info_on_garbage() {
        for bad in ["verbose", "", "INFO", "fatal", "panic", "kvn_tui=debug"] {
            assert_eq!(normalized_log_level(bad), "info");
        }
    }

    #[test]
    fn settings_log_level_defaults_when_absent() {
        let json = r#"{
            "tun_interface": "tun0",
            "dns_strategy": "prefer_ipv4",
            "geo_routing": {},
            "auto_connect": false
        }"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.log_level, "info");
        assert_eq!(s.geo_routing.auto_update, GeoAutoUpdate::Off);
    }

    #[test]
    fn geo_auto_update_serde_values_round_trip() {
        for schedule in [
            GeoAutoUpdate::Off,
            GeoAutoUpdate::Every12h,
            GeoAutoUpdate::Every1d,
            GeoAutoUpdate::Every3d,
            GeoAutoUpdate::Every7d,
        ] {
            let json = serde_json::to_string(&schedule).unwrap();
            let restored: GeoAutoUpdate = serde_json::from_str(&json).unwrap();
            assert_eq!(restored, schedule);
        }
        assert_eq!(
            serde_json::to_string(&GeoAutoUpdate::Every3d).unwrap(),
            r#""every_3d""#
        );
    }

    #[test]
    fn geo_routing_mode_falls_back_to_global() {
        let g = GeoRouting::default();
        assert_eq!(g.mode(), RoutingMode::Global);
    }

    #[test]
    fn geo_routing_set_mode_persists_per_region() {
        let mut g = GeoRouting::default();
        g.set_region(GeoRegion::Ru);
        g.set_mode(RoutingMode::Bypass(GeoRegion::Ru));
        assert_eq!(g.mode(), RoutingMode::Bypass(GeoRegion::Ru));
        assert_eq!(
            g.selected_region_modes.get(&GeoRegion::Ru),
            Some(&RoutingMode::Bypass(GeoRegion::Ru))
        );

        g.set_region(GeoRegion::Cn);
        g.set_mode(RoutingMode::Only(GeoRegion::Cn));
        assert_eq!(g.mode(), RoutingMode::Only(GeoRegion::Cn));
        g.set_region(GeoRegion::Ru);
        assert_eq!(g.mode(), RoutingMode::Bypass(GeoRegion::Ru));
    }

    #[test]
    fn geo_routing_available_modes_uses_current_region() {
        let mut g = GeoRouting::default();
        assert_eq!(g.available_modes(), vec![RoutingMode::Global]);
        g.set_region(GeoRegion::Ru);
        assert_eq!(
            g.available_modes(),
            vec![
                RoutingMode::Global,
                RoutingMode::Bypass(GeoRegion::Ru),
                RoutingMode::Only(GeoRegion::Ru)
            ]
        );
    }

    #[test]
    fn config_default() {
        let c = Config::default();
        assert!(c.profiles.is_empty());
        assert_eq!(c.settings.tun_interface, "tun0");
    }

    #[test]
    fn settings_serde_roundtrip_with_kill_switch() {
        let s = Settings {
            kill_switch: true,
            ..Settings::default()
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"kill_switch\":true"));
        let restored: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(s, restored);
        assert!(restored.kill_switch);
    }

    #[test]
    fn settings_serde_kill_switch_defaults_when_absent() {
        // Older configs without the field should deserialize with kill_switch=false.
        let json = r#"{
            "tun_interface": "tun0",
            "dns_strategy": "prefer_ipv4",
            "geo_routing": {},
            "auto_connect": false
        }"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert!(!s.kill_switch);
    }

    #[test]
    fn service_routes_default_to_disabled() {
        // Opt-in: overriding a service's route must never happen without an
        // explicit user decision — absent map entries mean Disabled.
        let g = GeoRouting::default();
        assert!(g.service_routes.is_empty());
        for service in RoutedService::ALL {
            assert_eq!(g.service_route(service), ServiceRoute::Disabled);
        }
        assert!(g.enabled_services().is_empty());
    }

    #[test]
    fn service_routes_absent_in_json_deserialize_as_empty() {
        let json = r#"{
            "tun_interface": "tun0",
            "dns_strategy": "prefer_ipv4",
            "geo_routing": {},
            "auto_connect": false
        }"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert!(s.geo_routing.service_routes.is_empty());
        // Empty map is skipped on serialize — no noise in profiles.json.
        assert!(
            !serde_json::to_string(&s)
                .unwrap()
                .contains("service_routes")
        );
    }

    #[test]
    fn service_routes_round_trip_with_snake_case_keys() {
        let mut s = Settings::default();
        s.geo_routing
            .service_routes
            .insert(RoutedService::Steam, ServiceRoute::Direct);
        s.geo_routing
            .service_routes
            .insert(RoutedService::Telegram, ServiceRoute::Proxy);
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"steam\":\"direct\""), "{json}");
        assert!(json.contains("\"telegram\":\"proxy\""), "{json}");
        let restored: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(
            restored.geo_routing.service_route(RoutedService::Steam),
            ServiceRoute::Direct
        );
        assert_eq!(
            restored.geo_routing.service_route(RoutedService::Telegram),
            ServiceRoute::Proxy
        );
    }

    #[test]
    fn enabled_services_follow_all_order() {
        let mut g = GeoRouting::default();
        // Inserted in reverse of ALL order; a Disabled entry is excluded.
        g.service_routes
            .insert(RoutedService::Telegram, ServiceRoute::Proxy);
        g.service_routes
            .insert(RoutedService::Steam, ServiceRoute::Disabled);
        assert_eq!(g.enabled_services(), vec![RoutedService::Telegram]);
        g.service_routes
            .insert(RoutedService::Steam, ServiceRoute::Direct);
        assert_eq!(
            g.enabled_services(),
            vec![RoutedService::Steam, RoutedService::Telegram]
        );
    }

    #[test]
    fn service_route_cycle_covers_all_states() {
        let mut r = ServiceRoute::Disabled;
        let mut seen = Vec::new();
        for _ in 0..3 {
            r = r.next();
            seen.push(r);
        }
        assert_eq!(
            seen,
            vec![
                ServiceRoute::Proxy,
                ServiceRoute::Direct,
                ServiceRoute::Disabled
            ]
        );
        for route in seen {
            assert_eq!(route.next().prev(), route);
        }
    }

    #[test]
    fn config_serde_roundtrip() {
        let mut config = Config::default();
        let mut profile = Profile::new_vless(
            "Example".to_string(),
            "203.0.113.1".to_string(),
            443,
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
        );
        if let ProtocolConfig::Vless(ref mut cfg) = profile.config {
            cfg.security = Some(Security::Reality);
            cfg.tls.reality = Some(RealitySettings {
                public_key: "pk".to_string(),
                short_id: "sid".to_string(),
                server_name: "sni".to_string(),
                spider_x: "/".to_string(),
            });
        }
        profile.tags = vec!["tag1".to_string()];
        config.profiles.push(profile);
        config.settings.geo_routing.set_region(GeoRegion::Ru);
        config
            .settings
            .geo_routing
            .set_mode(RoutingMode::Bypass(GeoRegion::Ru));

        let json = serde_json::to_string(&config).unwrap();
        let restored: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(config, restored);
    }

    #[test]
    fn config_serde_roundtrip_with_geo_routing() {
        let mut config = Config::default();
        config
            .settings
            .geo_routing
            .selected_region_modes
            .insert(GeoRegion::Ru, RoutingMode::Bypass(GeoRegion::Ru));
        config
            .settings
            .geo_routing
            .selected_region_modes
            .insert(GeoRegion::Cn, RoutingMode::Only(GeoRegion::Cn));
        config.settings.geo_routing.current_region = Some(GeoRegion::Ru);

        let json = serde_json::to_string(&config).unwrap();
        let restored: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(config, restored);
        assert_eq!(
            restored
                .settings
                .geo_routing
                .selected_region_modes
                .get(&GeoRegion::Ru)
                .copied()
                .unwrap_or(RoutingMode::Global),
            RoutingMode::Bypass(GeoRegion::Ru)
        );
        assert_eq!(
            restored
                .settings
                .geo_routing
                .selected_region_modes
                .get(&GeoRegion::Cn)
                .copied()
                .unwrap_or(RoutingMode::Global),
            RoutingMode::Only(GeoRegion::Cn)
        );
    }

    #[test]
    fn profile_deserialize_missing_optionals() {
        let json = r#"{
            "id": "550e8400-e29b-41d4-a716-446655440000",
            "name": "Minimal",
            "protocol": "vless",
            "address": "1.1.1.1",
            "port": 443,
            "uuid": "uuid"
        }"#;
        let p: Profile = serde_json::from_str(json).unwrap();
        assert_eq!(p.name, "Minimal");
        let cfg = vless_cfg(&p);
        assert!(cfg.flow.is_none());
        assert!(cfg.tls.reality.is_none());
        assert!(p.tags.is_empty());
    }

    #[test]
    fn config_deserialize_missing_fields() {
        let json = r#"{}"#;
        let c: Config = serde_json::from_str(json).unwrap();
        assert!(c.profiles.is_empty());
        assert_eq!(c.settings.tun_interface, "tun0");
    }

    #[test]
    fn config_rejects_unknown_top_level_field() {
        let json = r#"{"unknown_field": 42}"#;
        let result: Result<Config, _> = serde_json::from_str(json);
        assert!(result.is_err(), "Should reject unknown top-level field");
    }

    #[test]
    fn config_validate_accepts_valid_config() {
        let mut config = Config::default();
        config.profiles.push(Profile::new_vless(
            "Valid".to_string(),
            "1.2.3.4".to_string(),
            443,
            crate::test_helpers::TEST_UUID.to_string(),
        ));
        config.settings.default_profile = Some(config.profiles[0].id);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn config_validate_rejects_empty_profile_name() {
        let mut config = Config::default();
        config.profiles.push(Profile::new_vless(
            "   ".to_string(),
            "1.2.3.4".to_string(),
            443,
            "uuid".to_string(),
        ));
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("name must not be empty"), "Error was: {}", err);
    }

    #[test]
    fn config_validate_rejects_empty_profile_address() {
        let mut config = Config::default();
        config.profiles.push(Profile::new_vless(
            "Name".to_string(),
            "".to_string(),
            443,
            "uuid".to_string(),
        ));
        let err = config.validate().unwrap_err().to_string();
        assert!(
            err.contains("address must not be empty"),
            "Error was: {}",
            err
        );
    }

    #[test]
    fn config_validate_rejects_empty_profile_uuid() {
        let mut config = Config::default();
        config.profiles.push(Profile::new_vless(
            "Name".to_string(),
            "1.2.3.4".to_string(),
            443,
            "  ".to_string(),
        ));
        let err = config.validate().unwrap_err().to_string();
        assert!(
            err.contains("vless.uuid must not be empty"),
            "Error was: {}",
            err
        );
    }

    #[test]
    fn config_validate_rejects_reality_plus_ech() {
        let mut config = Config::default();
        let mut profile = Profile::new_vless(
            "RealityEch".to_string(),
            "1.2.3.4".to_string(),
            443,
            "uuid".to_string(),
        );
        if let ProtocolConfig::Vless(ref mut cfg) = profile.config {
            cfg.tls.reality = Some(RealitySettings::default());
            cfg.tls.ech = Some(EchSettings {
                enabled: true,
                config: Vec::new(),
            });
        }
        config.profiles.push(profile);
        let err = config.validate().unwrap_err().to_string();
        assert!(
            err.contains("tls.reality and tls.ech are mutually exclusive"),
            "Error was: {}",
            err
        );
    }

    #[test]
    fn validate_semantic_rejects_port_zero() {
        let mut p = Profile::new_vless(
            "P".to_string(),
            "1.2.3.4".to_string(),
            0,
            crate::test_helpers::TEST_UUID.to_string(),
        );
        p.port = 0;
        let err = p.validate_semantic().unwrap_err().to_string();
        assert!(err.contains("port"), "Error was: {}", err);
    }

    #[test]
    fn validate_semantic_rejects_garbage_uuid() {
        let p = Profile::new_vless(
            "P".to_string(),
            "1.2.3.4".to_string(),
            443,
            "not-a-uuid".to_string(),
        );
        let err = p.validate_semantic().unwrap_err().to_string();
        assert!(err.contains("vless.uuid"), "Error was: {}", err);
    }

    #[test]
    fn validate_semantic_accepts_ipv6_literal() {
        let p = Profile::new_vless(
            "P".to_string(),
            "2001:db8::1".to_string(),
            443,
            crate::test_helpers::TEST_UUID.to_string(),
        );
        p.validate_semantic().unwrap();
    }

    #[test]
    fn validate_semantic_accepts_hostname() {
        let p = Profile::new_vless(
            "P".to_string(),
            "vpn.example.com".to_string(),
            443,
            crate::test_helpers::TEST_UUID.to_string(),
        );
        p.validate_semantic().unwrap();
    }

    #[test]
    fn validate_semantic_rejects_address_with_spaces() {
        let p = Profile::new_vless(
            "P".to_string(),
            "bad host name".to_string(),
            443,
            crate::test_helpers::TEST_UUID.to_string(),
        );
        assert!(p.validate_semantic().is_err());
    }

    #[test]
    fn validate_semantic_rejects_reality_without_block() {
        let mut p = Profile::new_vless(
            "P".to_string(),
            "1.2.3.4".to_string(),
            443,
            crate::test_helpers::TEST_UUID.to_string(),
        );
        if let ProtocolConfig::Vless(ref mut cfg) = p.config {
            cfg.security = Some(Security::Reality);
            cfg.tls.reality = None;
        }
        let err = p.validate_semantic().unwrap_err().to_string();
        assert!(
            err.contains("reality") && err.contains("block"),
            "Error was: {}",
            err
        );
    }

    #[test]
    fn validate_semantic_accepts_reality_with_block() {
        let mut p = Profile::new_vless(
            "P".to_string(),
            "1.2.3.4".to_string(),
            443,
            crate::test_helpers::TEST_UUID.to_string(),
        );
        if let ProtocolConfig::Vless(ref mut cfg) = p.config {
            cfg.security = Some(Security::Reality);
            cfg.tls.reality = Some(RealitySettings::default());
        }
        p.validate_semantic().unwrap();
    }

    #[test]
    fn save_config_at_rejects_invalid_config() {
        // Fail-close: save must run Config::validate first so a corrupted
        // in-memory state cannot overwrite a good profiles.json on disk.
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut config = Config::default();
        config.profiles.push(Profile::new_vless(
            "Broken".to_string(),
            "1.2.3.4".to_string(),
            443,
            "not-a-uuid".to_string(),
        ));
        let err = crate::config::save_config_at(file.path(), &config).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("Refusing to save"), "Error was: {}", msg);
    }

    // ---- Settings::validate ----

    #[test]
    fn settings_validate_default_ok() {
        Settings::default().validate().unwrap();
    }

    #[test]
    fn settings_validate_rejects_empty_tun_interface() {
        let s = Settings {
            tun_interface: "   ".into(),
            ..Settings::default()
        };
        let err = s.validate().unwrap_err().to_string();
        assert!(err.contains("tun_interface"), "Error was: {}", err);
    }

    #[test]
    fn settings_validate_rejects_overlong_tun_interface() {
        // IFNAMSIZ − 1 = 15; 16 chars must be rejected.
        let s = Settings {
            tun_interface: "a".repeat(16),
            ..Settings::default()
        };
        let err = s.validate().unwrap_err().to_string();
        assert!(err.contains("IFNAMSIZ"), "Error was: {}", err);
    }

    #[test]
    fn settings_validate_rejects_tun_interface_with_bad_chars() {
        let s = Settings {
            tun_interface: "tun 0".into(),
            ..Settings::default()
        };
        let err = s.validate().unwrap_err().to_string();
        assert!(err.contains("disallowed"), "Error was: {}", err);
    }

    #[test]
    fn settings_validate_accepts_omarchy_sentinel() {
        let s = Settings {
            theme: OMARCHY_THEME_SENTINEL.into(),
            ..Settings::default()
        };
        s.validate().unwrap();
    }

    #[test]
    fn settings_validate_accepts_bundled_theme() {
        // "tokyo-night" ships in themes/, so it must be in BUNDLED_THEME_NAMES.
        let s = Settings {
            theme: "tokyo-night".into(),
            ..Settings::default()
        };
        s.validate().unwrap();
    }

    #[test]
    fn settings_validate_rejects_unknown_theme() {
        let s = Settings {
            theme: "not-a-bundled-slug".into(),
            ..Settings::default()
        };
        let err = s.validate().unwrap_err().to_string();
        assert!(err.contains("theme"), "Error was: {}", err);
    }

    #[test]
    fn settings_validate_rejects_empty_theme() {
        let s = Settings {
            theme: String::new(),
            ..Settings::default()
        };
        assert!(s.validate().is_err());
    }

    #[test]
    fn settings_validate_accepts_every_canonical_log_level() {
        for level in LOG_LEVELS {
            let s = Settings {
                log_level: (*level).to_string(),
                ..Settings::default()
            };
            s.validate().unwrap();
        }
    }

    #[test]
    fn settings_validate_rejects_unknown_log_level() {
        let s = Settings {
            log_level: "verbose".into(),
            ..Settings::default()
        };
        let err = s.validate().unwrap_err().to_string();
        assert!(err.contains("log_level"), "Error was: {}", err);
    }

    #[test]
    fn settings_validate_rejects_uppercased_log_level() {
        // normalized_log_level lowercases at runtime, but on-disk config is
        // validated case-sensitively so the JSON does not silently drift.
        let s = Settings {
            log_level: "INFO".into(),
            ..Settings::default()
        };
        assert!(s.validate().is_err());
    }

    // ---- migrate: reject configs from the future ----

    #[test]
    fn migrate_rejects_schema_version_from_the_future() {
        let mut cfg = Config {
            schema_version: CURRENT_SCHEMA_VERSION + 1,
            ..Config::default()
        };
        let err = cfg.migrate().unwrap_err().to_string();
        assert!(err.contains("newer than this build"), "Error was: {}", err);
        assert!(err.contains("upgrade kvn-tui"), "Error was: {}", err);
    }

    #[test]
    fn migrate_accepts_current_schema_version() {
        let mut cfg = Config::default();
        cfg.migrate().unwrap();
        assert_eq!(cfg.schema_version, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn config_rejects_unknown_field_in_shadowsocks_config() {
        // deny_unknown_fields on ShadowsocksConfig catches typos in the
        // per-protocol block. Verifies point 2 of the validation hardening.
        let json = r#"{
            "profiles": [{
                "name": "SS",
                "protocol": "shadowsocks",
                "address": "1.2.3.4",
                "port": 8388,
                "method": "aes-256-gcm",
                "password": "pw",
                "bogus": "typo"
            }]
        }"#;
        let result: Result<Config, _> = serde_json::from_str(json);
        assert!(result.is_err(), "Expected deny_unknown_fields to reject");
    }

    #[test]
    fn config_validate_rejects_dangling_default_profile() {
        let mut config = Config::default();
        config.settings.default_profile = Some(Uuid::new_v4());
        let err = config.validate().unwrap_err().to_string();
        assert!(
            err.contains("references a non-existent profile"),
            "Error was: {}",
            err
        );
    }

    #[test]
    fn parse_long_vless_uri() {
        let uri = r#"vless://671c62c7-6768-4b98-ac6b-572c9c707be0@203.0.113.42:59431?type=grpc&encryption=none&serviceName=&authority=&security=reality&pbk=0IO3LodsrMnhOWh4ogwgdVqYg30CS5-snhFMwldOuAQ&fp=chrome&sni=google.com&sid=f04debc34cbc48a4&spx=%2F#Example-2873vb06"#;
        let profile = parse_share_link(uri).unwrap();
        assert_eq!(profile.protocol(), Protocol::Vless);
        assert_eq!(profile.address, "203.0.113.42");
        assert_eq!(profile.port, 59431);
        assert_eq!(profile.name, "Example-2873vb06");
        let cfg = vless_cfg(&profile);
        assert_eq!(cfg.uuid, "671c62c7-6768-4b98-ac6b-572c9c707be0");
        assert!(cfg.security.is_some());
        let reality = cfg.tls.reality.as_ref().unwrap();
        assert_eq!(
            reality.public_key,
            "0IO3LodsrMnhOWh4ogwgdVqYg30CS5-snhFMwldOuAQ"
        );
        assert_eq!(reality.server_name, "google.com");
        assert_eq!(reality.short_id, "f04debc34cbc48a4");
        assert_eq!(reality.spider_x, "/");
        assert_eq!(cfg.tls.utls_fingerprint.as_deref(), Some("chrome"));
    }

    #[test]
    fn parse_vless_minimal() {
        let uri = "vless://uuid@1.2.3.4:443#Name";
        let profile = parse_share_link(uri).unwrap();
        assert_eq!(profile.address, "1.2.3.4");
        assert_eq!(profile.port, 443);
        assert_eq!(profile.name, "Name");
        let cfg = vless_cfg(&profile);
        assert_eq!(cfg.uuid, "uuid");
        assert!(cfg.tls.reality.is_none());
        assert!(cfg.flow.is_none());
        assert!(cfg.tls.utls_fingerprint.is_none());
        assert!(cfg.transport_type.is_none());
    }

    #[test]
    fn parse_vless_default_port() {
        let uri = "vless://uuid@example.com#Test";
        let profile = parse_share_link(uri).unwrap();
        assert_eq!(profile.port, 443);
        assert_eq!(profile.address, "example.com");
    }

    #[test]
    fn parse_vless_partial_reality() {
        let uri = "vless://uuid@1.2.3.4:8443?security=reality&pbk=pk123&sni=sni.test#Partial";
        let profile = parse_share_link(uri).unwrap();
        let cfg = vless_cfg(&profile);
        assert_eq!(cfg.security, Some(Security::Reality));
        let reality = cfg.tls.reality.as_ref().unwrap();
        assert_eq!(reality.public_key, "pk123");
        assert_eq!(reality.server_name, "sni.test");
        assert!(reality.short_id.is_empty());
        assert!(reality.spider_x.is_empty());
    }

    #[test]
    fn parse_vless_url_encoded_spx() {
        let uri = "vless://uuid@1.2.3.4?pbk=k&spx=%2Fpath%2Fhere#N";
        let profile = parse_share_link(uri).unwrap();
        let cfg = vless_cfg(&profile);
        assert_eq!(cfg.tls.reality.as_ref().unwrap().spider_x, "/path/here");
    }

    #[test]
    fn legacy_vless_json_deserializes_into_new_shape() {
        // A pre-v2 profiles.json: top-level `reality` / `ech` / `fingerprint`
        // alongside the new flat layout. `reality` and `ech` are picked up
        // by `#[serde(flatten)] tls: TlsCommon` automatically; `fingerprint`
        // is captured into `legacy_fingerprint` and promoted by migrate().
        let json = r#"{
            "schema_version": 1,
            "profiles": [{
                "id": "550e8400-e29b-41d4-a716-446655440000",
                "name": "Legacy",
                "protocol": "vless",
                "address": "1.1.1.1",
                "port": 443,
                "uuid": "legacy-uuid",
                "flow": "xtls-rprx-vision",
                "security": "reality",
                "reality": {
                    "public_key": "pk",
                    "short_id": "sid",
                    "server_name": "sni",
                    "spider_x": "/"
                },
                "ech": { "enabled": false },
                "transport_type": "grpc",
                "transport_service_name": "svc",
                "fingerprint": "chrome",
                "tags": ["legacy"]
            }]
        }"#;
        let mut config: Config = serde_json::from_str(json).unwrap();
        // Before migrate(): legacy_fingerprint holds the raw value, the new
        // slot is still empty.
        {
            let cfg = vless_cfg(&config.profiles[0]);
            assert_eq!(cfg.legacy_fingerprint.as_deref(), Some("chrome"));
            assert!(cfg.tls.utls_fingerprint.is_none());
            assert!(cfg.tls.reality.is_some(), "reality flattened through");
            assert!(cfg.tls.ech.is_some(), "ech flattened through");
        }
        config.migrate().unwrap();
        assert_eq!(config.schema_version, CURRENT_SCHEMA_VERSION);
        let p = &config.profiles[0];
        let cfg = vless_cfg(p);
        assert_eq!(cfg.uuid, "legacy-uuid");
        assert_eq!(cfg.flow, Some(Flow::XtlsRprxVision));
        assert_eq!(cfg.security, Some(Security::Reality));
        assert!(cfg.tls.reality.is_some());
        assert_eq!(cfg.transport_type, Some(TransportType::Grpc));
        assert_eq!(cfg.transport_service_name.as_deref(), Some("svc"));
        assert_eq!(cfg.tls.utls_fingerprint.as_deref(), Some("chrome"));
        assert!(
            cfg.legacy_fingerprint.is_none(),
            "migration must clear the legacy slot"
        );
        assert_eq!(p.tags, vec!["legacy".to_string()]);
    }

    #[test]
    fn vmess_profile_roundtrip() {
        let profile = Profile {
            id: Uuid::nil(),
            name: "VMess".to_string(),
            address: "1.1.1.1".to_string(),
            port: 443,
            config: ProtocolConfig::Vmess(VmessConfig {
                uuid: "vm-uuid".to_string(),
                alter_id: 0,
                security: VmessSecurity::Aes128Gcm,
                global_padding: None,
                tls: TlsCommon {
                    server_name: Some("sni".to_string()),
                    ech: Some(EchSettings {
                        enabled: true,
                        config: Vec::new(),
                    }),
                    ..TlsCommon::default()
                },
                transport: None,
            }),
            tags: Vec::new(),
            subscription_id: None,
        };
        let json = serde_json::to_string(&profile).unwrap();
        assert!(json.contains("\"protocol\":\"vmess\""));
        assert!(json.contains("\"ech\""));
        let restored: Profile = serde_json::from_str(&json).unwrap();
        assert_eq!(profile, restored);
    }

    #[test]
    fn dedup_key_distinguishes_protocols() {
        let v = Profile::new_vless("V".into(), "1.1.1.1".into(), 443, "shared-uuid".to_string());
        let m = Profile {
            id: Uuid::nil(),
            name: "M".into(),
            address: "1.1.1.1".into(),
            port: 443,
            config: ProtocolConfig::Vmess(VmessConfig {
                uuid: "shared-uuid".to_string(),
                ..VmessConfig::default()
            }),
            tags: Vec::new(),
            subscription_id: None,
        };
        assert_ne!(
            v.dedup_key(),
            m.dedup_key(),
            "same UUID on different protocols must dedup separately"
        );
    }

    #[test]
    fn parse_rejects_unknown_scheme() {
        let result = parse_share_link("snake-oil://whatever");
        assert!(result.is_err());
    }

    #[test]
    fn parse_vless_missing_host_fails() {
        let result = parse_share_link("vless://");
        assert!(result.is_err());
    }

    // ---- VMess ----

    #[test]
    fn parse_vmess_uri_form() {
        let uri = "vmess://vm-uuid@1.1.1.1:443?security=tls&type=ws&path=/ws&host=host.example&sni=sni.example#VMess-1";
        let p = parse_share_link(uri).unwrap();
        assert_eq!(p.protocol(), Protocol::Vmess);
        assert_eq!(p.address, "1.1.1.1");
        assert_eq!(p.port, 443);
        assert_eq!(p.name, "VMess-1");
        let ProtocolConfig::Vmess(cfg) = &p.config else {
            panic!("ProtocolConfig variant mismatch")
        };
        assert_eq!(cfg.uuid, "vm-uuid");
        assert_eq!(cfg.tls.server_name.as_deref(), Some("sni.example"));
        let t = cfg.transport.as_ref().unwrap();
        assert_eq!(t.kind, TransportType::Ws);
        assert_eq!(t.path.as_deref(), Some("/ws"));
        assert_eq!(t.host.as_deref(), Some("host.example"));
    }

    #[test]
    fn parse_vmess_b64_json_form() {
        use base64::Engine;
        let body = serde_json::json!({
            "v": "2", "ps": "VMessB64", "add": "1.2.3.4", "port": "10086",
            "id": "vm-id", "aid": "0", "scy": "aes-128-gcm",
            "net": "ws", "type": "none", "host": "h.example", "path": "/wp",
            "tls": "tls", "sni": "sni.example", "alpn": "h2,http/1.1", "fp": "chrome",
        });
        let encoded =
            base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&body).unwrap());
        let p = parse_share_link(&format!("vmess://{}", encoded)).unwrap();
        assert_eq!(p.name, "VMessB64");
        assert_eq!(p.address, "1.2.3.4");
        assert_eq!(p.port, 10086);
        let ProtocolConfig::Vmess(cfg) = &p.config else {
            panic!("ProtocolConfig variant mismatch")
        };
        assert_eq!(cfg.uuid, "vm-id");
        assert_eq!(cfg.security, VmessSecurity::Aes128Gcm);
        assert_eq!(cfg.tls.server_name.as_deref(), Some("sni.example"));
        assert_eq!(cfg.tls.alpn, vec!["h2".to_string(), "http/1.1".to_string()]);
        assert_eq!(cfg.tls.utls_fingerprint.as_deref(), Some("chrome"));
        let t = cfg.transport.as_ref().unwrap();
        assert_eq!(t.kind, TransportType::Ws);
        assert_eq!(t.path.as_deref(), Some("/wp"));
    }

    // ---- Trojan ----

    #[test]
    fn parse_trojan_basic() {
        let uri = "trojan://secret@trojan.example:443?sni=sni.example&type=ws&path=/p#Trojan-1";
        let p = parse_share_link(uri).unwrap();
        assert_eq!(p.protocol(), Protocol::Trojan);
        let ProtocolConfig::Trojan(cfg) = &p.config else {
            panic!("ProtocolConfig variant mismatch")
        };
        assert_eq!(cfg.password, "secret");
        assert_eq!(cfg.tls.server_name.as_deref(), Some("sni.example"));
        assert_eq!(cfg.transport.as_ref().unwrap().kind, TransportType::Ws);
    }

    #[test]
    fn parse_trojan_url_decodes_password() {
        let uri = "trojan://hello%20world@trojan.example:443#T";
        let p = parse_share_link(uri).unwrap();
        let ProtocolConfig::Trojan(cfg) = &p.config else {
            panic!("ProtocolConfig variant mismatch")
        };
        assert_eq!(cfg.password, "hello world");
    }

    // ---- Shadowsocks ----

    #[test]
    fn parse_shadowsocks_sip002() {
        use base64::Engine;
        let creds = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("aes-256-gcm:ssecret");
        let uri = format!("ss://{}@ss.example:8388#SS-1", creds);
        let p = parse_share_link(&uri).unwrap();
        assert_eq!(p.protocol(), Protocol::Shadowsocks);
        let ProtocolConfig::Shadowsocks(cfg) = &p.config else {
            panic!("ProtocolConfig variant mismatch")
        };
        assert_eq!(cfg.method, ShadowsocksCipher::Aes256Gcm);
        assert_eq!(cfg.password, "ssecret");
        assert_eq!(p.address, "ss.example");
        assert_eq!(p.port, 8388);
        assert_eq!(p.name, "SS-1");
    }

    #[test]
    fn parse_shadowsocks_legacy_form() {
        use base64::Engine;
        let blob = base64::engine::general_purpose::STANDARD
            .encode("chacha20-ietf-poly1305:pw@1.1.1.1:8388");
        let uri = format!("ss://{}#Legacy", blob);
        let p = parse_share_link(&uri).unwrap();
        let ProtocolConfig::Shadowsocks(cfg) = &p.config else {
            panic!("ProtocolConfig variant mismatch")
        };
        assert_eq!(cfg.method, ShadowsocksCipher::Chacha20IetfPoly1305);
        assert_eq!(cfg.password, "pw");
        assert_eq!(p.address, "1.1.1.1");
        assert_eq!(p.port, 8388);
        assert_eq!(p.name, "Legacy");
    }

    #[test]
    fn parse_shadowsocks_unsupported_cipher_fails() {
        use base64::Engine;
        let creds = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("aes-128-cfb:pw");
        let uri = format!("ss://{}@1.2.3.4:8388#X", creds);
        assert!(parse_share_link(&uri).is_err());
    }

    // ---- Hysteria2 ----

    #[test]
    fn parse_hysteria2_with_obfs_and_alias() {
        let uri = "hy2://hp@hy.example:443?obfs=salamander&obfs-password=ob&sni=sni.example&insecure=1&alpn=h3#H2";
        let p = parse_share_link(uri).unwrap();
        assert_eq!(p.protocol(), Protocol::Hysteria2);
        let ProtocolConfig::Hysteria2(cfg) = &p.config else {
            panic!("ProtocolConfig variant mismatch")
        };
        assert_eq!(cfg.password, "hp");
        let obfs = cfg.obfs.as_ref().unwrap();
        assert_eq!(obfs.kind, Hysteria2ObfsType::Salamander);
        assert_eq!(obfs.password, "ob");
        assert_eq!(cfg.tls.server_name.as_deref(), Some("sni.example"));
        assert!(cfg.tls.insecure);
        assert_eq!(cfg.tls.alpn, vec!["h3".to_string()]);
    }

    // ---- TUIC ----

    #[test]
    fn parse_tuic_basic() {
        let uri = "tuic://tu-uuid:tp@tuic.example:443?congestion_control=cubic&udp_relay_mode=quic&zero_rtt_handshake=1&alpn=h3&sni=sni.example#TUIC";
        let p = parse_share_link(uri).unwrap();
        assert_eq!(p.protocol(), Protocol::Tuic);
        let ProtocolConfig::Tuic(cfg) = &p.config else {
            panic!("ProtocolConfig variant mismatch")
        };
        assert_eq!(cfg.uuid, "tu-uuid");
        assert_eq!(cfg.password, "tp");
        assert_eq!(cfg.congestion_control, TuicCongestion::Cubic);
        assert_eq!(cfg.udp_relay_mode, TuicUdpRelayMode::Quic);
        assert!(cfg.zero_rtt_handshake);
        assert_eq!(cfg.tls.alpn, vec!["h3".to_string()]);
    }

    // ---- SOCKS / HTTP / SSH / AnyTLS / ShadowTLS ----

    #[test]
    fn parse_socks5_with_auth() {
        let p = parse_share_link("socks5://u:p@s.example:1080#S5").unwrap();
        assert_eq!(p.protocol(), Protocol::Socks);
        let ProtocolConfig::Socks(cfg) = &p.config else {
            panic!("ProtocolConfig variant mismatch")
        };
        assert_eq!(cfg.version, SocksVersion::V5);
        assert_eq!(cfg.username.as_deref(), Some("u"));
        assert_eq!(cfg.password.as_deref(), Some("p"));
    }

    #[test]
    fn parse_https_enables_tls() {
        let plain = parse_share_link("http://h.example:8080#HTTP").unwrap();
        let ProtocolConfig::Http(cfg) = &plain.config else {
            panic!("ProtocolConfig variant mismatch")
        };
        assert!(!tls_has_anything(&cfg.tls));

        let secure = parse_share_link("https://u:p@h.example#HTTPS").unwrap();
        assert_eq!(secure.port, 443);
        let ProtocolConfig::Http(cfg) = &secure.config else {
            panic!("ProtocolConfig variant mismatch")
        };
        assert!(cfg.tls.server_name.is_some());
        assert_eq!(cfg.username.as_deref(), Some("u"));
        assert_eq!(cfg.password.as_deref(), Some("p"));
    }

    fn tls_has_anything(tls: &TlsCommon) -> bool {
        tls.server_name.is_some()
            || tls.insecure
            || !tls.alpn.is_empty()
            || tls.utls_fingerprint.is_some()
            || tls.reality.is_some()
            || tls.ech.is_some()
    }

    #[test]
    fn parse_ssh_with_password_in_query() {
        let p = parse_share_link("ssh://alice@ssh.example:2222?password=p#SSH").unwrap();
        let ProtocolConfig::Ssh(cfg) = &p.config else {
            panic!("ProtocolConfig variant mismatch")
        };
        assert_eq!(cfg.user, "alice");
        assert_eq!(cfg.password.as_deref(), Some("p"));
        assert_eq!(p.port, 2222);
    }

    #[test]
    fn parse_anytls_basic() {
        let p = parse_share_link("anytls://pp@a.example:443?sni=sni.example#A").unwrap();
        let ProtocolConfig::Anytls(cfg) = &p.config else {
            panic!("ProtocolConfig variant mismatch")
        };
        assert_eq!(cfg.password, "pp");
        assert_eq!(cfg.tls.server_name.as_deref(), Some("sni.example"));
    }

    #[test]
    fn parse_shadowtls_basic() {
        let uri = "shadowtls://stp@st.example:443?version=3&ss-method=2022-blake3-aes-256-gcm&ss-password=isp&sni=sni.example#ST";
        let p = parse_share_link(uri).unwrap();
        let ProtocolConfig::Shadowtls(cfg) = &p.config else {
            panic!("ProtocolConfig variant mismatch")
        };
        assert_eq!(cfg.version, ShadowtlsVersion::V3);
        assert_eq!(cfg.password, "stp");
        assert_eq!(cfg.method, ShadowsocksCipher::Blake3Aes256Gcm);
        assert_eq!(cfg.ss_password, "isp");
        assert_eq!(cfg.tls.server_name.as_deref(), Some("sni.example"));
    }

    #[test]
    fn parse_shadowtls_requires_inner_ss_password() {
        // No ss-password means we can't build the SS detour.
        let uri = "shadowtls://stp@st.example:443?version=3&ss-method=aes-128-gcm#X";
        assert!(parse_share_link(uri).is_err());
    }

    // ---- Round-trip: encode → parse must reproduce the input profile
    // (modulo `id`, which is regenerated on every parse).

    fn assert_roundtrip(mut profile: Profile) {
        let link = encode_share_link(&profile).expect("encode");
        let mut parsed =
            parse_share_link(&link).unwrap_or_else(|e| panic!("parse failed for `{link}`: {e}"));
        parsed.id = profile.id;
        // `tags` and `subscription_id` are not transported via share links;
        // strip them from both sides for the comparison.
        profile.tags.clear();
        profile.subscription_id = None;
        assert_eq!(parsed, profile, "round-trip mismatch for `{link}`");
    }

    #[test]
    fn encode_vless_roundtrip_plain() {
        let p = Profile::new_vless(
            "VLESS plain".to_string(),
            "1.1.1.1".to_string(),
            443,
            "vless-uuid".to_string(),
        );
        assert_roundtrip(p);
    }

    #[test]
    fn encode_vless_roundtrip_reality() {
        let mut p = Profile::new_vless(
            "VLESS reality".to_string(),
            "rt.example".to_string(),
            443,
            "vless-uuid".to_string(),
        );
        let ProtocolConfig::Vless(ref mut cfg) = p.config else {
            unreachable!()
        };
        cfg.flow = Some(Flow::XtlsRprxVision);
        cfg.security = Some(Security::Reality);
        cfg.tls.utls_fingerprint = Some("chrome".to_string());
        cfg.tls.reality = Some(RealitySettings {
            public_key: "pbk-value".to_string(),
            short_id: "sid-value".to_string(),
            server_name: "rt.example".to_string(),
            spider_x: "/spx".to_string(),
        });
        cfg.transport_type = Some(TransportType::Grpc);
        cfg.transport_service_name = Some("svc".to_string());
        assert_roundtrip(p);
    }

    #[test]
    fn encode_vmess_roundtrip_with_tls_and_ws() {
        let p = Profile {
            id: Uuid::new_v4(),
            name: "VMess WS".to_string(),
            address: "vm.example".to_string(),
            port: 8443,
            config: ProtocolConfig::Vmess(VmessConfig {
                uuid: "vm-uuid".to_string(),
                alter_id: 0,
                security: VmessSecurity::Aes128Gcm,
                tls: TlsCommon {
                    server_name: Some("sni.example".to_string()),
                    alpn: vec!["h2".to_string(), "http/1.1".to_string()],
                    utls_fingerprint: Some("chrome".to_string()),
                    ..TlsCommon::default()
                },
                transport: Some(TransportConfig {
                    kind: TransportType::Ws,
                    path: Some("/ws".to_string()),
                    host: Some("host.example".to_string()),
                    service_name: None,
                    headers: HashMap::new(),
                }),
                ..VmessConfig::default()
            }),
            tags: Vec::new(),
            subscription_id: None,
        };
        assert_roundtrip(p);
    }

    #[test]
    fn encode_trojan_roundtrip() {
        let p = Profile {
            id: Uuid::new_v4(),
            name: "Trojan-1".to_string(),
            address: "tr.example".to_string(),
            port: 443,
            config: ProtocolConfig::Trojan(TrojanConfig {
                password: "hello world".to_string(),
                tls: TlsCommon {
                    server_name: Some("sni.example".to_string()),
                    ..TlsCommon::default()
                },
                transport: None,
            }),
            tags: Vec::new(),
            subscription_id: None,
        };
        assert_roundtrip(p);
    }

    #[test]
    fn encode_shadowsocks_roundtrip_sip002() {
        let p = Profile {
            id: Uuid::new_v4(),
            name: "SS AEAD-2022".to_string(),
            address: "ss.example".to_string(),
            port: 8388,
            config: ProtocolConfig::Shadowsocks(ShadowsocksConfig {
                method: ShadowsocksCipher::Blake3Aes128Gcm,
                password: "p4ssw0rd".to_string(),
            }),
            tags: Vec::new(),
            subscription_id: None,
        };
        assert_roundtrip(p);
    }

    #[test]
    fn encode_hysteria2_roundtrip_with_obfs() {
        let p = Profile {
            id: Uuid::new_v4(),
            name: "Hy2".to_string(),
            address: "hy.example".to_string(),
            port: 443,
            config: ProtocolConfig::Hysteria2(Hysteria2Config {
                password: "secret".to_string(),
                up_mbps: Some(100),
                down_mbps: Some(500),
                obfs: Some(Hysteria2Obfs {
                    kind: Hysteria2ObfsType::Salamander,
                    password: "obfs-pw".to_string(),
                }),
                tls: TlsCommon {
                    server_name: Some("hy.example".to_string()),
                    insecure: true,
                    ..TlsCommon::default()
                },
            }),
            tags: Vec::new(),
            subscription_id: None,
        };
        assert_roundtrip(p);
    }

    #[test]
    fn encode_tuic_roundtrip() {
        let p = Profile {
            id: Uuid::new_v4(),
            name: "TUIC".to_string(),
            address: "tuic.example".to_string(),
            port: 443,
            config: ProtocolConfig::Tuic(TuicConfig {
                uuid: "tuic-uuid".to_string(),
                password: "tuic-pass".to_string(),
                congestion_control: TuicCongestion::Cubic,
                udp_relay_mode: TuicUdpRelayMode::Quic,
                zero_rtt_handshake: true,
                tls: TlsCommon {
                    server_name: Some("tuic.example".to_string()),
                    alpn: vec!["h3".to_string()],
                    ..TlsCommon::default()
                },
            }),
            tags: Vec::new(),
            subscription_id: None,
        };
        assert_roundtrip(p);
    }

    #[test]
    fn encode_socks_roundtrip_with_auth() {
        let p = Profile {
            id: Uuid::new_v4(),
            name: "Socks".to_string(),
            address: "socks.example".to_string(),
            port: 1080,
            config: ProtocolConfig::Socks(SocksConfig {
                version: SocksVersion::V5,
                username: Some("alice".to_string()),
                password: Some("pa ss".to_string()),
            }),
            tags: Vec::new(),
            subscription_id: None,
        };
        assert_roundtrip(p);
    }

    #[test]
    fn encode_http_roundtrip_https_tls() {
        let p = Profile {
            id: Uuid::new_v4(),
            name: "HTTPS proxy".to_string(),
            address: "proxy.example".to_string(),
            port: 443,
            config: ProtocolConfig::Http(HttpConfig {
                username: Some("u".to_string()),
                password: Some("p".to_string()),
                tls: TlsCommon {
                    server_name: Some("proxy.example".to_string()),
                    ..TlsCommon::default()
                },
            }),
            tags: Vec::new(),
            subscription_id: None,
        };
        assert_roundtrip(p);
    }

    #[test]
    fn encode_ssh_roundtrip_key_path() {
        let p = Profile {
            id: Uuid::new_v4(),
            name: "SSH".to_string(),
            address: "ssh.example".to_string(),
            port: 22,
            config: ProtocolConfig::Ssh(SshConfig {
                user: "root".to_string(),
                password: None,
                private_key_path: Some("/home/me/.ssh/id_ed25519".to_string()),
                private_key_passphrase: Some("kp".to_string()),
                ..SshConfig::default()
            }),
            tags: Vec::new(),
            subscription_id: None,
        };
        assert_roundtrip(p);
    }

    #[test]
    fn encode_anytls_roundtrip() {
        let p = Profile {
            id: Uuid::new_v4(),
            name: "AnyTLS".to_string(),
            address: "at.example".to_string(),
            port: 443,
            config: ProtocolConfig::Anytls(AnytlsConfig {
                password: "anytls-pw".to_string(),
                tls: TlsCommon {
                    server_name: Some("at.example".to_string()),
                    ..TlsCommon::default()
                },
                ..AnytlsConfig::default()
            }),
            tags: Vec::new(),
            subscription_id: None,
        };
        assert_roundtrip(p);
    }

    #[test]
    fn encode_shadowtls_roundtrip_v3() {
        let p = Profile {
            id: Uuid::new_v4(),
            name: "ShadowTLS v3".to_string(),
            address: "st.example".to_string(),
            port: 443,
            config: ProtocolConfig::Shadowtls(ShadowtlsConfig {
                version: ShadowtlsVersion::V3,
                password: "stls-pw".to_string(),
                method: ShadowsocksCipher::Aes128Gcm,
                ss_password: "inner-ss-pw".to_string(),
                tls: TlsCommon {
                    server_name: Some("st.example".to_string()),
                    ..TlsCommon::default()
                },
            }),
            tags: Vec::new(),
            subscription_id: None,
        };
        assert_roundtrip(p);
    }

    // ---- ShadowtlsVersion ----

    #[test]
    fn shadowtls_version_as_u8_maps_each_variant() {
        assert_eq!(ShadowtlsVersion::V1.as_u8(), 1);
        assert_eq!(ShadowtlsVersion::V2.as_u8(), 2);
        assert_eq!(ShadowtlsVersion::V3.as_u8(), 3);
    }

    #[test]
    fn shadowtls_version_serializes_as_numeric() {
        assert_eq!(serde_json::to_string(&ShadowtlsVersion::V1).unwrap(), "1");
        assert_eq!(serde_json::to_string(&ShadowtlsVersion::V2).unwrap(), "2");
        assert_eq!(serde_json::to_string(&ShadowtlsVersion::V3).unwrap(), "3");
    }

    #[test]
    fn shadowtls_version_deserializes_each_known_value() {
        let v: ShadowtlsVersion = serde_json::from_str("1").unwrap();
        assert_eq!(v, ShadowtlsVersion::V1);
        let v: ShadowtlsVersion = serde_json::from_str("2").unwrap();
        assert_eq!(v, ShadowtlsVersion::V2);
        let v: ShadowtlsVersion = serde_json::from_str("3").unwrap();
        assert_eq!(v, ShadowtlsVersion::V3);
    }

    #[test]
    fn shadowtls_version_rejects_unknown_value() {
        let err = serde_json::from_str::<ShadowtlsVersion>("4").unwrap_err();
        assert!(err.to_string().contains("unknown ShadowTLS version"));
    }

    #[test]
    fn shadowtls_version_default_is_v3() {
        assert_eq!(ShadowtlsVersion::default(), ShadowtlsVersion::V3);
    }

    // ---- TlsCommon::validate ----

    #[test]
    fn tls_common_validate_accepts_plain() {
        TlsCommon::default().validate().unwrap();
    }

    #[test]
    fn tls_common_validate_accepts_reality_only() {
        let tls = TlsCommon {
            reality: Some(RealitySettings {
                public_key: "pk".into(),
                short_id: "sid".into(),
                server_name: "sn".into(),
                spider_x: "/".into(),
            }),
            ..TlsCommon::default()
        };
        tls.validate().unwrap();
    }

    #[test]
    fn tls_common_validate_accepts_ech_only() {
        let tls = TlsCommon {
            ech: Some(EchSettings {
                enabled: true,
                ..EchSettings::default()
            }),
            ..TlsCommon::default()
        };
        tls.validate().unwrap();
    }

    #[test]
    fn tls_common_validate_accepts_reality_with_disabled_ech() {
        // The mutual-exclusion check fires only when ECH is *enabled*.
        let tls = TlsCommon {
            reality: Some(RealitySettings {
                public_key: "pk".into(),
                short_id: "sid".into(),
                server_name: "sn".into(),
                spider_x: "/".into(),
            }),
            ech: Some(EchSettings {
                enabled: false,
                ..EchSettings::default()
            }),
            ..TlsCommon::default()
        };
        tls.validate().unwrap();
    }

    #[test]
    fn tls_common_validate_rejects_reality_with_enabled_ech() {
        let tls = TlsCommon {
            reality: Some(RealitySettings {
                public_key: "pk".into(),
                short_id: "sid".into(),
                server_name: "sn".into(),
                spider_x: "/".into(),
            }),
            ech: Some(EchSettings {
                enabled: true,
                ..EchSettings::default()
            }),
            ..TlsCommon::default()
        };
        let err = tls.validate().unwrap_err().to_string();
        assert!(err.contains("mutually exclusive"));
    }

    // ---- Config::migrate ----

    #[test]
    fn migrate_v0_promotes_legacy_dns_strategy() {
        let mut cfg = Config {
            schema_version: 0,
            ..Config::default()
        };
        // Legacy field set, new field at default → promote.
        cfg.settings.dns_strategy = DnsStrategy::OnlyIpv6;
        cfg.settings.dns.strategy = DnsStrategy::default(); // PreferIpv4
        cfg.migrate().unwrap();
        assert_eq!(cfg.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(cfg.settings.dns.strategy, DnsStrategy::OnlyIpv6);
        assert_eq!(cfg.settings.dns_strategy, DnsStrategy::OnlyIpv6);
    }

    #[test]
    fn migrate_v0_keeps_new_dns_strategy_when_legacy_is_default() {
        let mut cfg = Config {
            schema_version: 0,
            ..Config::default()
        };
        cfg.settings.dns.strategy = DnsStrategy::OnlyIpv4;
        cfg.settings.dns_strategy = DnsStrategy::default();
        cfg.migrate().unwrap();
        // New field wins; legacy is synced from it.
        assert_eq!(cfg.settings.dns.strategy, DnsStrategy::OnlyIpv4);
        assert_eq!(cfg.settings.dns_strategy, DnsStrategy::OnlyIpv4);
    }

    #[test]
    fn migrate_is_noop_when_schema_already_current() {
        let mut cfg = Config {
            schema_version: CURRENT_SCHEMA_VERSION,
            ..Config::default()
        };
        cfg.settings.dns_strategy = DnsStrategy::OnlyIpv6;
        cfg.settings.dns.strategy = DnsStrategy::OnlyIpv4;
        cfg.migrate().unwrap();
        // No promotion when schema_version != 0.
        assert_eq!(cfg.settings.dns.strategy, DnsStrategy::OnlyIpv4);
        assert_eq!(cfg.settings.dns_strategy, DnsStrategy::OnlyIpv6);
    }

    #[test]
    fn migrate_v0_is_idempotent() {
        let mut cfg = Config {
            schema_version: 0,
            ..Config::default()
        };
        cfg.settings.dns_strategy = DnsStrategy::OnlyIpv6;
        cfg.migrate().unwrap();
        let after_first = cfg.clone();
        cfg.migrate().unwrap();
        assert_eq!(cfg.schema_version, after_first.schema_version);
        assert_eq!(cfg.settings.dns.strategy, after_first.settings.dns.strategy);
        assert_eq!(cfg.settings.dns_strategy, after_first.settings.dns_strategy);
    }

    fn vless_profile_with_legacy_fingerprint(fp: &str) -> Profile {
        let mut p = Profile::new_vless(
            "Legacy".to_string(),
            "1.2.3.4".to_string(),
            443,
            "u".to_string(),
        );
        if let ProtocolConfig::Vless(ref mut cfg) = p.config {
            cfg.legacy_fingerprint = Some(fp.to_string());
        }
        p
    }

    #[test]
    fn migrate_v1_to_v2_promotes_legacy_vless_fingerprint() {
        let mut cfg = Config {
            schema_version: 1,
            ..Config::default()
        };
        cfg.profiles
            .push(vless_profile_with_legacy_fingerprint("chrome"));
        cfg.migrate().unwrap();
        assert_eq!(cfg.schema_version, CURRENT_SCHEMA_VERSION);
        let vc = vless_cfg(&cfg.profiles[0]);
        assert_eq!(vc.tls.utls_fingerprint.as_deref(), Some("chrome"));
        assert!(vc.legacy_fingerprint.is_none());
    }

    #[test]
    fn migrate_v1_to_v2_keeps_new_fingerprint_when_both_set() {
        // If a future writer somehow produced both, the new slot wins
        // (legacy is treated purely as a one-way input).
        let mut cfg = Config {
            schema_version: 1,
            ..Config::default()
        };
        let mut p = vless_profile_with_legacy_fingerprint("legacy-fp");
        if let ProtocolConfig::Vless(ref mut vc) = p.config {
            vc.tls.utls_fingerprint = Some("new-fp".to_string());
        }
        cfg.profiles.push(p);
        cfg.migrate().unwrap();
        let vc = vless_cfg(&cfg.profiles[0]);
        assert_eq!(vc.tls.utls_fingerprint.as_deref(), Some("new-fp"));
        assert!(vc.legacy_fingerprint.is_none());
    }

    #[test]
    fn migrate_v1_to_v2_is_idempotent() {
        let mut cfg = Config {
            schema_version: 1,
            ..Config::default()
        };
        cfg.profiles
            .push(vless_profile_with_legacy_fingerprint("chrome"));
        cfg.migrate().unwrap();
        let first = cfg.clone();
        cfg.migrate().unwrap();
        assert_eq!(cfg, first);
    }

    #[test]
    fn migrate_v1_to_v2_noop_for_non_vless_profiles() {
        let mut cfg = Config {
            schema_version: 1,
            ..Config::default()
        };
        cfg.profiles.push(Profile {
            id: Uuid::new_v4(),
            name: "VM".to_string(),
            address: "1.1.1.1".to_string(),
            port: 443,
            config: ProtocolConfig::Vmess(VmessConfig {
                uuid: "vm-uuid".to_string(),
                ..VmessConfig::default()
            }),
            tags: Vec::new(),
            subscription_id: None,
        });
        cfg.migrate().unwrap();
        assert_eq!(cfg.schema_version, CURRENT_SCHEMA_VERSION);
        // No panic, no mutation.
    }

    #[test]
    fn migrate_chains_v0_through_v2() {
        // schema_version=0 must arrive at v2 in a single migrate() call,
        // running both v0→v1 and v1→v2 steps.
        let mut cfg = Config {
            schema_version: 0,
            ..Config::default()
        };
        cfg.settings.dns_strategy = DnsStrategy::OnlyIpv6;
        cfg.profiles
            .push(vless_profile_with_legacy_fingerprint("chrome"));
        cfg.migrate().unwrap();
        assert_eq!(cfg.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(cfg.settings.dns.strategy, DnsStrategy::OnlyIpv6);
        let vc = vless_cfg(&cfg.profiles[0]);
        assert_eq!(vc.tls.utls_fingerprint.as_deref(), Some("chrome"));
    }

    // ---- P0 regression: VLESS plain-TLS share links must preserve
    // sni / alpn / insecure end-to-end.

    #[test]
    fn parse_vless_plain_tls_preserves_sni() {
        let uri = "vless://uuid@1.2.3.4:443?security=tls&sni=cdn.example.com#X";
        let p = parse_share_link(uri).unwrap();
        let cfg = vless_cfg(&p);
        assert_eq!(cfg.security, Some(Security::Tls));
        assert_eq!(cfg.tls.server_name.as_deref(), Some("cdn.example.com"));
        assert!(cfg.tls.reality.is_none());
    }

    #[test]
    fn parse_vless_plain_tls_preserves_alpn_and_insecure() {
        let uri = "vless://uuid@1.2.3.4:443?security=tls&alpn=h2,http/1.1&insecure=1&fp=chrome#X";
        let p = parse_share_link(uri).unwrap();
        let cfg = vless_cfg(&p);
        assert_eq!(cfg.tls.alpn, vec!["h2".to_string(), "http/1.1".to_string()]);
        assert!(cfg.tls.insecure);
        assert_eq!(cfg.tls.utls_fingerprint.as_deref(), Some("chrome"));
    }

    #[test]
    fn encode_vless_plain_tls_roundtrip() {
        let mut p = Profile::new_vless(
            "VLESS-TLS".to_string(),
            "1.2.3.4".to_string(),
            443,
            "vless-uuid".to_string(),
        );
        if let ProtocolConfig::Vless(ref mut cfg) = p.config {
            cfg.security = Some(Security::Tls);
            cfg.tls.server_name = Some("cdn.example.com".to_string());
            cfg.tls.alpn = vec!["h2".to_string(), "http/1.1".to_string()];
            cfg.tls.insecure = true;
            cfg.tls.utls_fingerprint = Some("chrome".to_string());
        }
        assert_roundtrip(p);
    }

    // ---- Error-path coverage: malformed share links must surface as
    // Result::Err rather than panic, and must not silently construct a
    // profile with empty credentials. Closes the "user pasted a broken
    // URI from a Telegram channel" gap that share_link.rs guards.

    #[test]
    fn parse_vmess_b64_rejects_invalid_base64() {
        // `!` is outside every base64 alphabet.
        assert!(parse_share_link("vmess://not!!base64").is_err());
    }

    #[test]
    fn parse_vmess_b64_rejects_missing_id() {
        use base64::Engine;
        // Valid base64 + valid JSON, but no `id` field.
        let json = r#"{"add":"1.1.1.1","port":443,"ps":"X"}"#;
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json.as_bytes());
        let err = parse_share_link(&format!("vmess://{b64}"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("missing 'id'"), "Error was: {err}");
    }

    #[test]
    fn parse_vmess_b64_rejects_missing_address() {
        use base64::Engine;
        let json = r#"{"port":443,"id":"u","ps":"X"}"#;
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json.as_bytes());
        let err = parse_share_link(&format!("vmess://{b64}"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("missing 'add'"), "Error was: {err}");
    }

    #[test]
    fn parse_trojan_rejects_empty_password() {
        assert!(parse_share_link("trojan://@trojan.example:443#X").is_err());
    }

    #[test]
    fn parse_hysteria2_rejects_empty_password() {
        assert!(parse_share_link("hysteria2://@hy.example:443#X").is_err());
    }

    #[test]
    fn parse_tuic_rejects_missing_password() {
        // `uuid:` with empty password.
        assert!(parse_share_link("tuic://uuid:@tuic.example:443#X").is_err());
    }

    #[test]
    fn parse_anytls_rejects_empty_password() {
        assert!(parse_share_link("anytls://@a.example:443#X").is_err());
    }

    #[test]
    fn parse_ssh_rejects_missing_user() {
        assert!(parse_share_link("ssh://@ssh.example:22#X").is_err());
    }

    #[test]
    fn parse_socks_rejects_missing_port() {
        // socks5:// without an explicit port — there's no protocol default.
        assert!(parse_share_link("socks5://user:pass@socks.example#X").is_err());
    }
}
