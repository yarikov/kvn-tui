use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Supported VPN protocols.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Vless,
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Protocol::Vless => write!(f, "vless"),
        }
    }
}

/// Routing mode for geoip/geosite rules.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RoutingMode {
    #[default]
    Global,
    BypassRu,
    OnlyRu,
}

impl RoutingMode {
    pub const ALL: &'static [RoutingMode] = &[
        RoutingMode::Global,
        RoutingMode::BypassRu,
        RoutingMode::OnlyRu,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            RoutingMode::Global => "Global",
            RoutingMode::BypassRu => "Bypass RU",
            RoutingMode::OnlyRu => "Only RU",
        }
    }

    pub fn index(&self) -> usize {
        match self {
            RoutingMode::Global => 0,
            RoutingMode::BypassRu => 1,
            RoutingMode::OnlyRu => 2,
        }
    }

    pub fn from_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(RoutingMode::Global),
            1 => Some(RoutingMode::BypassRu),
            2 => Some(RoutingMode::OnlyRu),
            _ => None,
        }
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum DnsStrategy {
    #[default]
    #[serde(rename = "prefer_ipv4")]
    PreferIpv4,
    #[serde(rename = "prefer_ipv6")]
    PreferIpv6,
    #[serde(rename = "ipv4_only")]
    OnlyIpv4,
    #[serde(rename = "ipv6_only")]
    OnlyIpv6,
}

/// Single VPN profile.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    #[serde(default = "Uuid::new_v4")]
    pub id: Uuid,
    pub name: String,
    pub protocol: Protocol,
    pub address: String,
    pub port: u16,
    pub uuid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flow: Option<Flow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security: Option<Security>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reality: Option<RealitySettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport_type: Option<TransportType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport_service_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

impl Profile {
    /// Create a new profile with a generated UUID.
    pub fn new(name: String, protocol: Protocol, address: String, port: u16, uuid: String) -> Self {
        Self {
            id: Uuid::new_v4(),
            name,
            protocol,
            address,
            port,
            uuid,
            flow: None,
            security: None,
            reality: None,
            transport_type: None,
            transport_service_name: None,
            fingerprint: None,
            tags: Vec::new(),
        }
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
    #[serde(default = "default_dns_strategy")]
    pub dns_strategy: DnsStrategy,
    #[serde(default)]
    pub routing_mode: RoutingMode,
}

fn default_tun_interface() -> String {
    "tun0".to_string()
}

fn default_dns_strategy() -> DnsStrategy {
    DnsStrategy::PreferIpv4
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            default_profile: None,
            tun_interface: default_tun_interface(),
            dns_strategy: default_dns_strategy(),
            routing_mode: RoutingMode::default(),
        }
    }
}

