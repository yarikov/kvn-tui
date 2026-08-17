use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::config::profile::{
    DnsConfig, DnsServer, GeoRegion, Profile, ProtocolConfig, RoutedService, RoutingMode,
    ServiceRoute, Settings,
};
use crate::singbox::outbound::{
    build_anytls_outbound, build_http_outbound, build_hysteria2_outbound,
    build_shadowsocks_outbound, build_shadowtls_outbounds, build_socks_outbound,
    build_ssh_outbound, build_trojan_outbound, build_tuic_outbound, build_vless_outbound,
    build_vmess_outbound,
};

/// Availability of local geoip/geosite rule-sets used when building routes.
/// A region is present only when both its files exist.
#[derive(Debug, Clone, Default)]
pub struct GeoAvailability {
    /// `(geoip_path, geosite_path)` per region. Populated by the runner from
    /// disk; `build_route` reads via `get`. Regions without rule-sets
    /// (`GeoRegion::Global`) never appear here.
    pub regions: HashMap<GeoRegion, (PathBuf, PathBuf)>,
    /// `(tag, path)` per rule-set, in rule order, for each service whose
    /// defined rule-set files all exist on disk.
    pub services: HashMap<RoutedService, Vec<(&'static str, PathBuf)>>,
}

impl GeoAvailability {
    pub fn get(&self, region: GeoRegion) -> Option<&(PathBuf, PathBuf)> {
        self.regions.get(&region)
    }

