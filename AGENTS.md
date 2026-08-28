# Agent Guide: kvn-tui

This document contains project-specific context and conventions for AI coding agents. It supplements `README.md` with architectural details, coding styles, and rules of thumb.

---

## Project Overview

`kvn-tui` is a **terminal VPN client** for Arch Linux. It is a Rust TUI application that manages VPN profiles, generates sing-box configurations, and orchestrates the `sing-box` binary as a child process. Navigation is vim-style (`j`/`k`/`g`/`G`).

The app does **not** implement VPN protocols itself. It is a configuration generator and process manager around the external `sing-box` binary.

---

## Module Map

| Module | Path | Responsibility |
|--------|------|----------------|
| `cli` | `src/cli.rs` | CLI argument parsing (`--waybar-status`, `status`/`connect`/`disconnect`/`reconnect`/`toggle` one-shot IPC clients, `setup --omarchy`, `--version`) |
| `app` | `src/app.rs`, `src/app/model.rs`, `src/app/msg.rs`, `src/app/update.rs`, `src/app/effect.rs` | TEA core: Model, Msg, Update, Effect — pure data, messages, business logic, side-effect declarations |
| `model` | `src/app/model.rs` | Application state (`Model`), overlay + connection state + subscription state, input state — pure data, no side effects |
| `msg` | `src/app/msg.rs` | Message enum (`Msg`) — all external events (keys, ticks, logs, geo, resume, etc.) |
| `update` | `src/app/update.rs` | Pure `update(model, msg) -> Vec<Effect>` — business logic, input routing, mode transitions |
| `effect` | `src/app/effect.rs` | Effect enum — declarative description of side effects to be executed by runtime |
| `daemon` | `src/daemon.rs` | Headless daemon: owns sing-box process, config, mpsc channel, IPC server, background services |
| `tui_client` | `src/tui_client.rs` | TUI client: connects to daemon via Unix socket, renders UI, forwards input, reads clipboard |
| `ipc` | `src/ipc.rs` | NDJSON protocol over Unix domain socket for daemon ↔ TUI client communication |
| `test_helpers` | `src/test_helpers.rs` | Shared test utilities (e.g. `model_with_profiles`)
| `ui` | `src/ui.rs`, `src/ui/layout.rs`, `src/ui/widgets.rs`, `src/ui/styles.rs`, `src/ui/palette.rs`, `src/ui/nav.rs` | ratatui rendering (used by TUI client only), layout splits, widget definitions, palette-driven `Theme`, navigation helpers |
| `palette` | `src/ui/palette.rs`, `themes/*.toml`, `build.rs` | 19 vendored Omarchy palettes; `build.rs` compiles `themes/*.toml` into a `BUNDLED` static at compile time (no runtime TOML parsing) |
| `config` | `src/config.rs`, `src/config/profile.rs`, `src/config/subscription.rs` | JSON config I/O, profile and subscription struct definitions, subscription fetcher |
| `singbox` | `src/singbox.rs`, `src/singbox/config.rs`, `src/singbox/runner.rs`, `src/singbox/clash_api.rs`, `src/singbox/process_handle.rs` | Process lifecycle: write temp config, run `sing-box check`, spawn `sing-box run`, kill on disconnect; Clash API client for live traffic stats; `Child` wrapper |
| `geo` | `src/geo.rs` | Download and cache geoip/geosite rule-sets for sing-box routing |
| `paths` | `src/paths.rs` | XDG directory resolution (`~/.config/kvn-tui/`), atomic path construction |
| `atomic_write` | `src/atomic_write.rs` | Atomic file write helper (write `.tmp` + fsync + rename + parent-dir fsync) |
| `waybar` | `src/services/waybar.rs` | Read/write `state.json` for waybar integration and crash recovery |
| `suspend` | `src/services/suspend.rs` | D-Bus listener for `systemd-logind` `PrepareForSleep` signals (zbus) |
| `killswitch` | `src/services/killswitch.rs` | nftables helper integration: enable/disable systemd unit, pre-allow VPN handshake IPs, reconcile state on startup |
| `services` | `src/services.rs`, `src/services/log_tailer.rs`, `src/services/waybar.rs`, `src/services/suspend.rs` | Background services: log tailer, waybar state I/O, suspend watcher (all run inside the daemon) |
| `clipboard` | `src/tui_client/clipboard.rs` | System clipboard integration; auto-detects Wayland (`wl-paste` / `wl-copy`) or X11 (`xclip`, falls back to `xsel`); reads clipboard content and passes it to `parse_share_link` or the subscription fetcher |
| `editor` | `src/tui_client/editor.rs` | Launch `$EDITOR` / `$VISUAL` on `profiles.json`, temporarily restore terminal |
| `theme_watch` | `src/tui_client/theme_watch.rs` | Resolves `settings.theme` slug to a `Theme` (with `"omarchy"` sentinel falling back to `tokyo-night`); watches Omarchy 4's XDG state current-theme directory or the Omarchy 3 XDG config fallback and emits `Msg::ThemeChanged`; no-op when Omarchy isn't installed |
| `omarchy plugin` | `contrib/omarchy-plugin/` | Quickshell bar module (`kvn.tui`) for Omarchy 4: `Widget.qml` (icon + popup panel), `KvnService.qml` (persistent NDJSON socket client to the daemon). Embedded into the binary via `include_str!`, materialized by `cli::write_omarchy_plugin_files`, installed to `~/.config/omarchy/plugins/kvn.tui/` by `contrib/setup-omarchy.sh` (falls back to the legacy `command` bar module when the shell plugin registry is absent) |

