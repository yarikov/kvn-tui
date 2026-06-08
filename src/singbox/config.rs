use serde_json::{Value, json};

use crate::config::profile::{DnsStrategy, Profile, RoutingMode, Settings, TransportType};

/// Generate a complete sing-box JSON configuration from a profile.
/// Uses the modern sing-box 1.12+ format.
pub fn generate_config(profile: &Profile, settings: &Settings) -> anyhow::Result<Value> {
    let outbound = build_outbound(profile)?;
    let (route, rule_sets) = build_route(&settings.routing_mode, settings.dns_strategy.clone());

    let mut config = json!({
        "log": {
            "level": "debug",
            "output": crate::infra::paths::singbox_log_path().to_string_lossy(),
            "timestamp": true
        },
        "dns": {
            "servers": [
                {
                    "tag": "local",
                    "type": "local"
                },
                {
                    "tag": "remote",
                    "type": "https",
                    "server": "1.1.1.1",
                    "path": "/dns-query"
                }
            ],
            "final": "remote",
            "strategy": settings.dns_strategy.clone()
        },
        "inbounds": [
            {
                "type": "tun",
                "tag": "tun-in",
                "interface_name": settings.tun_interface.clone(),
                "address": ["172.19.0.1/30"],
                "mtu": 1420,
                "auto_route": true,
                "strict_route": true,
                "endpoint_independent_nat": true,
                "stack": "gvisor"
            }
        ],
        "outbounds": [
            outbound,
            {
                "type": "direct",
                "tag": "direct"
            },
            {
                "type": "block",
                "tag": "block"
            }
        ],
        "route": route,
        "experimental": {
            "cache_file": {
                "enabled": true
            }
        }
    });

    // Merge rule_sets into route if any exist.
    if !rule_sets.is_empty() {
        config["route"]["rule_set"] = json!(rule_sets);
    }

    Ok(config)
}

/// Build route object and local rule-sets based on routing mode.
/// Returns (route_value, rule_sets_vec).
fn build_route(routing_mode: &RoutingMode, dns_strategy: DnsStrategy) -> (Value, Vec<Value>) {
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
            "ip_cidr": ["172.19.0.0/30"],
            "outbound": "direct"
        }),
    ];

    let mut rule_sets: Vec<Value> = Vec::new();

    match routing_mode {
        RoutingMode::Global => {}
        RoutingMode::BypassRu => {
            rules.push(json!({
                "ip_is_private": true,
                "outbound": "direct"
            }));
            if let Ok(geo) = crate::infra::geo::GeoManager::new() {
                let (geoip_ru, geosite_ru) = geo.local_paths();
                if geosite_ru.exists() {
                    rules.push(json!({
                        "rule_set": ["geosite-category-ru"],
                        "outbound": "direct"
                    }));
                    rule_sets.push(json!({
                        "tag": "geosite-category-ru",
                        "type": "local",
                        "format": "binary",
                        "path": geosite_ru
                    }));
                }
                if geoip_ru.exists() {
                    rules.push(json!({
                        "rule_set": ["geoip-ru"],
                        "outbound": "direct"
                    }));
                    rule_sets.push(json!({
                        "tag": "geoip-ru",
                        "type": "local",
                        "format": "binary",
                        "path": geoip_ru
                    }));
                }
            }
        }
        RoutingMode::OnlyRu => {
            rules.push(json!({
                "ip_is_private": true,
                "outbound": "direct"
            }));
            if let Ok(geo) = crate::infra::geo::GeoManager::new() {
                let (geoip_ru, geosite_ru) = geo.local_paths();
                if geosite_ru.exists() {
                    rules.push(json!({
                        "rule_set": ["geosite-category-ru"],
                        "outbound": "proxy"
                    }));
                    rule_sets.push(json!({
                        "tag": "geosite-category-ru",
                        "type": "local",
                        "format": "binary",
                        "path": geosite_ru
                    }));
                }
                if geoip_ru.exists() {
                    rules.push(json!({
                        "rule_set": ["geoip-ru"],
                        "outbound": "proxy"
                    }));
                    rule_sets.push(json!({
                        "tag": "geoip-ru",
                        "type": "local",
                        "format": "binary",
                        "path": geoip_ru
                    }));
                }
            }
        }
        RoutingMode::BypassCn => {
            rules.push(json!({
                "ip_is_private": true,
                "outbound": "direct"
            }));
            if let Ok(geo) = crate::infra::geo::GeoManager::new() {
                let (geoip_cn, geosite_cn) = geo.local_paths_cn();
                if geosite_cn.exists() {
                    rules.push(json!({
                        "rule_set": ["geosite-cn"],
                        "outbound": "direct"
                    }));
                    rule_sets.push(json!({
                        "tag": "geosite-cn",
                        "type": "local",
                        "format": "binary",
                        "path": geosite_cn
                    }));
                }
                if geoip_cn.exists() {
                    rules.push(json!({
                        "rule_set": ["geoip-cn"],
                        "outbound": "direct"
                    }));
                    rule_sets.push(json!({
                        "tag": "geoip-cn",
                        "type": "local",
                        "format": "binary",
                        "path": geoip_cn
                    }));
                }
            }
        }
        RoutingMode::OnlyCn => {
            rules.push(json!({
                "ip_is_private": true,
                "outbound": "direct"
            }));
            if let Ok(geo) = crate::infra::geo::GeoManager::new() {
                let (geoip_cn, geosite_cn) = geo.local_paths_cn();
                if geosite_cn.exists() {
                    rules.push(json!({
                        "rule_set": ["geosite-cn"],
                        "outbound": "proxy"
                    }));
                    rule_sets.push(json!({
                        "tag": "geosite-cn",
                        "type": "local",
                        "format": "binary",
                        "path": geosite_cn
                    }));
                }
                if geoip_cn.exists() {
                    rules.push(json!({
                        "rule_set": ["geoip-cn"],
                        "outbound": "proxy"
                    }));
                    rule_sets.push(json!({
                        "tag": "geoip-cn",
                        "type": "local",
                        "format": "binary",
                        "path": geoip_cn
                    }));
                }
            }
        }
    }

    let final_outbound = match routing_mode {
        RoutingMode::OnlyRu | RoutingMode::OnlyCn => "direct",
        _ => "proxy",
    };

    let route = json!({
        "default_domain_resolver": {
            "server": "remote",
            "strategy": dns_strategy
        },
        "rules": rules,
        "auto_detect_interface": true,
        "final": final_outbound
    });

    (route, rule_sets)
}