    /// All rule-sets available with dummy paths under `/geo/`, for tests.
    #[cfg(test)]
    pub fn all() -> Self {
        let mut regions = HashMap::new();
        for region in GeoRegion::ALL {
            if let Some(a) = crate::geo::region_assets(region) {
                regions.insert(
                    region,
                    (
                        PathBuf::from("/geo").join(a.geoip.filename),
                        PathBuf::from("/geo").join(a.geosite.filename),
                    ),
                );
            }
        }
        let mut services = HashMap::new();
        for service in RoutedService::ALL {
            services.insert(
                service,
                crate::geo::service_assets(service)
                    .present()
                    .into_iter()
                    .map(|a| (a.tag(), PathBuf::from("/geo").join(a.filename)))
                    .collect(),
            );
        }
        Self { regions, services }
    }
}

/// Generate a minimal sing-box config for latency testing: one SOCKS5 inbound
/// on `socks_port` (localhost only), the profile's outbound, and a route that
/// sends all traffic to that outbound.
pub fn generate_test_config(profile: &Profile, socks_port: u16) -> anyhow::Result<Value> {
    let outbounds = build_outbound(profile)?;
    Ok(json!({
        "log": { "level": "warn" },
        "inbounds": [{
            "type": "socks",
            "tag": "socks-in",
            "listen": "127.0.0.1",
            "listen_port": socks_port
        }],
        "outbounds": outbounds,
        "route": { "final": "proxy", "default_mark": 666, "auto_detect_interface": true }
    }))
}

/// Generate a complete sing-box JSON configuration from a profile.
/// Uses the modern sing-box 1.12+ format.
pub fn generate_config(
    profile: &Profile,
    settings: &Settings,
    geo: &GeoAvailability,
) -> anyhow::Result<Value> {
    let mut proxy_outbounds = build_outbound(profile)?;
    proxy_outbounds.push(json!({ "type": "direct", "tag": "direct" }));
    let (route, rule_sets) = build_route(
        &settings.geo_routing.mode(),
        &settings.dns,
        geo,
        &settings.geo_routing.service_routes,
    );
    let dns = build_dns(&settings.dns);

    let mut cache_file = json!({ "enabled": true });
    if settings.dns.fakeip_enabled && settings.dns.fakeip_server().is_some() {
        cache_file["store_fakeip"] = json!(true);
    }

    let mut config = json!({
        "log": {
            "level": crate::config::profile::normalized_log_level(&settings.log_level),
            "output": crate::paths::singbox_log_path().to_string_lossy(),
            "timestamp": true
        },
        "dns": dns,
        "inbounds": [
            {
                "type": "tun",
                "tag": "tun-in",
                "interface_name": settings.tun_interface.clone(),
                "address": ["10.222.0.1/30"],
                "mtu": 1420,
                "auto_route": true,
                "strict_route": true,
                "endpoint_independent_nat": true,
                "stack": "gvisor"
            }
        ],
        "outbounds": proxy_outbounds,
        "route": route,
        "experimental": {
            "cache_file": cache_file,
            "clash_api": {
                "external_controller": "127.0.0.1:9090"
            }
        }
    });

    // Merge rule_sets into route if any exist.
    if !rule_sets.is_empty() {
        config["route"]["rule_set"] = json!(rule_sets);
    }

    Ok(config)
}

/// Build the `dns` section from user configuration. Maps onto sing-box 1.12's
/// `dns` schema: server entries carry their own fake-IP ranges (no legacy
/// top-level `dns.fakeip` block), and when the user toggles fake-IP on we
/// auto-prepend an `A`/`AAAA`-routing rule and flip `independent_cache` so the
/// fake-IP server actually receives queries and its mappings stay separate
/// from upstream caches.
fn build_dns(dns: &DnsConfig) -> Value {
    let servers: Vec<Value> = dns.servers.iter().map(build_dns_server).collect();
    let mut block = Map::new();
    block.insert("servers".to_string(), Value::Array(servers));

    let mut rules: Vec<Value> = dns.rules.iter().map(build_dns_rule).collect();
    if dns.fakeip_enabled
        && let Some(server) = dns.fakeip_server()
    {
        let tag = server.tag().to_string();
        let already_routed = dns.rules.iter().any(|r| r.server == tag);
        if !already_routed {
            rules.insert(
                0,
                json!({
                    "query_type": ["A", "AAAA"],
                    "server": tag,
                }),
            );
        }
    }
    if !rules.is_empty() {
        block.insert("rules".to_string(), Value::Array(rules));
    }

    block.insert("final".to_string(), json!(dns.final_server));
    block.insert("strategy".to_string(), json!(dns.strategy.as_str()));
    if dns.fakeip_enabled && dns.fakeip_server().is_some() {
        block.insert("independent_cache".to_string(), json!(true));
    }
    Value::Object(block)
}

fn build_dns_server(server: &DnsServer) -> Value {
    match server {
        DnsServer::Local { tag } => json!({ "tag": tag, "type": "local" }),
        DnsServer::Udp {
            tag,
            server,
            server_port,
        } => server_with_port("udp", tag, server, *server_port, None),
        DnsServer::Tcp {
            tag,
            server,
            server_port,
        } => server_with_port("tcp", tag, server, *server_port, None),
        DnsServer::Tls {
            tag,
            server,
            server_port,
        } => server_with_port("tls", tag, server, *server_port, None),
        DnsServer::Https {
            tag,
            server,
            server_port,
            path,
        } => server_with_port("https", tag, server, *server_port, Some(path.as_str())),
        DnsServer::Quic {
            tag,
            server,
            server_port,
        } => server_with_port("quic", tag, server, *server_port, None),
        DnsServer::FakeIp {
            tag,
            inet4_range,
            inet6_range,
        } => json!({
            "tag": tag,
            "type": "fakeip",
            "inet4_range": inet4_range,
            "inet6_range": inet6_range,
        }),
    }
}

fn server_with_port(
    ty: &str,
    tag: &str,
    server: &str,
    port: Option<u16>,
    doh_path: Option<&str>,
) -> Value {
    let mut obj = Map::new();
    obj.insert("tag".to_string(), json!(tag));
    obj.insert("type".to_string(), json!(ty));
    obj.insert("server".to_string(), json!(server));
    if let Some(p) = port {
        obj.insert("server_port".to_string(), json!(p));
    }
    if let Some(path) = doh_path {
        obj.insert("path".to_string(), json!(path));
    }
    Value::Object(obj)
}

fn build_dns_rule(rule: &crate::config::profile::DnsRule) -> Value {
    let mut obj = Map::new();
    if !rule.domain.is_empty() {
        obj.insert("domain".to_string(), json!(rule.domain));
    }
    if !rule.domain_suffix.is_empty() {
        obj.insert("domain_suffix".to_string(), json!(rule.domain_suffix));
    }
    if !rule.domain_keyword.is_empty() {
        obj.insert("domain_keyword".to_string(), json!(rule.domain_keyword));
    }
    if !rule.domain_regex.is_empty() {
        obj.insert("domain_regex".to_string(), json!(rule.domain_regex));
    }
    if !rule.rule_set.is_empty() {
        obj.insert("rule_set".to_string(), json!(rule.rule_set));
    }
    obj.insert("server".to_string(), json!(rule.server));
    if rule.disable_cache {
        obj.insert("disable_cache".to_string(), json!(true));
    }
    Value::Object(obj)
}

/// Build route object and local rule-sets based on routing mode.
/// Returns (route_value, rule_sets_vec).
fn build_route(
    routing_mode: &RoutingMode,
    dns: &DnsConfig,
    geo: &GeoAvailability,
    service_routes: &HashMap<RoutedService, ServiceRoute>,
) -> (Value, Vec<Value>) {
    let mut rules = vec![
        json!({
            "ip_version": 6,
            "action": "reject"
        }),
        json!({
            "inbound": ["tun-in"],
            "port": 53,
            "action": "hijack-dns"
        }),
        json!({
            "ip_cidr": ["10.222.0.0/30"],
            "outbound": "direct"
        }),
    ];

    let mut rule_sets: Vec<Value> = Vec::new();

    // Per-service routing overrides, placed before the regional geo rules so
    // an explicit override wins in every routing mode (`Direct` beats a
    // region's `Only` → proxy; `Proxy` beats its `Bypass` → direct). A
    // service is skipped while its rule-set files are missing — a pending
    // download must never fail the connection. Iterates `RoutedService::ALL`,
    // not the map: `HashMap` iteration order is nondeterministic and would
    // make the generated config unstable across runs.
    for service in RoutedService::ALL {
        let outbound = match service_routes.get(&service).copied().unwrap_or_default() {
            ServiceRoute::Disabled => continue,
            ServiceRoute::Proxy => "proxy",
            ServiceRoute::Direct => "direct",
        };
        let Some(entries) = geo.services.get(&service) else {
            continue;
        };
        let tags: Vec<&str> = entries.iter().map(|(tag, _)| *tag).collect();
        rules.push(json!({
            "rule_set": tags,
            "outbound": outbound,
        }));
        for (tag, path) in entries {
            rule_sets.push(json!({
                "tag": tag,
                "type": "local",
                "format": "binary",
                "path": path,
            }));
        }
    }

    match routing_mode {
        RoutingMode::Global => {}
        RoutingMode::Bypass(region) | RoutingMode::Only(region)
            if !matches!(region, GeoRegion::Global) =>
        {
            // `Bypass` sends matching traffic out the direct outbound; `Only`
            // sends matching traffic through the proxy.
            let outbound = if matches!(routing_mode, RoutingMode::Bypass(_)) {
                "direct"
            } else {
                "proxy"
            };
            rules.push(json!({
                "ip_is_private": true,
                "outbound": "direct",
            }));
            if let (Some(assets), Some((geoip_path, geosite_path))) =
                (crate::geo::region_assets(*region), geo.get(*region))
            {
                // Existing order: geosite rule first, then geoip. Tags are
                // derived from filenames (`geoip-ru.srs` → `geoip-ru`).
                for (path, asset) in [(geosite_path, &assets.geosite), (geoip_path, &assets.geoip)]
                {
                    let tag = asset.tag();
                    rules.push(json!({
                        "rule_set": [tag],
                        "outbound": outbound,
                    }));
                    rule_sets.push(json!({
                        "tag": tag,
                        "type": "local",
                        "format": "binary",
                        "path": path,
                    }));
                }
            }
        }
        // Bypass(Global) / Only(Global) are unreachable through `available()`
        // but the enum permits them — degrade to a no-op.
        RoutingMode::Bypass(_) | RoutingMode::Only(_) => {}
    }

    let final_outbound = match routing_mode {
        RoutingMode::Only(_) => "direct",
        _ => "proxy",
    };

    // `default_mark` tags every packet sing-box sends to the network with a
    // Linux fwmark. The kvn-tui kill switch's nft ruleset allowlists this mark,
    // so traffic from sing-box's `direct` outbound (used by Bypass/Only routing
    // modes) can reach the physical interface while everything else is dropped.
    let route = json!({
        "default_domain_resolver": {
            "server": dns.final_server,
            "strategy": dns.strategy.as_str(),
        },
        "rules": rules,
        "auto_detect_interface": true,
        "default_mark": 666,
        "final": final_outbound
    });

    (route, rule_sets)
}

/// Build the proxy outbound list for a profile. Most protocols return a
/// single outbound tagged `proxy`; ShadowTLS returns two (the wrapper plus
/// an inner Shadowsocks detour, with the SS half tagged `proxy`).
fn build_outbound(profile: &Profile) -> anyhow::Result<Vec<Value>> {
    let outbounds = match &profile.config {
        ProtocolConfig::Vless(cfg) => vec![build_vless_outbound(profile, cfg)?],
        ProtocolConfig::Vmess(cfg) => vec![build_vmess_outbound(profile, cfg)?],
        ProtocolConfig::Trojan(cfg) => vec![build_trojan_outbound(profile, cfg)?],
        ProtocolConfig::Shadowsocks(cfg) => vec![build_shadowsocks_outbound(profile, cfg)?],
        ProtocolConfig::Hysteria2(cfg) => vec![build_hysteria2_outbound(profile, cfg)?],
        ProtocolConfig::Tuic(cfg) => vec![build_tuic_outbound(profile, cfg)?],
        ProtocolConfig::Shadowtls(cfg) => build_shadowtls_outbounds(profile, cfg)?,
        ProtocolConfig::Anytls(cfg) => vec![build_anytls_outbound(profile, cfg)?],
        ProtocolConfig::Socks(cfg) => vec![build_socks_outbound(profile, cfg)?],
        ProtocolConfig::Http(cfg) => vec![build_http_outbound(profile, cfg)?],
        ProtocolConfig::Ssh(cfg) => vec![build_ssh_outbound(profile, cfg)?],
    };
    Ok(outbounds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::profile::{
        DnsRule, DnsStrategy, GeoRegion, GeoRouting, Profile, ProtocolConfig, RealitySettings,
        ShadowtlsVersion, TransportType, VlessConfig,
    };

    fn test_profile() -> Profile {
        let mut p = Profile::new_vless(
            "Example".to_string(),
            "203.0.113.42".to_string(),
            59431,
            "671c62c7-6768-4b98-ac6b-572c9c707be0".to_string(),
        );
        if let ProtocolConfig::Vless(ref mut cfg) = p.config {
            cfg.security = Some(crate::config::profile::Security::Reality);
            cfg.tls.reality = Some(RealitySettings {
                public_key: "0IO3LodsrMnhOWh4ogwgdVqYg30CS5-snhFMwldOuAQ".to_string(),
                short_id: "f04debc34cbc48a4".to_string(),
                server_name: "google.com".to_string(),
                spider_x: "/".to_string(),
            });
            cfg.transport_type = Some(TransportType::Grpc);
            cfg.tls.utls_fingerprint = Some("chrome".to_string());
        }
        p
    }

    fn vless_cfg_mut(profile: &mut Profile) -> &mut VlessConfig {
        match &mut profile.config {
            ProtocolConfig::Vless(c) => c,
            _ => panic!("expected VLESS"),
        }
    }

    fn vless_cfg(profile: &Profile) -> &VlessConfig {
        match &profile.config {
            ProtocolConfig::Vless(c) => c,
            _ => panic!("expected VLESS"),
        }
    }

    #[test]
    fn generated_config_has_required_keys() {
        let profile = test_profile();
        let settings = Settings::default();
        let config = generate_config(&profile, &settings, &GeoAvailability::all()).unwrap();

        assert!(config.get("log").is_some());
        assert!(config.get("dns").is_some());
        assert!(config.get("inbounds").is_some());
        assert!(config.get("outbounds").is_some());
        assert!(config.get("route").is_some());
        assert!(config.get("experimental").is_some());
    }

    #[test]
    fn generated_config_log_level_defaults_to_info() {
        let profile = test_profile();
        let settings = Settings::default();
        let config = generate_config(&profile, &settings, &GeoAvailability::all()).unwrap();
        assert_eq!(config["log"]["level"].as_str(), Some("info"));
    }

    #[test]
    fn generated_config_log_level_follows_settings() {
        let profile = test_profile();
        let settings = Settings {
            log_level: "debug".to_string(),
            ..Settings::default()
        };
        let config = generate_config(&profile, &settings, &GeoAvailability::all()).unwrap();
        assert_eq!(config["log"]["level"].as_str(), Some("debug"));
    }

    #[test]
    fn generated_config_log_level_falls_back_on_garbage() {
        let profile = test_profile();
        let settings = Settings {
            log_level: "verbose".to_string(),
            ..Settings::default()
        };
        let config = generate_config(&profile, &settings, &GeoAvailability::all()).unwrap();
        assert_eq!(config["log"]["level"].as_str(), Some("info"));
    }

    #[test]
    fn generated_config_enables_clash_api() {
        let profile = test_profile();
        let settings = Settings::default();
        let config = generate_config(&profile, &settings, &GeoAvailability::all()).unwrap();
        assert_eq!(
            config["experimental"]["clash_api"]["external_controller"], "127.0.0.1:9090",
            "clash_api must be enabled so the TUI can poll traffic stats"
        );
    }

    #[test]
    fn generated_config_global_final_is_proxy() {
        let profile = test_profile();
        let settings = Settings::default();
        let config = generate_config(&profile, &settings, &GeoAvailability::all()).unwrap();
        let route = config.get("route").unwrap();
        assert_eq!(route["final"].as_str().unwrap(), "proxy");
    }

    #[test]
    fn generated_config_only_ru_final_is_direct() {
        let profile = test_profile();
        let mut geo_routing = GeoRouting::default();
        geo_routing.set_region(GeoRegion::Ru);
        geo_routing.set_mode(RoutingMode::Only(GeoRegion::Ru));
        let settings = Settings {
            geo_routing,
            ..Default::default()
        };
        let config = generate_config(&profile, &settings, &GeoAvailability::all()).unwrap();
        let route = config.get("route").unwrap();
        assert_eq!(route["final"].as_str().unwrap(), "direct");
    }

    #[test]
    fn vless_outbound_with_reality() {
        let profile = test_profile();
        let outbound = build_vless_outbound(&profile, vless_cfg(&profile)).unwrap();

        assert_eq!(outbound["type"], "vless");
        assert_eq!(outbound["tag"], "proxy");
        assert_eq!(outbound["server"], "203.0.113.42");
        assert_eq!(outbound["server_port"], 59431);
        assert_eq!(outbound["uuid"], "671c62c7-6768-4b98-ac6b-572c9c707be0");

        let tls = &outbound["tls"];
        assert_eq!(tls["enabled"], true);
        assert_eq!(tls["server_name"], "google.com");
        assert!(tls.get("reality").is_some());
        assert_eq!(
            tls["reality"]["public_key"],
            "0IO3LodsrMnhOWh4ogwgdVqYg30CS5-snhFMwldOuAQ"
        );
        assert_eq!(tls["reality"]["short_id"], "f04debc34cbc48a4");
        assert_eq!(tls["utls"]["enabled"], true);
        assert_eq!(tls["utls"]["fingerprint"], "chrome");
    }

    #[test]
    fn vless_outbound_without_reality() {
        let profile = Profile::new_vless(
            "Simple".to_string(),
            "1.2.3.4".to_string(),
            443,
            "uuid".to_string(),
        );
        let outbound = build_vless_outbound(&profile, vless_cfg(&profile)).unwrap();

        let tls = &outbound["tls"];
        assert_eq!(tls["enabled"], true);
        assert_eq!(tls["server_name"], "1.2.3.4");
        // sing-box treats absent `insecure` as false; we now omit it when
        // unset (shared shape with VMess/Trojan via `build_tls_block`).
        assert!(
            tls.get("insecure")
                .is_none_or(|v| v.as_bool() == Some(false))
        );
        assert!(tls.get("reality").is_none());
    }

    #[test]
    fn vless_outbound_plain_tls_uses_custom_sni() {
        // P0 regression: a profile with cfg.tls.server_name = Some("cdn...")
        // must emit `tls.server_name == "cdn..."`, not profile.address.
        let mut profile = Profile::new_vless(
            "CDN".to_string(),
            "1.2.3.4".to_string(),
            443,
            "uuid".to_string(),
        );
        vless_cfg_mut(&mut profile).tls.server_name = Some("cdn.example.com".to_string());
        vless_cfg_mut(&mut profile).tls.alpn = vec!["h2".to_string()];
        vless_cfg_mut(&mut profile).tls.insecure = true;
        let outbound = build_vless_outbound(&profile, vless_cfg(&profile)).unwrap();
        let tls = &outbound["tls"];
        assert_eq!(tls["server_name"], "cdn.example.com");
        assert_eq!(tls["alpn"], serde_json::json!(["h2"]));
        assert_eq!(tls["insecure"], true);
    }

    #[test]
    fn vless_outbound_with_flow() {
        let mut profile = test_profile();
        vless_cfg_mut(&mut profile).flow = Some(crate::config::profile::Flow::XtlsRprxVision);
        let outbound = build_vless_outbound(&profile, vless_cfg(&profile)).unwrap();
        assert_eq!(outbound["flow"], "xtls-rprx-vision");
    }

    #[test]
    fn vless_outbound_with_grpc_transport() {
        let profile = test_profile();
        let outbound = build_vless_outbound(&profile, vless_cfg(&profile)).unwrap();

        assert!(outbound.get("transport").is_some());
        let transport = &outbound["transport"];
        assert_eq!(transport["type"], "grpc");
        assert_eq!(transport["idle_timeout"], "15s");
        assert_eq!(transport["ping_timeout"], "15s");
    }

    #[test]
    fn vless_outbound_with_grpc_service_name() {
        let mut profile = test_profile();
        vless_cfg_mut(&mut profile).transport_service_name = Some("my-service".to_string());
        let outbound = build_vless_outbound(&profile, vless_cfg(&profile)).unwrap();
        assert_eq!(outbound["transport"]["service_name"], "my-service");
    }

    #[test]
    fn vless_outbound_without_transport() {
        let mut profile = test_profile();
        vless_cfg_mut(&mut profile).transport_type = None;
        let outbound = build_vless_outbound(&profile, vless_cfg(&profile)).unwrap();
        assert!(outbound.get("transport").is_none());
    }

    fn profile_with(config: ProtocolConfig, address: &str, port: u16) -> Profile {
        Profile {
            id: uuid::Uuid::nil(),
            name: "T".into(),
            address: address.into(),
            port,
            config,
            tags: Vec::new(),
            subscription_id: None,
        }
    }

    fn build_one(profile: &Profile) -> Value {
        let mut outs = build_outbound(profile).unwrap();
        assert_eq!(outs.len(), 1, "expected a single outbound");
        outs.remove(0)
    }

    #[test]
    fn vmess_outbound_basic_shape() {
        use crate::config::profile::{VmessConfig, VmessSecurity};
        let profile = profile_with(
            ProtocolConfig::Vmess(VmessConfig {
                uuid: "vm-uuid".into(),
                alter_id: 0,
                security: VmessSecurity::Aes128Gcm,
                ..Default::default()
            }),
            "1.1.1.1",
            443,
        );
        let outbound = build_one(&profile);
        assert_eq!(outbound["type"], "vmess");
        assert_eq!(outbound["uuid"], "vm-uuid");
        assert_eq!(outbound["security"], "aes-128-gcm");
        assert_eq!(outbound["alter_id"], 0);
        assert_eq!(outbound["packet_encoding"], "xudp");
        assert!(outbound.get("tls").is_some());
        // Sing-box 1.12 forbids the legacy stream cipher.
        assert_ne!(outbound["security"], "aes-128-cfb");
    }

    #[test]
    fn vmess_outbound_with_ws_transport() {
        use crate::config::profile::{TransportConfig, TransportType, VmessConfig};
        let profile = profile_with(
            ProtocolConfig::Vmess(VmessConfig {
                uuid: "u".into(),
                transport: Some(TransportConfig {
                    kind: TransportType::Ws,
                    path: Some("/ws".into()),
                    host: Some("example.com".into()),
                    service_name: None,
                    headers: Default::default(),
                }),
                ..Default::default()
            }),
            "1.1.1.1",
            443,
        );
        let outbound = build_one(&profile);
        assert_eq!(outbound["transport"]["type"], "ws");
        assert_eq!(outbound["transport"]["path"], "/ws");
        assert_eq!(outbound["transport"]["host"], "example.com");
    }

    #[test]
    fn vmess_tls_emits_ech_when_enabled() {
        use crate::config::profile::{EchSettings, TlsCommon, VmessConfig};
        let profile = profile_with(
            ProtocolConfig::Vmess(VmessConfig {
                uuid: "u".into(),
                tls: TlsCommon {
                    ech: Some(EchSettings {
                        enabled: true,
                        config: vec!["base64-blob".into()],
                    }),
                    ..Default::default()
                },
                ..Default::default()
            }),
            "ech.example.com",
            443,
        );
        let outbound = build_one(&profile);
        let ech = &outbound["tls"]["ech"];
        assert_eq!(ech["enabled"], true);
        assert_eq!(ech["config"][0], "base64-blob");
    }

    #[test]
    fn trojan_outbound_shape() {
        use crate::config::profile::TrojanConfig;
        let profile = profile_with(
            ProtocolConfig::Trojan(TrojanConfig {
                password: "secret".into(),
                ..Default::default()
            }),
            "trojan.example",
            443,
        );
        let outbound = build_one(&profile);
        assert_eq!(outbound["type"], "trojan");
        assert_eq!(outbound["password"], "secret");
        assert_eq!(outbound["tls"]["enabled"], true);
        assert_eq!(outbound["tls"]["server_name"], "trojan.example");
    }

    #[test]
    fn shadowsocks_outbound_uses_aead_cipher() {
        use crate::config::profile::{ShadowsocksCipher, ShadowsocksConfig};
        let profile = profile_with(
            ProtocolConfig::Shadowsocks(ShadowsocksConfig {
                method: ShadowsocksCipher::Blake3Aes256Gcm,
                password: "ss-pass".into(),
            }),
            "ss.example",
            8388,
        );
        let outbound = build_one(&profile);
        assert_eq!(outbound["type"], "shadowsocks");
        assert_eq!(outbound["method"], "2022-blake3-aes-256-gcm");
        assert_eq!(outbound["password"], "ss-pass");
        assert!(
            outbound.get("tls").is_none(),
            "Shadowsocks must not carry a tls block"
        );
    }

    #[test]
    fn hysteria2_outbound_shape() {
        use crate::config::profile::{Hysteria2Config, Hysteria2Obfs, Hysteria2ObfsType};
        let profile = profile_with(
            ProtocolConfig::Hysteria2(Hysteria2Config {
                password: "hy2-pass".into(),
                up_mbps: Some(100),
                down_mbps: Some(200),
                obfs: Some(Hysteria2Obfs {
                    kind: Hysteria2ObfsType::Salamander,
                    password: "obfs-pass".into(),
                }),
                ..Default::default()
            }),
            "hy2.example",
            443,
        );
        let outbound = build_one(&profile);
        assert_eq!(outbound["type"], "hysteria2");
        assert_eq!(outbound["password"], "hy2-pass");
        assert_eq!(outbound["up_mbps"], 100);
        assert_eq!(outbound["down_mbps"], 200);
        assert_eq!(outbound["obfs"]["type"], "salamander");
        assert_eq!(outbound["obfs"]["password"], "obfs-pass");
        // The legacy top-level obfs_password key must not appear.
        assert!(outbound.get("obfs_password").is_none());
        // Hysteria2 is QUIC-based — ALPN defaults to h3 when not set.
        assert_eq!(outbound["tls"]["alpn"][0], "h3");
    }

    #[test]
    fn tuic_outbound_shape() {
        use crate::config::profile::{TuicConfig, TuicCongestion, TuicUdpRelayMode};
        let profile = profile_with(
            ProtocolConfig::Tuic(TuicConfig {
                uuid: "tuic-uuid".into(),
                password: "tuic-pass".into(),
                congestion_control: TuicCongestion::Bbr,
                udp_relay_mode: TuicUdpRelayMode::Native,
                zero_rtt_handshake: true,
                ..Default::default()
            }),
            "tuic.example",
            443,
        );
        let outbound = build_one(&profile);
        assert_eq!(outbound["type"], "tuic");
        assert_eq!(outbound["uuid"], "tuic-uuid");
        assert_eq!(outbound["password"], "tuic-pass");
        assert_eq!(outbound["congestion_control"], "bbr");
        assert_eq!(outbound["udp_relay_mode"], "native");
        assert_eq!(outbound["zero_rtt_handshake"], true);
        assert_eq!(outbound["tls"]["alpn"][0], "h3");
    }

    #[test]
    fn shadowtls_emits_wrapper_and_detour_pair() {
        use crate::config::profile::{ShadowsocksCipher, ShadowtlsConfig};
        let profile = profile_with(
            ProtocolConfig::Shadowtls(ShadowtlsConfig {
                version: ShadowtlsVersion::V3,
                password: "st-pass".into(),
                method: ShadowsocksCipher::Chacha20IetfPoly1305,
                ss_password: "inner-ss".into(),
                ..Default::default()
            }),
            "st.example",
            443,
        );
        let outs = build_outbound(&profile).unwrap();
        assert_eq!(outs.len(), 2, "ShadowTLS emits wrapper + detour");
        let wrap = outs.iter().find(|o| o["type"] == "shadowtls").unwrap();
        let inner = outs.iter().find(|o| o["type"] == "shadowsocks").unwrap();
        assert_eq!(wrap["version"], 3);
        assert_eq!(wrap["password"], "st-pass");
        assert_eq!(inner["tag"], "proxy");
        assert_eq!(inner["server"], "st.example");
        assert_eq!(inner["server_port"], 443);
        assert_eq!(inner["password"], "inner-ss");
        assert_eq!(inner["detour"], wrap["tag"]);
    }

    #[test]
    fn shadowtls_v1_omits_password() {
        use crate::config::profile::{ShadowsocksCipher, ShadowtlsConfig};
        let profile = profile_with(
            ProtocolConfig::Shadowtls(ShadowtlsConfig {
                version: ShadowtlsVersion::V1,
                password: "ignored".into(),
                method: ShadowsocksCipher::Chacha20IetfPoly1305,
                ss_password: "inner-ss".into(),
                ..Default::default()
            }),
            "st.example",
            443,
        );
        let outs = build_outbound(&profile).unwrap();
        let wrap = outs.iter().find(|o| o["type"] == "shadowtls").unwrap();
        assert_eq!(wrap["version"], 1);
        assert!(wrap.get("password").is_none(), "v1 must not emit password");
    }

    #[test]
    fn anytls_outbound_shape() {
        use crate::config::profile::AnytlsConfig;
        let profile = profile_with(
            ProtocolConfig::Anytls(AnytlsConfig {
                password: "anytls-pass".into(),
                idle_session_timeout: Some("30s".into()),
                ..Default::default()
            }),
            "anytls.example",
            443,
        );
        let outbound = build_one(&profile);
        assert_eq!(outbound["type"], "anytls");
        assert_eq!(outbound["password"], "anytls-pass");
        assert_eq!(outbound["idle_session_timeout"], "30s");
        assert_eq!(outbound["tls"]["enabled"], true);
    }

    #[test]
    fn socks_outbound_with_auth() {
        use crate::config::profile::{SocksConfig, SocksVersion};
        let profile = profile_with(
            ProtocolConfig::Socks(SocksConfig {
                version: SocksVersion::V5,
                username: Some("u".into()),
                password: Some("p".into()),
            }),
            "socks.example",
            1080,
        );
        let outbound = build_one(&profile);
        assert_eq!(outbound["type"], "socks");
        assert_eq!(outbound["version"], "5");
        assert_eq!(outbound["username"], "u");
        assert_eq!(outbound["password"], "p");
        assert!(outbound.get("tls").is_none());
    }

    #[test]
    fn http_outbound_emits_tls_only_when_user_opts_in() {
        use crate::config::profile::{HttpConfig, TlsCommon};
        let plain = profile_with(
            ProtocolConfig::Http(HttpConfig::default()),
            "http.example",
            8080,
        );
        let plain_out = build_one(&plain);
        assert!(plain_out.get("tls").is_none(), "plain HTTP omits TLS");

        let secure = profile_with(
            ProtocolConfig::Http(HttpConfig {
                tls: TlsCommon {
                    server_name: Some("proxy.example".into()),
                    ..Default::default()
                },
                ..Default::default()
            }),
            "http.example",
            8443,
        );
        let secure_out = build_one(&secure);
        assert_eq!(secure_out["tls"]["enabled"], true);
        assert_eq!(secure_out["tls"]["server_name"], "proxy.example");
    }

    #[test]
    fn ssh_outbound_password_and_key() {
        use crate::config::profile::SshConfig;
        let profile = profile_with(
            ProtocolConfig::Ssh(SshConfig {
                user: "alice".into(),
                password: Some("p".into()),
                private_key_path: Some("/keys/id_ed25519".into()),
                host_key_algorithms: vec!["ssh-ed25519".into()],
                ..Default::default()
            }),
            "ssh.example",
            22,
        );
        let outbound = build_one(&profile);
        assert_eq!(outbound["type"], "ssh");
        assert_eq!(outbound["user"], "alice");
        assert_eq!(outbound["password"], "p");
        assert_eq!(outbound["private_key_path"], "/keys/id_ed25519");
        assert_eq!(outbound["host_key_algorithms"][0], "ssh-ed25519");
    }

    #[test]
    fn generated_config_includes_direct_outbound() {
        let profile = test_profile();
        let settings = Settings::default();
        let config = generate_config(&profile, &settings, &GeoAvailability::all()).unwrap();
        let outbounds = config["outbounds"].as_array().unwrap();
        assert!(outbounds.iter().any(|o| o["type"] == "direct"));
        assert!(outbounds.iter().any(|o| o["tag"] == "proxy"));
    }

    fn dns_default() -> DnsConfig {
        DnsConfig::default()
    }

    #[test]
    fn route_has_default_mark_for_killswitch() {
        let (route, _) = build_route(
            &RoutingMode::Global,
            &dns_default(),
            &GeoAvailability::all(),
            &no_services(),
        );
        assert_eq!(
            route["default_mark"].as_u64(),
            Some(666),
            "default_mark must match the kill-switch nft rule (0x29a)"
        );
    }

    #[test]
    fn build_route_global_has_basic_rules() {
        let mut dns = dns_default();
        dns.strategy = DnsStrategy::OnlyIpv4;
        let (route, rule_sets) = build_route(
            &RoutingMode::Global,
            &dns,
            &GeoAvailability::all(),
            &no_services(),
        );
        assert!(rule_sets.is_empty());
        let rules = route["rules"].as_array().unwrap();
        assert_eq!(rules.len(), 3); // ipv6 reject, dns hijack, direct cidr
        assert_eq!(route["final"], "proxy");
        assert_eq!(route["default_domain_resolver"]["strategy"], "ipv4_only");
        assert_eq!(route["default_domain_resolver"]["server"], "remote");
    }

    #[test]
    fn build_route_only_ru_has_private_rule_and_final_direct() {
        let (route, _rule_sets) = build_route(
            &RoutingMode::Only(GeoRegion::Ru),
            &dns_default(),
            &GeoAvailability::all(),
            &no_services(),
        );
        let rules = route["rules"].as_array().unwrap();
        assert!(rules.len() >= 4); // basic 3 + ip_is_private
        assert_eq!(route["final"], "direct");
    }

    #[test]
    fn generated_config_only_cn_final_is_direct() {
        let profile = test_profile();
        let mut geo_routing = GeoRouting::default();
        geo_routing.set_region(GeoRegion::Cn);
        geo_routing.set_mode(RoutingMode::Only(GeoRegion::Cn));
        let settings = Settings {
            geo_routing,
            ..Default::default()
        };
        let config = generate_config(&profile, &settings, &GeoAvailability::all()).unwrap();
        let route = config.get("route").unwrap();
        assert_eq!(route["final"].as_str().unwrap(), "direct");
    }

    #[test]
    fn build_route_bypass_cn_has_private_rule() {
        let (route, _rule_sets) = build_route(
            &RoutingMode::Bypass(GeoRegion::Cn),
            &dns_default(),
            &GeoAvailability::all(),
            &no_services(),
        );
        let rules = route["rules"].as_array().unwrap();
        assert!(rules.len() >= 4); // basic 3 + ip_is_private
        assert_eq!(route["final"], "proxy");
    }

    #[test]
    fn build_route_only_cn_has_private_rule_and_final_direct() {
        let (route, _rule_sets) = build_route(
            &RoutingMode::Only(GeoRegion::Cn),
            &dns_default(),
            &GeoAvailability::all(),
            &no_services(),
        );
        let rules = route["rules"].as_array().unwrap();
        assert!(rules.len() >= 4); // basic 3 + ip_is_private
        assert_eq!(route["final"], "direct");
    }

    #[test]
    fn default_dns_block_matches_legacy_layout() {
        let dns = build_dns(&dns_default());
        let servers = dns["servers"].as_array().unwrap();
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[0]["tag"], "local");
        assert_eq!(servers[0]["type"], "local");
        assert_eq!(servers[1]["tag"], "remote");
        assert_eq!(servers[1]["type"], "https");
        assert_eq!(servers[1]["server"], "1.1.1.1");
        assert_eq!(servers[1]["path"], "/dns-query");
        assert!(servers[1].get("server_port").is_none());
        assert_eq!(dns["final"], "remote");
        assert_eq!(dns["strategy"], "prefer_ipv4");
        assert!(dns.get("rules").is_none());
        assert!(dns.get("fakeip").is_none());
    }

    #[test]
    fn build_dns_emits_dot_server_with_port() {
        let dns = DnsConfig {
            servers: vec![
                DnsServer::Local {
                    tag: "local".to_string(),
                },
                DnsServer::Tls {
                    tag: "google-dot".to_string(),
                    server: "8.8.8.8".to_string(),
                    server_port: Some(853),
                },
            ],
            rules: Vec::new(),
            final_server: "google-dot".to_string(),
            strategy: DnsStrategy::PreferIpv4,
            fakeip_enabled: false,
        };
        let block = build_dns(&dns);
        let s = &block["servers"].as_array().unwrap()[1];
        assert_eq!(s["type"], "tls");
        assert_eq!(s["server"], "8.8.8.8");
        assert_eq!(s["server_port"], 853);
        assert_eq!(block["final"], "google-dot");
    }

    #[test]
    fn build_dns_emits_fakeip_server_and_auto_rule_when_enabled() {
        let dns = DnsConfig {
            servers: vec![
                DnsServer::Local {
                    tag: "local".to_string(),
                },
                DnsServer::FakeIp {
                    tag: "fake".to_string(),
                    inet4_range: "198.18.0.0/15".to_string(),
                    inet6_range: "fc00::/18".to_string(),
                },
            ],
            rules: Vec::new(),
            final_server: "local".to_string(),
            strategy: DnsStrategy::PreferIpv4,
            fakeip_enabled: true,
        };
        let block = build_dns(&dns);
        // Sing-box 1.12 no longer accepts the top-level `dns.fakeip` block;
        // the ranges live inside the server entry instead.
        assert!(block.get("fakeip").is_none());
        let fake_server = block["servers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["type"] == "fakeip")
            .unwrap();
        assert_eq!(fake_server["tag"], "fake");
        assert_eq!(fake_server["inet4_range"], "198.18.0.0/15");
        assert_eq!(fake_server["inet6_range"], "fc00::/18");

        let rules = block["rules"].as_array().unwrap();
        assert_eq!(
            rules.len(),
            1,
            "auto-rule must be injected when fakeip is on"
        );
        assert_eq!(rules[0]["server"], "fake");
        assert_eq!(rules[0]["query_type"][0], "A");
        assert_eq!(rules[0]["query_type"][1], "AAAA");

        assert_eq!(block["independent_cache"], true);
    }

    #[test]
    fn build_dns_does_not_duplicate_fakeip_rule_when_user_added_one() {
        let dns = DnsConfig {
            servers: vec![
                DnsServer::Local {
                    tag: "local".to_string(),
                },
                DnsServer::FakeIp {
                    tag: "fake".to_string(),
                    inet4_range: "198.18.0.0/15".to_string(),
                    inet6_range: "fc00::/18".to_string(),
                },
            ],
            rules: vec![DnsRule {
                domain_suffix: vec!["example.com".to_string()],
                server: "fake".to_string(),
                ..Default::default()
            }],
            final_server: "local".to_string(),
            strategy: DnsStrategy::PreferIpv4,
            fakeip_enabled: true,
        };
        let block = build_dns(&dns);
        let rules = block["rules"].as_array().unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0]["domain_suffix"][0], "example.com");
        assert_eq!(rules[0]["server"], "fake");
    }