---

## Build System & Dependencies

- **Rust**: edition 2024, minimum version 1.88
- **External binary**: `sing-box` must be installed separately and available on `$PATH` (or via `SING_BOX_PATH` env var)
- **Key crates**: `ratatui` + `crossterm` (TUI), `serde` + `serde_json` (config), `zbus` (D-Bus), `ureq` (HTTP), `tracing` (logs), `anyhow` + `thiserror` (errors)

Build release:

```bash
cargo build --release
```

Run (no root required when sing-box has capabilities):

```bash
./target/release/kvn-tui
```

Install polkit rule (avoids authentication dialogs on connect):

```bash
sudo ./target/release/kvn-tui setup --polkit
```

---

## Release Process

See the `release` skill in `.agents/skills/release/SKILL.md` for the full version-bump and tagging workflow. Supports auto-bump by semver level (major / minor / patch) or explicit version.

---

## Platform Constraints

**Arch Linux.** Both Wayland and X11 sessions are supported. Do not add generic Linux abstractions (other distros, BSDs, …) without explicit user request.

- Clipboard: auto-detected at startup in `src/tui_client/clipboard.rs` — prefers `wl-paste` / `wl-copy` on Wayland, falls back to `xclip` then `xsel` on X11
- Power events: listens to `org.freedesktop.login1.Manager.PrepareForSleep` via zbus (display-server-agnostic)
- TUN interface: created by sing-box; requires root privileges

---

## Code Conventions

### Error Handling
- Use `anyhow::Result<T>` for fallible functions at the application / UI boundary.
- Use `thiserror` only if you need structured error enums (rare in this codebase).
- Prefer `.context("...")` and `.with_context(|| format!("..."))` to add descriptive messages.

### File I/O
- **Atomic writes are mandatory** for config files. Pattern: write to `.tmp`, then `fs::rename`.
- See `config::save_config_at` and `geo::GeoManager::write_atomic` for the canonical implementation.

### Logging
- Use `tracing::info!`, `tracing::warn!`, `tracing::error!` — not `println!`.
- The subscriber is initialized in `main.rs` with `EnvFilter` and `fmt::layer().without_time()`.

### Serialization
- All persistent data uses `serde` + `serde_json`.
- Config file: `profiles.json` (top-level `Config` struct with `profiles: Vec<Profile>` and `settings: Settings`).
- `Profile` stores common fields (`id`, `name`, `address`, `port`) plus `#[serde(flatten)] config: ProtocolConfig`. `ProtocolConfig` is an internally-tagged enum (`#[serde(tag = "protocol", rename_all = "lowercase")]`) with one variant per protocol (11 total). VLESS keeps its protocol-specific fields flat on `VlessConfig` for backward compatibility with configs written before the multi-protocol refactor.
- Shared TLS/transport types: `TlsCommon` (SNI, ALPN, fingerprint, insecure, `EchSettings`, `RealitySettings`), `TransportConfig` (WebSocket, gRPC, HTTP upgrade). ECH and REALITY are mutually exclusive and enforced in `ProtocolConfig::validate`.
- `Profile::dedup_key()` returns a stable string key (`"protocol:credential@host:port"`) used by the subscription importer to detect duplicate profiles across imports.
- Enums use `#[serde(rename_all = "snake_case")]` or `"lowercase"` as appropriate.