/// Root configuration file structure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub profiles: Vec<Profile>,
    #[serde(default)]
    pub settings: Settings,
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
    pub fn validate(&self) -> anyhow::Result<()> {
        for (idx, profile) in self.profiles.iter().enumerate() {
            let num = idx + 1;
            if profile.name.trim().is_empty() {
                anyhow::bail!("Profile {num}: name must not be empty");
            }
            if profile.address.trim().is_empty() {
                anyhow::bail!("Profile {num}: address must not be empty");
            }
            if profile.uuid.trim().is_empty() {
                anyhow::bail!("Profile {num}: uuid must not be empty");
            }
        }

        if let Some(id) = self.settings.default_profile {
            if !self.profiles.iter().any(|p| p.id == id) {
                anyhow::bail!(
                    "settings.default_profile ({id}) references a non-existent profile"
                );
            }
        }

        Ok(())
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
    fn routing_mode_as_str() {
        assert_eq!(RoutingMode::Global.as_str(), "Global");
        assert_eq!(RoutingMode::BypassRu.as_str(), "Bypass RU");
        assert_eq!(RoutingMode::OnlyRu.as_str(), "Only RU");
    }

    #[test]
    fn routing_mode_index_roundtrip() {
        for mode in RoutingMode::ALL {
            assert_eq!(RoutingMode::from_index(mode.index()), Some(*mode));
        }
        assert_eq!(RoutingMode::from_index(3), None);
        assert_eq!(RoutingMode::from_index(100), None);
    }

    #[test]
    fn profile_new_defaults() {
        let p = Profile::new(
            "test".to_string(),
            Protocol::Vless,
            "1.2.3.4".to_string(),
            443,
            "uuid-here".to_string(),
        );
        assert_eq!(p.name, "test");
        assert_eq!(p.protocol, Protocol::Vless);
        assert_eq!(p.address, "1.2.3.4");
        assert_eq!(p.port, 443);
        assert_eq!(p.uuid, "uuid-here");
        assert!(p.flow.is_none());
        assert!(p.security.is_none());
        assert!(p.reality.is_none());
        assert!(p.transport_type.is_none());
        assert!(p.transport_service_name.is_none());
        assert!(p.fingerprint.is_none());
        assert!(p.tags.is_empty());
        // UUID should be non-nil
        assert_ne!(p.id, Uuid::nil());
    }

    #[test]
    fn settings_default() {
        let s = Settings::default();
        assert_eq!(s.tun_interface, "tun0");
        assert_eq!(s.dns_strategy, DnsStrategy::PreferIpv4);
        assert_eq!(s.routing_mode, RoutingMode::Global);
        assert!(s.default_profile.is_none());
    }

    #[test]
    fn config_default() {
        let c = Config::default();
        assert!(c.profiles.is_empty());
        assert_eq!(c.settings.tun_interface, "tun0");
    }

    #[test]
    fn config_serde_roundtrip() {
        let mut config = Config::default();
        let mut profile = Profile::new(
            "Example".to_string(),
            Protocol::Vless,
            "203.0.113.1".to_string(),
            443,
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
        );
        profile.security = Some(Security::Reality);
        profile.reality = Some(RealitySettings {
            public_key: "pk".to_string(),
            short_id: "sid".to_string(),
            server_name: "sni".to_string(),
            spider_x: "/".to_string(),
        });
        profile.tags = vec!["tag1".to_string()];
        config.profiles.push(profile);
        config.settings.routing_mode = RoutingMode::BypassRu;

        let json = serde_json::to_string(&config).unwrap();
        let restored: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(config, restored);
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
        assert!(p.flow.is_none());
        assert!(p.reality.is_none());
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
    fn profile_rejects_unknown_field() {
        let json = r#"{
            "id": "550e8400-e29b-41d4-a716-446655440000",
            "name": "Test",
            "protocol": "vless",
            "address": "1.1.1.1",
            "port": 443,
            "uuid": "uuid",
            "unknown_field": true
        }"#;
        let result: Result<Profile, _> = serde_json::from_str(json);
        assert!(result.is_err(), "Should reject unknown profile field");
    }

    #[test]
    fn config_validate_accepts_valid_config() {
        let mut config = Config::default();
        config.profiles.push(Profile::new(
            "Valid".to_string(),
            Protocol::Vless,
            "1.2.3.4".to_string(),
            443,
            "uuid".to_string(),
        ));
        config.settings.default_profile = Some(config.profiles[0].id);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn config_validate_rejects_empty_profile_name() {
        let mut config = Config::default();
        config.profiles.push(Profile::new(
            "   ".to_string(),
            Protocol::Vless,
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
        config.profiles.push(Profile::new(
            "Name".to_string(),
            Protocol::Vless,
            "".to_string(),
            443,
            "uuid".to_string(),
        ));
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("address must not be empty"), "Error was: {}", err);
    }

    #[test]
    fn config_validate_rejects_empty_profile_uuid() {
        let mut config = Config::default();
        config.profiles.push(Profile::new(
            "Name".to_string(),
            Protocol::Vless,
            "1.2.3.4".to_string(),
            443,
            "  ".to_string(),
        ));
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("uuid must not be empty"), "Error was: {}", err);
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
}