/// Build the outbound object based on profile protocol and settings.
fn build_outbound(profile: &Profile) -> anyhow::Result<Value> {
    build_vless_outbound(profile)
}

/// Build VLESS outbound with optional REALITY / XTLS Vision.
fn build_vless_outbound(profile: &Profile) -> anyhow::Result<Value> {
    let tls = if let Some(ref reality) = profile.reality {
        let fingerprint = profile.fingerprint.as_deref().unwrap_or("chrome");
        let reality_json = json!({
            "enabled": true,
            "public_key": reality.public_key,
            "short_id": reality.short_id
        });
        json!({
            "enabled": true,
            "server_name": reality.server_name,
            "utls": {
                "enabled": true,
                "fingerprint": fingerprint
            },
            "reality": reality_json
        })
    } else {
        json!({
            "enabled": true,
            "server_name": profile.address,
            "insecure": false
        })
    };

    let mut outbound = json!({
        "type": "vless",
        "tag": "proxy",
        "server": profile.address,
        "server_port": profile.port,
        "uuid": profile.uuid,
        "packet_encoding": "xudp",
        "tls": tls
    });

    if let Some(ref flow) = profile.flow {
        outbound["flow"] = json!(flow);
    }

    // Add transport layer if specified (grpc, ws, httpupgrade, etc.)
    if let Some(ref transport_type) = profile.transport_type {
        let mut transport = json!({"type": transport_type});
        if *transport_type == TransportType::Grpc {
            if let Some(ref service_name) = profile.transport_service_name {
                transport["service_name"] = json!(service_name);
            }
            transport["idle_timeout"] = json!("15s");
            transport["ping_timeout"] = json!("15s");
        }
        outbound["transport"] = transport;
    }

    Ok(outbound)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::profile::{Profile, Protocol, RealitySettings};

    fn test_profile() -> Profile {
        let mut p = Profile::new(
            "Example".to_string(),
            Protocol::Vless,
            "203.0.113.42".to_string(),
            59431,
            "671c62c7-6768-4b98-ac6b-572c9c707be0".to_string(),
        );
        p.security = Some(crate::config::profile::Security::Reality);
        p.reality = Some(RealitySettings {
            public_key: "0IO3LodsrMnhOWh4ogwgdVqYg30CS5-snhFMwldOuAQ".to_string(),
            short_id: "f04debc34cbc48a4".to_string(),
            server_name: "google.com".to_string(),
            spider_x: "/".to_string(),
        });
        p.transport_type = Some(TransportType::Grpc);
        p.fingerprint = Some("chrome".to_string());
        p
    }

    #[test]
    fn generated_config_has_required_keys() {
        let profile = test_profile();
        let settings = Settings::default();
        let config = generate_config(&profile, &settings).unwrap();

        assert!(config.get("log").is_some());
        assert!(config.get("dns").is_some());
        assert!(config.get("inbounds").is_some());
        assert!(config.get("outbounds").is_some());
        assert!(config.get("route").is_some());
        assert!(config.get("experimental").is_some());
    }

    #[test]
    fn generated_config_global_final_is_proxy() {
        let profile = test_profile();
        let settings = Settings::default();
        let config = generate_config(&profile, &settings).unwrap();
        let route = config.get("route").unwrap();
        assert_eq!(route["final"].as_str().unwrap(), "proxy");
    }

    #[test]
    fn generated_config_only_ru_final_is_direct() {
        let profile = test_profile();
        let settings = Settings {
            routing_mode: RoutingMode::OnlyRu,
            ..Default::default()
        };
        let config = generate_config(&profile, &settings).unwrap();
        let route = config.get("route").unwrap();
        assert_eq!(route["final"].as_str().unwrap(), "direct");
    }

    #[test]
    fn vless_outbound_with_reality() {
        let profile = test_profile();
        let outbound = build_vless_outbound(&profile).unwrap();

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
        let profile = Profile::new(
            "Simple".to_string(),
            Protocol::Vless,
            "1.2.3.4".to_string(),
            443,
            "uuid".to_string(),
        );
        let outbound = build_vless_outbound(&profile).unwrap();

        let tls = &outbound["tls"];
        assert_eq!(tls["enabled"], true);
        assert_eq!(tls["server_name"], "1.2.3.4");
        assert_eq!(tls["insecure"], false);
        assert!(tls.get("reality").is_none());
    }

    #[test]
    fn vless_outbound_with_flow() {
        let mut profile = test_profile();
        profile.flow = Some(crate::config::profile::Flow::XtlsRprxVision);
        let outbound = build_vless_outbound(&profile).unwrap();
        assert_eq!(outbound["flow"], "xtls-rprx-vision");
    }

    #[test]
    fn vless_outbound_with_grpc_transport() {
        let profile = test_profile();
        let outbound = build_vless_outbound(&profile).unwrap();

        assert!(outbound.get("transport").is_some());
        let transport = &outbound["transport"];
        assert_eq!(transport["type"], "grpc");
        assert_eq!(transport["idle_timeout"], "15s");
        assert_eq!(transport["ping_timeout"], "15s");
    }

    #[test]
    fn vless_outbound_with_grpc_service_name() {
        let mut profile = test_profile();
        profile.transport_service_name = Some("my-service".to_string());
        let outbound = build_vless_outbound(&profile).unwrap();
        assert_eq!(outbound["transport"]["service_name"], "my-service");
    }

    #[test]
    fn vless_outbound_without_transport() {
        let mut profile = test_profile();
        profile.transport_type = None;
        let outbound = build_vless_outbound(&profile).unwrap();
        assert!(outbound.get("transport").is_none());
    }

    #[test]
    fn build_route_global_has_basic_rules() {
        let (route, rule_sets) = build_route(&RoutingMode::Global, DnsStrategy::OnlyIpv4);
        assert!(rule_sets.is_empty());
        let rules = route["rules"].as_array().unwrap();
        assert_eq!(rules.len(), 3); // ipv6 reject, dns hijack, direct cidr
        assert_eq!(route["final"], "proxy");
    }

    #[test]
    fn build_route_only_ru_has_private_rule_and_final_direct() {
        let (route, _rule_sets) = build_route(&RoutingMode::OnlyRu, DnsStrategy::PreferIpv4);
        let rules = route["rules"].as_array().unwrap();
        assert!(rules.len() >= 4); // basic 3 + ip_is_private
        assert_eq!(route["final"], "direct");
    }

    #[test]
    fn generated_config_only_cn_final_is_direct() {
        let profile = test_profile();
        let settings = Settings {
            routing_mode: RoutingMode::OnlyCn,
            ..Default::default()
        };
        let config = generate_config(&profile, &settings).unwrap();
        let route = config.get("route").unwrap();
        assert_eq!(route["final"].as_str().unwrap(), "direct");
    }

    #[test]
    fn build_route_bypass_cn_has_private_rule() {
        let (route, _rule_sets) = build_route(&RoutingMode::BypassCn, DnsStrategy::PreferIpv4);
        let rules = route["rules"].as_array().unwrap();
        assert!(rules.len() >= 4); // basic 3 + ip_is_private
        assert_eq!(route["final"], "proxy");
    }

    #[test]
    fn build_route_only_cn_has_private_rule_and_final_direct() {
        let (route, _rule_sets) = build_route(&RoutingMode::OnlyCn, DnsStrategy::PreferIpv4);
        let rules = route["rules"].as_array().unwrap();
        assert!(rules.len() >= 4); // basic 3 + ip_is_private
        assert_eq!(route["final"], "direct");
    }
}