### Formatting & Linting
- Run `cargo fmt` before committing to keep the codebase consistent.
- Run `cargo clippy --all-targets --all-features` and fix any warnings before committing.
- The project uses the default `rustfmt` configuration (no `rustfmt.toml`).

### Naming
- Modules are snake_case (`singbox`, not `sing_box`).
- The binary name is `kvn-tui`; the crate name is `kvn-tui`.

### Module Files
- Use the **new Rust module style**: a module `foo` with submodules lives in `src/foo.rs` (parent) and `src/foo/bar.rs` (children). Do **not** create `src/foo/mod.rs`; that is the old style and is not used in this project.

---

## Testing Patterns

- Tests are co-located in `#[cfg(test)] mod tests` blocks at the bottom of each source file.
- `src/test_helpers.rs` provides shared test utilities (e.g., `model_with_profiles`).
- Tests should not depend on external network or the `sing-box` binary unless explicitly marked `#[ignore]` (or guarded by a `command -v sing-box` runtime check).
- Use `tempfile` for file-system tests; use `NamedTempFile` / `tempdir()` for isolation.
- Tests that mutate process environment (`std::env::set_var`) **must** lock `crate::test_helpers::ENV_LOCK` to serialize against other env-touching tests. The same lock is required for tests that *read* the environment non-atomically — spawning a child process (execve/PATH lookup reads the env) or resolving an env-dependent path more than once — otherwise they race the mutating tests and fail intermittently.
- Snapshot tests use [insta](https://insta.rs/). Regenerate with `INSTA_UPDATE=always cargo test`, then review the diffs before committing.
- Example pattern: create a default `Profile`, generate a config, assert on JSON structure.

### Coverage Policy

- **Minimum total coverage is 85 %** on both regions and lines. CI enforces this in the `coverage` job — see `.github/workflows/ci.yml`.
- Any change that lowers total region or line coverage below 85 % must add tests in the same PR to bring it back up. A code change that drops coverage is not "done."
- Check locally before pushing:

  ```bash
  # Requires cargo-llvm-cov (install once: cargo install cargo-llvm-cov)
  # and llvm-tools-preview. On distros without the rustup component, set
  # LLVM_COV=/usr/bin/llvm-cov LLVM_PROFDATA=/usr/bin/llvm-profdata.
  cargo llvm-cov --summary-only
  ```

  The `TOTAL` line shows region / function / line coverage. Both region and line numbers must be ≥ 85 % for CI to pass.
- The CI gate parses the `TOTAL` line directly because `cargo-llvm-cov --fail-under-*` flags are silently no-op in the 0.8.x series.
- 0 %-coverage I/O wrappers (`daemon.rs`, `tui_client.rs`, `main.rs`, `services/killswitch.rs`, `services/suspend.rs`, `tui_client/clipboard.rs`, `tui_client/theme_watch.rs` watcher thread, `singbox/clash_api.rs`, install_* in `cli.rs`) are accepted as-is — they wrap subprocesses, DBus, Unix sockets, HTTP, and filesystem watchers, which need integration harnesses out of scope for unit tests. **Do not rewrite them just to add fake-based tests.** Cover new logic with pure-function tests instead.

---

## Key Design Decisions

### TEA Architecture
The application follows **The Elm Architecture (TEA)**:
1. **Model** (`app/model.rs`) holds all application state as pure data. UI state is split into `Overlay` (popup/modal) and `ConnectionState` (idle/connecting/connected).
2. **Messages** (`app/msg.rs`) represent every external event — keyboard input, timer ticks, log lines, geo updates, system resume.
3. **Update** (`app/update.rs`) is a pure function `update(model, msg) -> Vec<Effect>`: no I/O, no threads, no system calls. All business logic lives here.
4. **Effects** (`app/effect.rs`) are declarative descriptions of side effects (`Connect`, `DownloadGeo`, `SaveConfig`, `Quit`, etc.).
5. **Daemon** (`daemon.rs`) owns the canonical `Model`, the `mpsc` channel, the sing-box `process_slot`, and all background services (ticker, suspend watcher, log tailer, IPC server). It exposes a Unix domain socket IPC server (`ipc.rs`) that accepts NDJSON commands from TUI clients.
6. **TUI Client** (`tui_client.rs`) connects to the daemon socket, enters the alternate screen, renders the UI using ratatui, and forwards keyboard input (plus clipboard/editor actions) as IPC commands. It has its own local `Model` that is kept in sync via `StateSnapshot` broadcasts from the daemon.
7. **IPC Protocol** (`ipc.rs`) uses newline-delimited JSON over a Unix socket. Commands: `Attach`, `Detach`, `Key`, `SelectSource`, `SetMainPaneFocus`, `GoFirst`, `ConnectProfile`, `Disconnect`, `Reconnect`, `SetRoutingMode`, `SetGeoRegion`, `SetKillSwitch`, `SetAutoConnect`, `Paste`, `Copied`, `ReloadConfig`, `Quit`, `ClientError`. Responses: `StateSnapshot` pushed by the daemon after every state change. The semantic commands (`ConnectProfile` through `SetAutoConnect`) exist for non-TUI clients — the Omarchy Quickshell module and the `kvn-tui status/connect/disconnect/reconnect/toggle` CLI subcommands. Overlay commits (routing mode, geo region) are shared between the key handlers and IPC via `commit_routing_mode` / `commit_geo_region` in `update.rs` so both paths run identical logic.

This separation makes `update.rs` fully synchronous and trivial to unit-test.

### Background Services
Background work is executed in dedicated threads spawned by the **daemon** (`daemon.rs`):
- **Ticker** — sends `Msg::Tick` every 250 ms to drive connection state machines.
- **Suspend watcher** — `services/suspend.rs` runs a blocking zbus listener that sends `Msg::SystemResumed`; the daemon auto-reconnects on resume even when no TUI is attached.
- **IPC server** — `ipc.rs` accepts Unix socket connections from TUI clients, parses NDJSON commands, and forwards them as `Msg::IpcCommand` into the daemon's mpsc channel.
- **Effects** — `Connect`, `DownloadGeo`, and `PasteClipboard` (via `IpcCommand`) each spawn a short-lived thread that sends the result back via the daemon's channel.
- **Log tailer** — `LogTailer` (`services/log_tailer.rs`) reads new lines from the shared log file on every `Tick` inside the daemon. App status messages are also written to the same file (with an `[app]` prefix) so both sing-box and app logs are visible in the TUI log panel.
- **State I/O** — `services/waybar.rs` writes `state.json` on connect/disconnect for waybar integration.

The **TUI client** (`tui_client.rs`) additionally spawns:
- **Event reader** — polls `crossterm` events and sends `Msg::Key` / `Msg::Resize` to the local TUI channel. Reading can be paused while `$EDITOR` is open.
- **Ticker** — sends `Msg::Tick` every 250 ms to drive the local log tailer.
- **IPC reader** — reads NDJSON state snapshots from the daemon socket and forwards them as `Msg::StateUpdate`.

### sing-box Config Generation
- `singbox::config::generate_config` builds a complete sing-box 1.12+ JSON object from a `Profile` and `Settings`.
- The config is written to a temp file (`/tmp/kvn-tui-singbox.json` or `$XDG_RUNTIME_DIR`), validated with `sing-box check`, and only then is `sing-box run` spawned.
- If the process exits immediately, stderr is captured and surfaced to the user.
- `build_outbound(profile)` dispatches to a per-protocol builder and returns `Vec<serde_json::Value>` (most protocols return one outbound; ShadowTLS returns two — a `shadowtls` wrapper tagged `shadowtls-wrap` plus a `shadowsocks` detour tagged `proxy`).
- Shared helpers: `build_tls_block` (TLS + ECH + REALITY), `build_transport_block` (WebSocket / gRPC / HTTP upgrade). No deprecated sing-box fields (no `obfs_password`, no `aes-128-cfb`, no top-level `dns.fakeip`, no WireGuard outbound).

### Routing Modes
- `RoutingMode::Global` — all traffic through VPN.
- `RoutingMode::BypassRu` — RU IPs/domains bypass VPN (direct).
- `RoutingMode::OnlyRu` — only RU IPs/domains go through VPN; everything else is direct.
- `RoutingMode::BypassCn` — CN IPs/domains bypass VPN (direct).
- `RoutingMode::OnlyCn` — only CN IPs/domains go through VPN; everything else is direct.
- The available routing modes depend on the selected **geo region** (`Ru`, `Cn`, `Ir`, or `Global`). `RoutingMode::available(region)` returns the list dynamically.
- Geo-region and routing-mode preferences are grouped under `settings.geo_routing: GeoRouting`. It stores `current_region: Option<GeoRegion>` and `selected_region_modes: HashMap<GeoRegion, RoutingMode>`. The active mode is derived from `selected_region_modes[current_region]` and falls back to `Global`. Switching back to a previously used region restores its last routing mode.
- Rule-sets are local `.srs` binary files downloaded to `~/.config/kvn-tui/geo/`.
- **Service routing overrides** (`geo_routing.service_routes: HashMap<RoutedService, ServiceRoute>`, absent = `Disabled` / opt-in): orthogonal to the routing mode — each of the predefined services (`Steam`, `Telegram`) can be forced to `Direct` (real network location; e.g. Steam CDN downloads) or `Proxy` (always through the tunnel, even under `Bypass`). `build_route` emits the per-service `rule_set → outbound` rules ahead of the geo rules so an override wins in every mode, iterating `RoutedService::ALL` (never the map — HashMap order is nondeterministic). Assets are declared in `geo::service_assets()` as a `ServiceAssets { geoip: Option<GeoAsset>, geosite: Option<GeoAsset> }` descriptor per service, all sourced from MetaCubeX/meta-rules-dat (one provider, one branch layout). They are fetched *through the tunnel* (`Effect::DownloadServiceRuleSetsIfMissing`) — never pre-connect, where the kill switch or ISP blocks would stall the fetch — and refreshed with the periodic geo updates. Two triggers: after `Msg::Connected` (backstop), and on a service-routing commit while connected, where the reconnect is DEFERRED until the download pass reports back (`Model::pending_service_reconnect` → `Msg::ServiceRuleSetsReady`) so a first-enabled service's rules are live on the very next connection rather than requiring a second reconnect. Missing files degrade to "no rule for that service", never a failed connection. Edited via the `S` overlay (draft map in `Model::service_routing_draft`; cycling a route back to `Disabled` removes its entry — absent = Disabled — so a full cycle commits as a no-op; committed atomically on Enter).

### Share-Link Parsing
- Entry point: `config::profile::parse_share_link(uri)` dispatches on the URI scheme.
- Supported schemes: `vless://`, `vmess://`, `trojan://`, `ss://`, `hysteria2://`, `hy2://`, `tuic://`, `shadowtls://`, `anytls://`, `socks://`, `socks5://`, `http://`, `https://`, `ssh://`.
- All supported schemes are listed in `SUPPORTED_SHARE_SCHEMES` (used by both dispatch and the subscription Base64 heuristic in `config::subscription`).
- VLESS: extracts UUID, host, port, fragment (name), `flow`, `security`, `fp`, transport type, and REALITY params (`pbk`, `sid`, `sni`, `spx`). ECH config also parsed when present.
- VMess: handles both base64-JSON (v2rayN / Shadowrocket) and inline URI forms.
- Shadowsocks: handles SIP002 (`ss://base64(method:password)@host:port`) and legacy fully-base64 forms.
- Hysteria 2: `hy2://` is an alias for `hysteria2://`.
- SOCKS: `socks5://` is an alias for `socks://`.
- TLS parameters shared across protocols (VLESS excluded — keeps fields flat for backward compat): `TlsCommon` with SNI, ALPN, fingerprint, insecure, ECH (`EchSettings`), and REALITY (`RealitySettings`). ECH and REALITY are mutually exclusive.
- Transport (WebSocket / gRPC / HTTP): `TransportConfig` shared across VLESS, VMess, Trojan, AnyTLS.

### Suspend / Resume
- `services/suspend.rs` runs a blocking zbus listener in a dedicated thread. On resume (`PrepareForSleep` with `false`), it sends `Msg::SystemResumed` through the `mpsc` channel so `update.rs` can schedule a reconnect effect.

### Kill Switch
- Uses **nftables** + a systemd unit (`kvn-tui-killswitch.service`) that loads `/etc/kvn-tui/killswitch.nft`. The ruleset drops all outbound traffic except localhost, `tun*` interfaces, and packets marked `0x29a` by sing-box.
- Privilege escalation via **sudoers NOPASSWD** (not polkit) — grants the `network` group passwordless access to `/usr/lib/kvn-tui/killswitch-helper.sh`. Installed with `sudo kvn-tui setup --killswitch`.
- **Toggle flow**: `K` keybinding → `Effect::ApplyKillSwitch { enabled }` → daemon spawns thread calling `services::killswitch::apply(enabled)` → sends `Msg::KillSwitchApplied { enabled, error }` back. On success the boolean is flipped and config is saved; on error the boolean is unchanged and the error is shown.
- **Reconciliation on startup**: daemon queries systemd to check whether the unit is actually active and aligns `settings.kill_switch` with the real state, preventing drift if the unit was manually disabled or the helper was uninstalled.
- **Handshake window**: before spawning `sing-box run`, the daemon pre-resolves the VPN endpoint's IP addresses via DNS and adds them as temporary nftables exceptions (`allow <ip> tcp <port>`), ensuring the initial TLS/REALITY handshake is not blocked. Every non-`local`, non-`fakeip` DNS upstream from `settings.dns.servers` is also resolved and allowlisted with its protocol-appropriate port (UDP/53, TCP/53, DoT/853, DoH/443, DoQ/853) so sing-box's bootstrap resolver can reach the user-configured DoH/DoT endpoint instead of a hard-coded 1.1.1.1. These exceptions are revoked on disconnect.
- **sing-box integration**: all sing-box packets carry `default_mark=666` (fwmark `0x29a`); the nftables rule `meta mark 0x29a accept` lets them through. This ensures Bypass/Only geo-routing modes work correctly even with the kill switch active.
- **UI**: the status bar shows a `[KS]` badge when the kill switch is enabled.

### DNS Configuration
- **Data model**: `settings.dns: DnsConfig` holds `servers: Vec<DnsServer>`, `rules: Vec<DnsRule>`, `final_server: String`, `strategy: DnsStrategy`, `fakeip_enabled: bool`. Server variants map 1:1 onto sing-box 1.12 server types: `Local`, `Udp`, `Tcp`, `Tls` (DoT), `Https` (DoH, with optional `path`), `Quic` (DoQ), `FakeIp` (with `inet4_range` / `inet6_range`).
- **Validation** (`Config::validate`): server tags are non-empty and unique, `final_server` and every `rule.server` reference an existing tag, and when `fakeip_enabled` at least one `FakeIp` server is present.
- **Legacy migration**: the old `settings.dns_strategy` field is still read; on load, `Config::migrate_legacy_dns_strategy` promotes it into `dns.strategy` if `dns.strategy` is at its default. On save, `save_config_at` mirrors `dns.strategy` back into `dns_strategy` so older kvn-tui builds keep loading the file.
- **Config generation** (`singbox::config::build_dns`): emits the modern sing-box 1.12 schema — no legacy top-level `dns.fakeip` block; the fake-IP server carries its own ranges. When `fakeip_enabled` is set and a `fakeip` server exists, the builder auto-prepends an `{ query_type: ["A","AAAA"], server: <tag> }` rule (skipped if the user already added one), flips `dns.independent_cache: true`, and sets `experimental.cache_file.store_fakeip: true` so the IP→domain map survives restarts.
- **TUI overlay** (`Overlay::DnsSettings`, key `D`): six rows — four presets (Cloudflare DoH `1.1.1.1`, Google DoT `8.8.8.8`, Quad9 DoH `9.9.9.9`, system `local`), a strategy cycle, and the fake-IP toggle. Custom servers and per-domain rules are edited in `profiles.json` via `e`.
- **Strategy draft**: `Model::dns_strategy_draft: Option<DnsStrategy>` previews strategy changes while the overlay is open. `h` / `l` on the Strategy row cycle the draft (`DnsStrategy::prev` / `next`); the label renders as `Strategy: ‹ value ›` with a trailing `*` when the draft differs from the saved setting. Enter on the Strategy row commits the draft (clears it, triggers `SaveConfig` + reconnect-if-connected); Esc/q discards it.
- **Active-state detection**: `current_dns_preset_index` (in `ui/layout.rs`) structurally matches `dns.servers + final_server` against the four presets, ignoring any fake-IP server alongside; `draw_selection_modal` paints the active item bold green via `Theme::success()`. This generic active-index parameter is also used by the routing-mode and geo-region overlays.
- **Status bar**: a `[DNS: <kind>]` badge derives its label from the final server's `kind_label` (`DoH` / `DoT` / `DoQ` / `UDP` / `TCP` / `local`) or `fakeip` when `fakeip_enabled` is true.

### Theme System
- **Data**: every UI style is derived from a `Palette` (16 ANSI colors + 6 semantic colors: accent, cursor, foreground, background, selection_foreground, selection_background). `Theme` holds a `Palette` and exposes `&self` methods (`accent`, `normal`, `status`, `error`, `success`, `border`, `selected`, `selected_connected`, `popup_bg`, `background`).
- **Bundling**: `themes/*.toml` contains all 22 Omarchy 4 semantic palettes, vendored from `/usr/share/omarchy/themes/<name>/colors.toml`. `build.rs` derives the ANSI and UI fields and compiles them into `OUT_DIR/bundled_palettes.rs` (build-dep `toml`). No runtime TOML parsing — `Palette::lookup(slug)` is a static array scan.
- **Active theme resolution**: `tui_client::theme_watch::resolve_active(slug)` is the single source of truth, called both at startup and on `Msg::ThemeChanged`. The reserved slug `"omarchy"` prefers `$XDG_STATE_HOME/omarchy/current/theme.name` (Omarchy 4) and falls back to `$XDG_CONFIG_HOME/omarchy/current/theme.name` (Omarchy 3); any other slug looks up a bundled palette (with `Theme::legacy()` as the fallback for unknown names).
- **In-TUI picker** (`Overlay::ThemeSettings`, key `C`): mirrors the DNS overlay draft pattern. `j`/`k` update `Model.theme_selected` and `Model.theme_draft`; the TUI client recomputes `model.theme` from the draft on every snapshot apply (live preview). Enter persists `settings.theme = <slug>` and emits `Effect::SaveConfig`. Esc clears the draft and reverts. The Auto-entry (slug `"omarchy"`) is shown only when `detect_omarchy_theme()` returns `Some` — non-Omarchy users see only the 22 bundled palettes.
- **Watcher**: spawned only when the detected Omarchy 4 state or Omarchy 3 config `current/` directory exists. It watches that directory because theme updates replace files and subtrees within it atomically. Emits `Msg::ThemeChanged(Theme)` to the TUI channel. The update reducer applies it only when `settings.theme == "omarchy"`; manual picker overrides win.
- **Frame background**: `draw()` paints the whole `frame.area()` with `theme.background()` before any other widget so cells with `Style::default()` (no explicit `bg`) inherit the palette color instead of falling through to the terminal default. Popups continue to use the same color via `theme.popup_bg()`; border-only blocks only set `fg`, so the fill survives.
- **Terminal padding (OSC 11)**: ratatui can't reach the pixel padding between the character grid and the window border. `tui_client::apply_terminal_bg(color)` emits `ESC ] 11 ; #RRGGBB ESC \\` so the terminal emulator repaints its own background; unsupported emulators silently ignore. Called on startup, on every effective-bg change in `apply_snapshot`, and in `Msg::ThemeChanged`. On exit `reset_terminal_bg()` emits `ESC ] 111 ESC \\` (OSC 111) to restore the user's terminal default. Both calls are guarded by `io::stdout().is_terminal()` to stay silent in pipes/CI.

### State I/O
- `services/waybar.rs` writes a small JSON file (`state.json`) on every connect/disconnect. It stores connection status, active profile name, and sing-box PID.
- Used by the `--waybar-status` CLI flag and for crash recovery (state is cleared on startup).

### Daemon + TUI Client Architecture
- **Daemon** (`kvn-tui --daemon`) runs headless. It owns the sing-box process, config, geo updates, suspend/resume handling, and log tailing. It binds a Unix domain socket for IPC.
- **TUI Client** (`kvn-tui`) connects to the daemon socket, requests a state snapshot (`Attach`), enters the alternate screen, and renders the UI. Keyboard input is forwarded to the daemon as `IpcCommand::Key` (except `p` and `e`, which are handled locally because they need terminal/clipboard access).
- Pressing `q` (or `Esc`) when no overlay is shown sends `Detach` to the daemon, leaves the alternate screen, disables raw mode, and **exits the TUI process**. The daemon and sing-box keep running. Shell regains the prompt immediately because the foreground TUI process actually exits. If an overlay is open (Help, ConfirmDelete, RoutingMode, GeoRegions, Error), `q`/`Esc` is forwarded to the daemon as a normal key, which closes the overlay.
- Pressing `Ctrl+C` sends `Quit` to the daemon. The daemon stops sing-box, cleans up the Unix socket, and exits. The TUI waits briefly (300 ms) for cleanup to complete before exiting.
- Running `kvn-tui` again connects to the same daemon and re-attaches, restoring the TUI instantly without restarting sing-box.
- The IPC protocol is NDJSON over a Unix socket. The daemon pushes a full `StateSnapshot` after every state change. The snapshot includes the complete config (`profiles` and `settings`) so the TUI client always renders the current data.
- `handle_ipc_command` unconditionally appends `Effect::BroadcastState` to every IPC command result, ensuring the daemon always pushes state after user interaction.
- `handle_geo_result`, `Msg::ConnectFailed`, and the `handle_tick` idle fallback also append `Effect::BroadcastState` so state mutations that don't produce other broadcast-triggering effects are still visible to the TUI.

### Geo Region Selection
- `settings.geo_routing.current_region` (`Option<GeoRegion>`) controls which country rule-sets are downloaded and which routing modes are shown.
- `GeoRegion::Ru` — download RU geoip/geosite, enable `Global` / `BypassRu` / `OnlyRu`.
- `GeoRegion::Cn` — download CN geoip/geosite, enable `Global` / `BypassCn` / `OnlyCn`.
- `GeoRegion::Global` — skip geo downloads, only `Global` mode is available.
- On first launch (when `geo_routing.current_region` is `None`), a modal overlay forces the user to pick a region before the main UI is usable.
- The region can be changed at runtime with the `o` keybinding. When the region changes, the previous region's mode is saved into `geo_routing.selected_region_modes` and the new region's previously stored mode is restored (falling back to `Global`).

### Auto-Connect
- `settings.auto_connect` (persisted in `profiles.json`) controls whether the app reconnects to the last used profile on startup.
- `settings.last_connected_profile` stores the UUID of the most recently connected profile. It is updated in `update.rs` on `Msg::Connected` and saved via `Effect::SaveConfig`.
- `Model::new()` calls `resolve_startup_state()` to check `auto_connect` + `last_connected_profile`. If both are set and the profile exists, the model starts in `ConnectionState::Connecting` with that profile pre-selected, and the status bar shows `Auto-connecting to {name}…`.
- The user can toggle `auto_connect` at runtime with the `a` keybinding, which triggers `Effect::SaveConfig` immediately.

---

## Side-effect-free Boundaries

The TEA update function (`app::update::update`) must remain free of I/O, threads, and system calls. Side effects are declared as `Effect` values and executed by the daemon runtime.

Rules of thumb:
- `app::update::update(model, msg) -> Vec<Effect>` must not call functions from `services`, `geo`, `paths`, `atomic_write`, `config::load_config`, `config::subscription`, `singbox::clash_api`, `singbox::runner`, `tui_client::clipboard`, `tui_client::editor`, or perform any file/network/process I/O. **Documented exception**: `theme_picker_slugs()` (used by the `C` key handler and `handle_theme_picker`) calls `theme_watch::detect_omarchy_theme()`, which does a single small `fs::read_to_string` of the Omarchy 4 state theme path or Omarchy 3 config fallback to decide whether to show the Auto entry. Cheap, deterministic, and scoped to a key press; promoted to "OK" because the alternative (caching in `Model`) costs more clarity than it saves. The full `Theme` resolution stays out of `update`: the picker handler only mutates `theme_draft`/`settings.theme`, and the TUI client recomputes `model.theme` via `resolve_active` on snapshot apply.
- `Model::set_status` is pure (mutates only in-memory state). Any message that should also be persisted to the application log must return `Effect::AppendAppLog`.
- `Model::new` is allowed to perform initialization I/O (load config, read `state.json`, etc.).
- `singbox::config::generate_config` is pure: it receives geo file availability (`GeoAvailability`) from the caller and does not touch the file system.
- `ui::widgets::StatusBar::render` reads only `Model` fields; it does not call `GeoManager` or access files.
- The daemon (`daemon::execute_daemon_effect`) is the sole executor of `Effect` values. It may perform I/O, spawn threads, and mutate `Model` where appropriate.

If you need to add a new side effect from `update`, add a new `Effect` variant and implement it in `daemon::execute_daemon_effect`.

---

## Configuration Paths

| Resource | Path |
|----------|------|
| Profiles & settings | `~/.config/kvn-tui/profiles.json` |
| Geo rule-sets | `~/.config/kvn-tui/geo/` |
| sing-box logs | `~/.config/kvn-tui/logs/sing-box.log` |
| Temp sing-box config | `$XDG_RUNTIME_DIR/kvn-tui-singbox.json` or `/tmp/kvn-tui-singbox.json` |
| Runtime state (waybar) | `~/.config/kvn-tui/state.json` |
| IPC socket (daemon ↔ TUI) | `~/.config/kvn-tui/kvn-tui.sock` or `$XDG_RUNTIME_DIR/kvn-tui.sock` |

---

## Agent Checklist Before Editing

1. Are you preserving atomic file writes for any new config files?
2. Are you using `anyhow::Result` and `tracing` instead of `println!` / `eprintln!`?
3. Are tests added for new public functions, and does `cargo llvm-cov --summary-only` still report **≥ 85 %** region and line coverage?
4. Are you respecting the Arch-only constraint (no support for other distros or BSDs added silently)?
5. Does the sing-box config generation remain valid for sing-box 1.12+?
6. Have you run `cargo fmt` and `cargo clippy --all-targets --all-features` and fixed any warnings?