    #[test]
    fn generated_config_sets_store_fakeip_when_enabled() {
        let profile = test_profile();
        let settings = Settings {
            dns: DnsConfig {
                servers: vec![
                    DnsServer::Local {
                        tag: "local".to_string(),
                    },
                    DnsServer::FakeIp {
                        tag: "fake".to_string(),
                        inet4_range: "198.18.0.0/15".to_string(),
                        inet6_range: "fc00::/18".to_string(),
                    },
                ],
                rules: Vec::new(),
                final_server: "local".to_string(),
                strategy: DnsStrategy::PreferIpv4,
                fakeip_enabled: true,
            },
            ..Settings::default()
        };
        let config = generate_config(&profile, &settings, &GeoAvailability::all()).unwrap();
        assert_eq!(
            config["experimental"]["cache_file"]["store_fakeip"], true,
            "store_fakeip must persist the v4/v6→domain map across restarts"
        );
    }

    #[test]
    fn generated_config_omits_store_fakeip_when_disabled() {
        let profile = test_profile();
        let settings = Settings::default();
        let config = generate_config(&profile, &settings, &GeoAvailability::all()).unwrap();
        assert!(
            config["experimental"]["cache_file"]
                .get("store_fakeip")
                .is_none()
        );
    }

    #[test]
    fn build_dns_emits_rules_with_domain_suffix() {
        let dns = DnsConfig {
            servers: vec![
                DnsServer::Local {
                    tag: "local".to_string(),
                },
                DnsServer::Https {
                    tag: "remote".to_string(),
                    server: "1.1.1.1".to_string(),
                    server_port: None,
                    path: "/dns-query".to_string(),
                },
            ],
            rules: vec![DnsRule {
                domain_suffix: vec!["example.com".to_string()],
                server: "local".to_string(),
                ..Default::default()
            }],
            final_server: "remote".to_string(),
            strategy: DnsStrategy::PreferIpv4,
            fakeip_enabled: false,
        };
        let block = build_dns(&dns);
        let rules = block["rules"].as_array().unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0]["domain_suffix"][0], "example.com");
        assert_eq!(rules[0]["server"], "local");
        assert!(rules[0].get("disable_cache").is_none());
    }

    #[test]
    fn generated_config_uses_custom_final_server() {
        let profile = test_profile();
        let settings = Settings {
            dns: DnsConfig {
                servers: vec![
                    DnsServer::Local {
                        tag: "local".to_string(),
                    },
                    DnsServer::Https {
                        tag: "quad9".to_string(),
                        server: "9.9.9.9".to_string(),
                        server_port: None,
                        path: "/dns-query".to_string(),
                    },
                ],
                rules: Vec::new(),
                final_server: "quad9".to_string(),
                strategy: DnsStrategy::PreferIpv4,
                fakeip_enabled: false,
            },
            ..Settings::default()
        };
        let config = generate_config(&profile, &settings, &GeoAvailability::all()).unwrap();
        assert_eq!(config["dns"]["final"], "quad9");
        assert_eq!(
            config["route"]["default_domain_resolver"]["server"],
            "quad9"
        );
    }

    // ---- Iran routing modes (previously only Ru/Cn covered) ----

    #[test]
    fn build_route_bypass_ir_has_private_rule_and_final_proxy() {
        let (route, rule_sets) = build_route(
            &RoutingMode::Bypass(GeoRegion::Ir),
            &dns_default(),
            &GeoAvailability::all(),
            &no_services(),
        );
        let rules = route["rules"].as_array().unwrap();
        assert!(rules.len() >= 4);
        assert_eq!(route["final"], "proxy");
        // BypassIr with geo data must register both geoip-ir and geosite-category-ir.
        let tags: Vec<&str> = rule_sets
            .iter()
            .map(|rs| rs["tag"].as_str().unwrap())
            .collect();
        assert!(tags.contains(&"geoip-ir"));
        assert!(tags.contains(&"geosite-category-ir"));
    }

    #[test]
    fn build_route_only_ir_has_private_rule_and_final_direct() {
        let (route, rule_sets) = build_route(
            &RoutingMode::Only(GeoRegion::Ir),
            &dns_default(),
            &GeoAvailability::all(),
            &no_services(),
        );
        let rules = route["rules"].as_array().unwrap();
        assert!(rules.len() >= 4);
        assert_eq!(route["final"], "direct");
        assert_eq!(rule_sets.len(), 2);
    }

    #[test]
    fn build_route_bypass_ru_has_private_rule_and_final_proxy() {
        // Only the OnlyRu variant was tested explicitly; Bypass had no direct test.
        let (route, rule_sets) = build_route(
            &RoutingMode::Bypass(GeoRegion::Ru),
            &dns_default(),
            &GeoAvailability::all(),
            &no_services(),
        );
        assert_eq!(route["final"], "proxy");
        assert_eq!(rule_sets.len(), 2);
    }

    // ---- Geo availability missing: rule_sets must stay empty ----

    #[test]
    fn build_route_bypass_ru_without_geo_emits_no_rule_sets() {
        let (_route, rule_sets) = build_route(
            &RoutingMode::Bypass(GeoRegion::Ru),
            &dns_default(),
            &GeoAvailability::default(),
            &no_services(),
        );
        assert!(rule_sets.is_empty());
    }

    #[test]
    fn build_route_only_cn_without_geo_emits_no_rule_sets() {
        let (_route, rule_sets) = build_route(
            &RoutingMode::Only(GeoRegion::Cn),
            &dns_default(),
            &GeoAvailability::default(),
            &no_services(),
        );
        assert!(rule_sets.is_empty());
    }

    #[test]
    fn build_route_bypass_ir_without_geo_emits_no_rule_sets() {
        let (_route, rule_sets) = build_route(
            &RoutingMode::Bypass(GeoRegion::Ir),
            &dns_default(),
            &GeoAvailability::default(),
            &no_services(),
        );
        assert!(rule_sets.is_empty());
    }

    #[test]
    fn build_route_only_ir_without_geo_emits_no_rule_sets() {
        let (_route, rule_sets) = build_route(
            &RoutingMode::Only(GeoRegion::Ir),
            &dns_default(),
            &GeoAvailability::default(),
            &no_services(),
        );
        assert!(rule_sets.is_empty());
    }

    // ---- Per-service routing overrides ----

    fn no_services() -> HashMap<RoutedService, ServiceRoute> {
        HashMap::new()
    }

    fn routes(entries: &[(RoutedService, ServiceRoute)]) -> HashMap<RoutedService, ServiceRoute> {
        entries.iter().copied().collect()
    }

    #[test]
    fn build_route_service_direct_adds_rule_and_rule_sets_in_global() {
        let (route, rule_sets) = build_route(
            &RoutingMode::Global,
            &dns_default(),
            &GeoAvailability::all(),
            &routes(&[(RoutedService::Steam, ServiceRoute::Direct)]),
        );
        let rules = route["rules"].as_array().unwrap();
        let steam_rule = rules
            .iter()
            .find(|r| r["rule_set"] == json!(["geosite-steam", "geoip-steam"]))
            .expect("steam rule present");
        assert_eq!(steam_rule["outbound"], "direct");
        let tags: Vec<&str> = rule_sets.iter().filter_map(|r| r["tag"].as_str()).collect();
        assert_eq!(tags, ["geosite-steam", "geoip-steam"]);
        assert!(
            rule_sets
                .iter()
                .all(|r| r["type"] == "local" && r["format"] == "binary")
        );
    }

    #[test]
    fn build_route_service_proxy_uses_proxy_outbound() {
        let (route, rule_sets) = build_route(
            &RoutingMode::Bypass(GeoRegion::Ru),
            &dns_default(),
            &GeoAvailability::all(),
            &routes(&[(RoutedService::Telegram, ServiceRoute::Proxy)]),
        );
        let rules = route["rules"].as_array().unwrap();
        let tg_rule = rules
            .iter()
            .find(|r| r["rule_set"] == json!(["geosite-telegram", "geoip-telegram"]))
            .expect("telegram rule present");
        assert_eq!(tg_rule["outbound"], "proxy");
        // Telegram's 2 rule-sets + the region's 2.
        assert_eq!(rule_sets.len(), 4);
    }

    #[test]
    fn build_route_service_rules_precede_geo_rules_under_only() {
        let (route, rule_sets) = build_route(
            &RoutingMode::Only(GeoRegion::Ru),
            &dns_default(),
            &GeoAvailability::all(),
            &routes(&[(RoutedService::Steam, ServiceRoute::Direct)]),
        );
        let rules = route["rules"].as_array().unwrap();
        let steam_idx = rules
            .iter()
            .position(|r| r["rule_set"] == json!(["geosite-steam", "geoip-steam"]))
            .unwrap();
        let geo_idx = rules
            .iter()
            .position(|r| r["rule_set"] == json!(["geosite-category-ru"]))
            .unwrap();
        assert!(
            steam_idx < geo_idx,
            "service overrides must win over the regional rules"
        );
        assert_eq!(rule_sets.len(), 4);
    }

    #[test]
    fn build_route_service_rules_follow_all_order() {
        // Same config must always yield the same rule order, regardless of
        // HashMap internals — services appear in RoutedService::ALL order.
        let (route, _) = build_route(
            &RoutingMode::Global,
            &dns_default(),
            &GeoAvailability::all(),
            &routes(&[
                (RoutedService::Telegram, ServiceRoute::Direct),
                (RoutedService::Steam, ServiceRoute::Direct),
            ]),
        );
        let rules = route["rules"].as_array().unwrap();
        let service_rules: Vec<&Value> = rules
            .iter()
            .filter(|r| r.get("rule_set").is_some())
            .collect();
        assert_eq!(service_rules.len(), 2);
        assert_eq!(
            service_rules[0]["rule_set"],
            json!(["geosite-steam", "geoip-steam"])
        );
        assert_eq!(
            service_rules[1]["rule_set"],
            json!(["geosite-telegram", "geoip-telegram"])
        );
    }

    #[test]
    fn build_route_disabled_services_emit_no_entries() {
        let (route, rule_sets) = build_route(
            &RoutingMode::Global,
            &dns_default(),
            &GeoAvailability::all(),
            &routes(&[(RoutedService::Steam, ServiceRoute::Disabled)]),
        );
        assert!(rule_sets.is_empty());
        assert!(
            route["rules"]
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r.get("rule_set").is_none())
        );
    }

    #[test]
    fn build_route_service_enabled_without_files_is_noop() {
        let (_route, rule_sets) = build_route(
            &RoutingMode::Global,
            &dns_default(),
            &GeoAvailability::default(),
            &routes(&[(RoutedService::Steam, ServiceRoute::Direct)]),
        );
        assert!(rule_sets.is_empty());
    }

    #[test]
    fn generated_config_includes_service_rule_sets_when_enabled() {
        let profile = test_profile();
        let mut settings = Settings::default();
        settings
            .geo_routing
            .service_routes
            .insert(RoutedService::Steam, ServiceRoute::Direct);
        let config = generate_config(&profile, &settings, &GeoAvailability::all()).unwrap();
        let tags: Vec<&str> = config["route"]["rule_set"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["tag"].as_str())
            .collect();
        assert!(tags.contains(&"geosite-steam"));
        assert!(tags.contains(&"geoip-steam"));
    }

    #[test]
    fn generated_config_omits_service_rule_sets_by_default() {
        let profile = test_profile();
        let settings = Settings::default();
        let config = generate_config(&profile, &settings, &GeoAvailability::all()).unwrap();
        assert!(config["route"].get("rule_set").is_none());
    }

    // ---- build_dns_server for every variant ----

    #[test]
    fn build_dns_server_emits_local_shape() {
        let v = build_dns_server(&DnsServer::Local {
            tag: "loc".to_string(),
        });
        assert_eq!(v["tag"], "loc");
        assert_eq!(v["type"], "local");
    }

    #[test]
    fn build_dns_server_emits_udp_and_tcp_with_optional_port() {
        let udp = build_dns_server(&DnsServer::Udp {
            tag: "u".to_string(),
            server: "1.1.1.1".to_string(),
            server_port: Some(53),
        });
        assert_eq!(udp["type"], "udp");
        assert_eq!(udp["server_port"], 53);
        let udp_no_port = build_dns_server(&DnsServer::Udp {
            tag: "u".to_string(),
            server: "1.1.1.1".to_string(),
            server_port: None,
        });
        assert!(udp_no_port.get("server_port").is_none());

        let tcp = build_dns_server(&DnsServer::Tcp {
            tag: "t".to_string(),
            server: "1.1.1.1".to_string(),
            server_port: None,
        });
        assert_eq!(tcp["type"], "tcp");
    }

    #[test]
    fn build_dns_server_emits_quic_shape() {
        let v = build_dns_server(&DnsServer::Quic {
            tag: "q".to_string(),
            server: "9.9.9.9".to_string(),
            server_port: Some(853),
        });
        assert_eq!(v["type"], "quic");
        assert_eq!(v["server_port"], 853);
    }

    #[test]
    fn build_dns_server_emits_fakeip_with_inet_ranges() {
        let v = build_dns_server(&DnsServer::FakeIp {
            tag: "fakeip".to_string(),
            inet4_range: "198.18.0.0/15".to_string(),
            inet6_range: "fc00::/18".to_string(),
        });
        assert_eq!(v["type"], "fakeip");
        assert_eq!(v["inet4_range"], "198.18.0.0/15");
        assert_eq!(v["inet6_range"], "fc00::/18");
    }

    // ---- build_dns_rule branches ----

    #[test]
    fn build_dns_rule_emits_only_populated_fields() {
        let rule = DnsRule {
            domain: vec!["a.example".to_string()],
            server: "remote".to_string(),
            ..DnsRule::default()
        };
        let v = build_dns_rule(&rule);
        assert!(v.get("domain").is_some());
        assert!(v.get("domain_suffix").is_none());
        assert!(v.get("domain_keyword").is_none());
        assert!(v.get("domain_regex").is_none());
        assert!(v.get("rule_set").is_none());
        assert_eq!(v["server"], "remote");
    }

    #[test]
    fn build_dns_rule_includes_domain_keyword_regex_and_ruleset() {
        let rule = DnsRule {
            domain_keyword: vec!["youtube".to_string()],
            domain_regex: vec!["^ads\\.".to_string()],
            rule_set: vec!["geosite-ads".to_string()],
            server: "remote".to_string(),
            ..DnsRule::default()
        };
        let v = build_dns_rule(&rule);
        assert_eq!(v["domain_keyword"], json!(["youtube"]));
        assert_eq!(v["domain_regex"], json!(["^ads\\."]));
        assert_eq!(v["rule_set"], json!(["geosite-ads"]));
    }

    #[test]
    fn build_dns_rule_emits_disable_cache_when_set() {
        let rule = DnsRule {
            domain: vec!["x.test".to_string()],
            server: "remote".to_string(),
            disable_cache: true,
            ..DnsRule::default()
        };
        let v = build_dns_rule(&rule);
        assert_eq!(v["disable_cache"], true);
    }

    #[test]
    fn generate_test_config_has_socks_inbound_on_given_port() {
        let profile = test_profile();
        let cfg = generate_test_config(&profile, 12345).unwrap();
        let inbounds = cfg["inbounds"].as_array().unwrap();
        assert_eq!(inbounds.len(), 1);
        assert_eq!(inbounds[0]["type"], "socks");
        assert_eq!(inbounds[0]["listen"], "127.0.0.1");
        assert_eq!(inbounds[0]["listen_port"], 12345);
    }

    #[test]
    fn generate_test_config_log_level_is_warn() {
        let profile = test_profile();
        let cfg = generate_test_config(&profile, 9999).unwrap();
        assert_eq!(cfg["log"]["level"], "warn");
    }

    #[test]
    fn generate_test_config_includes_profile_outbound() {
        let profile = test_profile();
        let cfg = generate_test_config(&profile, 9999).unwrap();
        let outbounds = cfg["outbounds"].as_array().unwrap();
        assert!(!outbounds.is_empty());
        // The primary outbound should have tag "proxy" (VLESS convention).
        assert!(outbounds.iter().any(|o| o["tag"] == "proxy"));
    }

    #[test]
    fn generate_test_config_has_route_to_proxy_and_no_dns() {
        let profile = test_profile();
        let cfg = generate_test_config(&profile, 9999).unwrap();
        assert_eq!(
            cfg["route"]["final"], "proxy",
            "route must send traffic to proxy outbound"
        );
        assert_eq!(
            cfg["route"]["default_mark"].as_u64(),
            Some(666),
            "default_mark must match killswitch nft rule (0x29a)"
        );
        assert_eq!(
            cfg["route"]["auto_detect_interface"].as_bool(),
            Some(true),
            "auto_detect_interface needed to bypass TUN via SO_BINDTODEVICE"
        );
        assert!(
            cfg.get("dns").is_none(),
            "no dns section needed in test config"
        );
    }
}
