# Configuration

`kvn-tui` stores profiles, subscriptions, and application settings in:

```text
~/.config/kvn-tui/profiles.json
```

This is the default location; `XDG_CONFIG_HOME` is respected when set.

Press `e` in the TUI to edit the file with `$VISUAL` or `$EDITOR`. The daemon
reloads and validates the result when the editor closes. Keep a backup before
substantial manual changes. Saves performed by kvn-tui use an atomic temporary
file and rename, so an interrupted write cannot replace a valid configuration
with a partial file.

## File structure

The current schema version is 2. A minimal configuration is:

```json
{
  "schema_version": 2,
  "profiles": [],
  "subscriptions": [],
  "settings": {}
}
```

Each profile has common fields such as `id`, `name`, `address`, `port`, and
`tags`. Protocol-specific fields are stored at the same level and selected by
the `protocol` discriminator. See the
[supported protocols](../README.md#supported-protocols) for available profile
types and share-link schemes.

A subscription contains `id`, `name`, `url`, `auto_update`, and an optional
`last_updated` timestamp. Valid update schedules are `off`, `every_1h`,
`every_12h`, `every_1d`, and `every_7d`.

## Settings

| Field | Default | Description |
|-------|---------|-------------|
| `default_profile` | `null` | UUID of the selected default profile |
| `tun_interface` | `tun0` | Name of the sing-box TUN interface |
| `dns` | Cloudflare DoH | DNS servers, rules, strategy, and fake-IP state |
| `geo_routing` | no region | Country modes, rule-set updates, and service overrides |
| `auto_connect` | `false` | Connect to `last_connected_profile` at startup |
| `kill_switch` | `false` | Persisted kill-switch state |
| `last_connected_profile` | `null` | Last connected profile; maintained by the application |
| `theme` | `tokyo-night` | Bundled palette slug or `omarchy` |
| `log_level` | `info` | `trace`, `debug`, `info`, `warn`, or `error` |

`dns_strategy` is a legacy compatibility field mirrored from `dns.strategy`.
Edit `dns.strategy` instead. `RUST_LOG`, when set, overrides `log_level`.

## DNS

Press `D` for the built-in Cloudflare DoH, Google DoT, Quad9 DoH, and system
resolver presets, strategy selection, and fake-IP toggle. Custom servers and
rules can be edited in JSON.

Supported server types are `local`, `udp`, `tcp`, `tls`, `https`, `quic`, and
`fake_ip`. Supported strategies are `prefer_ipv4`, `prefer_ipv6`, `ipv4_only`,
and `ipv6_only`.

The following snippet belongs inside `settings`:

```json
{
  "dns": {
    "servers": [
      { "type": "local", "tag": "local" },
      {
        "type": "https",
        "tag": "remote",
        "server": "1.1.1.1",
        "path": "/dns-query"
      }
    ],
    "rules": [
      {
        "domain_suffix": ["internal.example"],
        "server": "local"
      }
    ],
    "final_server": "remote",
    "strategy": "prefer_ipv4",
    "fakeip_enabled": false
  }
}
```

Server tags must be non-empty and unique. `final_server` and every rule's
`server` must reference an existing tag. Enabling `fakeip_enabled` requires a
`fake_ip` server in the same list.

DNS rules can match `domain`, `domain_suffix`, `domain_keyword`,
`domain_regex`, or `rule_set`; each rule may also set `disable_cache`.

## Geo and service routing

Press `o` to select `ru`, `cn`, `ir`, or `global`. Country regions offer
Global, Bypass, and Only modes; `global` skips country rule-set downloads and
offers Global mode only. The last mode selected for each country is retained.

The following snippet belongs inside `settings`:

```json
{
  "geo_routing": {
    "current_region": "ru",
    "selected_region_modes": {
      "ru": "bypass_ru"
    },
    "auto_update": "every_1d",
    "service_routes": {
      "steam": "direct",
      "telegram": "proxy"
    }
  }
}
```

Routing modes are serialized as `global`, `bypass_<region>`, or
`only_<region>`. Geo update schedules are `off`, `every_12h`, `every_1d`,
`every_3d`, and `every_7d`.

Press `S` to apply the predefined service overrides. `proxy` always uses the
tunnel; `direct` sends matching traffic through the real network, including
past the kill switch. An absent service entry means Disabled and follows the
country routing mode.

Service rule-sets are fetched through the active tunnel. Missing files do not
block a connection: the override remains inactive until the files are
downloaded and the connection is restarted.

## Themes and logging

Press `C` to choose one of the 22 palettes bundled from [`themes/`](../themes/).
The special `omarchy` value follows the active Omarchy theme. Fresh non-Omarchy
installations use `tokyo-night`.

`log_level` controls both application and generated sing-box logging. Accepted
values are `trace`, `debug`, `info`, `warn`, and `error`; `RUST_LOG` takes
precedence when present.

## Validation and migrations

Configuration is parsed, migrated, and semantically validated before use.
Validation checks profile references and required values, DNS tags and server
references, the TUN interface, theme slug, and log level.

The TUN interface must contain only ASCII letters, digits, `-`, or `_`, and be
at most 15 characters. `default_profile`, when set, must reference an existing
profile.

The root structure, settings, DNS objects, subscriptions, and protocol structs
without flattened TLS reject unknown fields. Protocol variants containing a
flattened TLS block cannot enforce this serde rule, so do not rely on unknown
fields being rejected everywhere.

Older files are migrated automatically:

- v0 → v1 moves the legacy `dns_strategy` value into `dns.strategy`.
- v1 → v2 moves the legacy VLESS `fingerprint` into the shared TLS settings.

A file with a schema version newer than the running kvn-tui build is rejected;
upgrade kvn-tui instead of downgrading the version manually.

## Runtime files

| Resource | Location |
|----------|----------|
| Profiles and settings | `~/.config/kvn-tui/profiles.json` |
| Geo and service rule-sets | `~/.config/kvn-tui/geo/` |
| Application log | `~/.config/kvn-tui/logs/app.log` |
| sing-box log | `~/.config/kvn-tui/logs/sing-box.log` |
| Waybar and recovery state | `~/.config/kvn-tui/state.json` |
| IPC socket | `$XDG_RUNTIME_DIR/kvn-tui.sock` or `/tmp/kvn-tui-<uid>.sock` |
| Generated sing-box config | `$XDG_RUNTIME_DIR/kvn-tui-singbox.json`, `$XDG_CACHE_HOME/kvn-tui/singbox.json`, or `/tmp/kvn-tui-singbox.json` |
