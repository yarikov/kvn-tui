# Agent Guide: kvn-tui

This document contains project-specific context and conventions for AI coding agents. It supplements `README.md` with architectural details, coding styles, and rules of thumb.

---

## Project Overview

`kvn-tui` (v0.5.2) is a **terminal VPN client** for Arch Linux + Wayland. It is a Rust TUI application that manages VPN profiles, generates sing-box configurations, and orchestrates the `sing-box` binary as a child process. Navigation is vim-style (`j`/`k`/`g`/`G`).

The app does **not** implement VPN protocols itself. It is a configuration generator and process manager around the external `sing-box` binary.

---

## Module Map

| Module | Path | Responsibility |
|--------|------|----------------|
| `cli` | `src/cli.rs` | CLI argument parsing (`--waybar-status`, `--install-omarchy`, `--version`) |
| `app` | `src/app.rs`, `src/app/model.rs`, `src/app/msg.rs`, `src/app/update.rs`, `src/app/effect.rs` | TEA core: Model, Msg, Update, Effect — pure data, messages, business logic, side-effect declarations |
| `model` | `src/app/model.rs` | Application state (`Model`), overlay + connection state, input state — pure data, no side effects |
| `msg` | `src/app/msg.rs` | Message enum (`Msg`) — all external events (keys, ticks, logs, geo, resume, etc.) |
| `update` | `src/app/update.rs` | Pure `update(model, msg) -> Vec<Effect>` — business logic, input routing, mode transitions |
| `effect` | `src/app/effect.rs` | Effect enum — declarative description of side effects to be executed by runtime |
| `runtime` | `src/runtime.rs` | TUI main loop: owns `mpsc` channel, spawns threads, renders UI, executes effects |
| `process_handle` | `src/infra/process_handle.rs` | Wrapper around `std::process::Child` for sing-box lifecycle |
| `ui` | `src/ui.rs`, `src/ui/layout.rs`, `src/ui/widgets.rs`, `src/ui/styles.rs`, `src/ui/nav.rs` | ratatui rendering, layout splits, widget definitions, color theme, navigation helpers |
| `config` | `src/config.rs`, `src/config/profile.rs` | JSON config I/O, profile struct definitions |
| `singbox` | `src/singbox.rs`, `src/singbox/config.rs`, `src/singbox/runner.rs` | Process lifecycle: write temp config, run `sing-box check`, spawn `sing-box run`, kill on disconnect |
| `clipboard` | `src/infra/clipboard.rs` | Wayland clipboard integration (`wl-paste`), VLESS share link parsing (`vless://`) |
| `geo` | `src/infra/geo.rs` | Download and cache geoip/geosite rule-sets for sing-box routing |
| `editor` | `src/infra/editor.rs` | Launch `$EDITOR` / `$VISUAL` on `profiles.json`, temporarily restore terminal |
| `paths` | `src/infra/paths.rs` | XDG directory resolution (`~/.config/kvn-tui/`), atomic path construction |
| `waybar` | `src/services/waybar.rs` | Read/write `state.json` for waybar integration and crash recovery |
| `suspend` | `src/services/suspend.rs` | D-Bus listener for `systemd-logind` `PrepareForSleep` signals (zbus) |
| `services` | `src/services.rs`, `src/services/log_tailer.rs`, `src/services/waybar.rs`, `src/services/suspend.rs` | Background services: log tailer, waybar state I/O, suspend watcher |
| `infra` | `src/infra.rs`, `src/infra/clipboard.rs`, `src/infra/editor.rs`, `src/infra/geo.rs`, `src/infra/paths.rs`, `src/infra/process_handle.rs`, `src/infra/user_env.rs` | Infrastructure utilities: clipboard, editor, geo, paths, process handle, user env |

---

## Build System & Dependencies

