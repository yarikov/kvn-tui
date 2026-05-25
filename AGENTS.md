# Agent Guide: kvn-tui

This document contains project-specific context and conventions for AI coding agents. It supplements `README.md` with architectural details, coding styles, and rules of thumb.

---

## Project Overview

`kvn-tui` is a **terminal VPN client** for Arch Linux + Wayland. It is a Rust TUI application that manages VPN profiles, generates sing-box configurations, and orchestrates the `sing-box` binary as a child process. Navigation is vim-style (`j`/`k`/`g`/`G`).

The app does **not** implement VPN protocols itself. It is a configuration generator and process manager around the external `sing-box` binary.

---

## Module Map

| Module | Path | Responsibility |
|--------|------|----------------|
| `app` | `src/app/mod.rs`, `src/app/services.rs` | Application state (`App`), mode enum, background services (log tailer, health checker, geo updater, suspend watcher) |
| `ui` | `src/ui/mod.rs`, `src/ui/layout.rs`, `src/ui/widgets.rs`, `src/ui/styles.rs`, `src/ui/nav.rs` | ratatui rendering, layout splits, widget definitions, color theme, navigation helpers |
| `input` | `src/input/mod.rs` | crossterm event handling, vim key bindings, mode-specific input routing |
| `config` | `src/config/mod.rs`, `src/config/profile.rs`, `src/config/singbox.rs` | JSON config I/O, profile struct definitions, sing-box JSON config generation |
| `singbox` | `src/singbox/mod.rs`, `src/singbox/runner.rs` | Process lifecycle: write temp config, run `sing-box check`, spawn `sing-box run`, kill on disconnect |
| `clipboard` | `src/clipboard/mod.rs` | Wayland clipboard integration (`wl-paste`), VLESS share link parsing (`vless://`) |
| `geo` | `src/geo/mod.rs` | Download and cache geoip/geosite rule-sets for sing-box routing |
| `editor` | `src/editor/mod.rs` | Launch `$EDITOR` / `$VISUAL` on `profiles.json`, temporarily restore terminal |
| `paths` | `src/paths.rs` | XDG directory resolution (`~/.config/kvn-tui/`), atomic path construction |
| `suspend` | `src/suspend.rs` | D-Bus listener for `systemd-logind` `PrepareForSleep` signals (zbus) |

---

## Build System & Dependencies

- **Rust**: edition 2021, minimum version 1.87
- **External binary**: `sing-box` must be installed separately and available on `$PATH` (or via `SING_BOX_PATH` env var)
- **Key crates**: `tokio` (async), `ratatui` + `crossterm` (TUI), `serde` + `serde_json` (config), `zbus` (D-Bus), `reqwest` (HTTP), `tracing` (logs), `anyhow` + `thiserror` (errors)

Build release:

```bash
cargo build --release
```

Run (must be root for TUN):

```bash
sudo ./target/release/kvn-tui
```

---

## Platform Constraints

**Arch Linux on Wayland only.** Do not add X11-specific clipboard tools or generic Linux abstractions without explicit user request.

- Clipboard: hard dependency on `wl-paste`
- Power events: listens to `org.freedesktop.login1.Manager.PrepareForSleep` via zbus
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
- Enums use `#[serde(rename_all = "snake_case")]` or `"lowercase"` as appropriate.

### Naming
- Modules are snake_case (`singbox`, not `sing_box`).
- The binary name is `kvn-tui`; the crate name is `kvn-tui`.

---

## Testing Patterns

- Tests are co-located in `#[cfg(test)] mod tests` blocks at the bottom of each source file.
- `src/test_helpers.rs` provides shared test utilities (e.g., `app_with_profiles`).
- Tests should not depend on external network or the `sing-box` binary unless explicitly marked `#[ignore]`.
- Use `tempfile` for file-system tests; use `NamedTempFile` / `tempdir()` for isolation.
- Example pattern: create a default `Profile`, generate a config, assert on JSON structure.

---

## Key Design Decisions

### sing-box Config Generation
- `config::singbox::generate_config` builds a complete sing-box 1.12+ JSON object from a `Profile` and `Settings`.
- The config is written to a temp file (`/tmp/kvn-tui-singbox.json` or `$XDG_RUNTIME_DIR`), validated with `sing-box check`, and only then is `sing-box run` spawned.
- If the process exits immediately, stderr is captured and surfaced to the user.

### Routing Modes
- `RoutingMode::Global` — all traffic through VPN.
- `RoutingMode::BypassRu` — RU IPs/domains bypass VPN (direct).
- `RoutingMode::OnlyRu` — only RU IPs/domains go through VPN; everything else is direct.
- Rule-sets are local `.srs` binary files downloaded to `~/.config/kvn-tui/geo/`.

### Clipboard Parsing
- Only `vless://` URIs are supported.
- The parser extracts: UUID, host, port, fragment (name), `flow`, `security`, `fp` (fingerprint), `type` (transport), `serviceName`, and REALITY params (`pbk`, `sid`, `sni`, `spx`).

### Suspend / Resume
- `suspend.rs` spawns an async zbus listener. On resume (`PrepareForSleep` with `false`), it signals the main loop via an `mpsc` channel so the app can reconnect.

---

## Configuration Paths

| Resource | Path |
|----------|------|
| Profiles & settings | `~/.config/kvn-tui/profiles.json` |
| Geo rule-sets | `~/.config/kvn-tui/geo/` |
| sing-box logs | `~/.config/kvn-tui/logs/sing-box.log` |
| Temp sing-box config | `$XDG_RUNTIME_DIR/kvn-tui-singbox.json` or `/tmp/kvn-tui-singbox.json` |

---

## Agent Checklist Before Editing

1. Are you preserving atomic file writes for any new config files?
2. Are you using `anyhow::Result` and `tracing` instead of `println!` / `eprintln!`?
3. Are tests added for new public functions?
4. Are you respecting the Arch + Wayland constraint (no X11 fallbacks added silently)?
5. Does the sing-box config generation remain valid for sing-box 1.12+?