- **Rust**: edition 2024, minimum version 1.87
- **External binary**: `sing-box` must be installed separately and available on `$PATH` (or via `SING_BOX_PATH` env var)
- **Key crates**: `ratatui` + `crossterm` (TUI), `serde` + `serde_json` (config), `zbus` (D-Bus), `ureq` (HTTP), `tracing` (logs), `anyhow` + `thiserror` (errors)

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
- `src/test_helpers.rs` provides shared test utilities (e.g., `model_with_profiles`).
- Tests should not depend on external network or the `sing-box` binary unless explicitly marked `#[ignore]`.
- Use `tempfile` for file-system tests; use `NamedTempFile` / `tempdir()` for isolation.
- Example pattern: create a default `Profile`, generate a config, assert on JSON structure.

---

## Key Design Decisions

### TEA Architecture
The application follows **The Elm Architecture (TEA)**:
1. **Model** (`app/model.rs`) holds all application state as pure data. UI state is split into `Overlay` (popup/modal) and `ConnectionState` (idle/connecting/connected).
2. **Messages** (`app/msg.rs`) represent every external event — keyboard input, timer ticks, log lines, geo updates, system resume.
3. **Update** (`app/update.rs`) is a pure function `update(model, msg) -> Vec<Effect>`: no I/O, no threads, no system calls. All business logic lives here.
4. **Effects** (`app/effect.rs`) are declarative descriptions of side effects (`Connect`, `DownloadGeo`, `SaveConfig`, `Quit`, etc.).
5. **Runtime** (`runtime.rs`) owns the `mpsc` channel, spawns background threads (event reader, ticker, suspend watcher), renders the UI, and executes effects by performing the actual I/O. The sing-box process handle lives in an `Arc<Mutex<Option<ProcessHandle>>>` inside the runtime, not in `Model`.

This separation makes `update.rs` fully synchronous and trivial to unit-test.

### Background Services
Background work is executed in dedicated threads spawned by `runtime.rs`:
- **Event reader** — polls `crossterm` events with `event::poll` and sends `Msg::Key` / `Msg::Resize`. Reading can be paused via an `AtomicBool` flag (used when opening an external editor so that keystrokes meant for the editor do not accumulate in the channel).
- **Ticker** — sends `Msg::Tick` every 250 ms to drive log tailing and connection state machines.
- **Suspend watcher** — `services/suspend.rs` runs a blocking zbus listener that sends `Msg::SystemResumed`.
- **Effects** — `Connect` (with optional `force_restart`), `DownloadGeo`, and `PasteClipboard` each spawn a short-lived thread that sends the result back via the same channel.
- **Log tailer** — `LogTailer` (`services/log_tailer.rs`) reads new lines from the sing-box log file on every `Tick`.
- **State I/O** — `services/waybar.rs` writes `state.json` on connect/disconnect for waybar integration.

### sing-box Config Generation
- `singbox::config::generate_config` builds a complete sing-box 1.12+ JSON object from a `Profile` and `Settings`.
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
- `services/suspend.rs` runs a blocking zbus listener in a dedicated thread. On resume (`PrepareForSleep` with `false`), it sends `Msg::SystemResumed` through the `mpsc` channel so `update.rs` can schedule a reconnect effect.

### State I/O
- `services/waybar.rs` writes a small JSON file (`state.json`) on every connect/disconnect. It stores connection status, active profile name, and sing-box PID.
- Used by the `--waybar-status` CLI flag and for crash recovery (state is cleared on startup).

---

## Configuration Paths

| Resource | Path |
|----------|------|
| Profiles & settings | `~/.config/kvn-tui/profiles.json` |
| Geo rule-sets | `~/.config/kvn-tui/geo/` |
| sing-box logs | `~/.config/kvn-tui/logs/sing-box.log` |
| Temp sing-box config | `$XDG_RUNTIME_DIR/kvn-tui-singbox.json` or `/tmp/kvn-tui-singbox.json` |
| Runtime state (waybar) | `~/.config/kvn-tui/state.json` |

---

## Agent Checklist Before Editing

1. Are you preserving atomic file writes for any new config files?
2. Are you using `anyhow::Result` and `tracing` instead of `println!` / `eprintln!`?
3. Are tests added for new public functions?
4. Are you respecting the Arch + Wayland constraint (no X11 fallbacks added silently)?
5. Does the sing-box config generation remain valid for sing-box 1.12+?
