# Agent Guide: kvn-tui

This document contains project-specific context and conventions for AI coding agents. It supplements `README.md` with architectural details, coding styles, and rules of thumb.

---

## Project Overview

`kvn-tui` (v0.10.0) is a **terminal VPN client** for Arch Linux + Wayland. It is a Rust TUI application that manages VPN profiles, generates sing-box configurations, and orchestrates the `sing-box` binary as a child process. Navigation is vim-style (`j`/`k`/`g`/`G`).

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
| `daemon` | `src/daemon.rs` | Headless daemon: owns sing-box process, config, mpsc channel, IPC server, background services |
| `tui_client` | `src/tui_client.rs` | TUI client: connects to daemon via Unix socket, renders UI, forwards input, reads clipboard |
| `ipc` | `src/ipc.rs` | NDJSON protocol over Unix domain socket for daemon ↔ TUI client communication |
| `test_helpers` | `src/test_helpers.rs` | Shared test utilities (e.g. `model_with_profiles`)
| `process_handle` | `src/infra/process_handle.rs` | Wrapper around `std::process::Child` for sing-box lifecycle |
| `ui` | `src/ui.rs`, `src/ui/layout.rs`, `src/ui/widgets.rs`, `src/ui/styles.rs`, `src/ui/nav.rs` | ratatui rendering (used by TUI client only), layout splits, widget definitions, color theme, navigation helpers |
| `config` | `src/config.rs`, `src/config/profile.rs` | JSON config I/O, profile struct definitions |
| `singbox` | `src/singbox.rs`, `src/singbox/config.rs`, `src/singbox/runner.rs` | Process lifecycle: write temp config, run `sing-box check`, spawn `sing-box run`, kill on disconnect |
| `clipboard` | `src/infra/clipboard.rs` | Wayland clipboard integration (`wl-paste`), VLESS share link parsing (`vless://`) |
| `geo` | `src/infra/geo.rs` | Download and cache geoip/geosite rule-sets for sing-box routing |
| `editor` | `src/infra/editor.rs` | Launch `$EDITOR` / `$VISUAL` on `profiles.json`, temporarily restore terminal |
| `paths` | `src/infra/paths.rs` | XDG directory resolution (`~/.config/kvn-tui/`), atomic path construction |
| `waybar` | `src/services/waybar.rs` | Read/write `state.json` for waybar integration and crash recovery |
| `suspend` | `src/services/suspend.rs` | D-Bus listener for `systemd-logind` `PrepareForSleep` signals (zbus) |
| `services` | `src/services.rs`, `src/services/log_tailer.rs`, `src/services/waybar.rs`, `src/services/suspend.rs` | Background services: log tailer, waybar state I/O, suspend watcher (all run inside the daemon) |
| `infra` | `src/infra.rs`, `src/infra/clipboard.rs`, `src/infra/editor.rs`, `src/infra/geo.rs`, `src/infra/paths.rs`, `src/infra/process_handle.rs`, `src/infra/user_env.rs` | Infrastructure utilities: clipboard (TUI client), editor (TUI client), geo, paths, process handle, user env |

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

## Release Process

See the `release` skill in `.agents/skills/release/SKILL.md` for the full version-bump and tagging workflow. Supports auto-bump by semver level (major / minor / patch) or explicit version.

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
5. **Daemon** (`daemon.rs`) owns the canonical `Model`, the `mpsc` channel, the sing-box `process_slot`, and all background services (ticker, suspend watcher, log tailer, IPC server). It exposes a Unix domain socket IPC server (`ipc.rs`) that accepts NDJSON commands from TUI clients.
6. **TUI Client** (`tui_client.rs`) connects to the daemon socket, enters the alternate screen, renders the UI using ratatui, and forwards keyboard input (plus clipboard/editor actions) as IPC commands. It has its own local `Model` that is kept in sync via `StateSnapshot` broadcasts from the daemon.
7. **IPC Protocol** (`ipc.rs`) uses newline-delimited JSON over a Unix socket. Commands: `Attach`, `Detach`, `Key`, `Paste`, `ReloadConfig`, `Quit`. Responses: `StateSnapshot` pushed by the daemon after every state change.

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

### Routing Modes
- `RoutingMode::Global` — all traffic through VPN.
- `RoutingMode::BypassRu` — RU IPs/domains bypass VPN (direct).
- `RoutingMode::OnlyRu` — only RU IPs/domains go through VPN; everything else is direct.
- `RoutingMode::BypassCn` — CN IPs/domains bypass VPN (direct).
- `RoutingMode::OnlyCn` — only CN IPs/domains go through VPN; everything else is direct.
- The available routing modes depend on the selected **geo region** (`Ru`, `Cn`, or `Other`). `RoutingMode::available(region)` returns the list dynamically.
- Rule-sets are local `.srs` binary files downloaded to `~/.config/kvn-tui/geo/`.

### Clipboard Parsing
- Only `vless://` URIs are supported.
- The parser extracts: UUID, host, port, fragment (name), `flow`, `security`, `fp` (fingerprint), `type` (transport), `serviceName`, and REALITY params (`pbk`, `sid`, `sni`, `spx`).

### Suspend / Resume
- `services/suspend.rs` runs a blocking zbus listener in a dedicated thread. On resume (`PrepareForSleep` with `false`), it sends `Msg::SystemResumed` through the `mpsc` channel so `update.rs` can schedule a reconnect effect.

### State I/O
- `services/waybar.rs` writes a small JSON file (`state.json`) on every connect/disconnect. It stores connection status, active profile name, and sing-box PID.
- Used by the `--waybar-status` CLI flag and for crash recovery (state is cleared on startup).

### Daemon + TUI Client Architecture
- **Daemon** (`sudo kvn-tui --daemon`) runs headless. It owns the sing-box process, config, geo updates, suspend/resume handling, and log tailing. It binds a Unix domain socket for IPC.
- **TUI Client** (`sudo kvn-tui`) connects to the daemon socket, requests a state snapshot (`Attach`), enters the alternate screen, and renders the UI. Keyboard input is forwarded to the daemon as `IpcCommand::Key` (except `p` and `e`, which are handled locally because they need terminal/Wayland access).
- Pressing `q` (or `Esc`) when no overlay is shown sends `Detach` to the daemon, leaves the alternate screen, disables raw mode, and **exits the TUI process**. The daemon and sing-box keep running. Shell regains the prompt immediately because the foreground `sudo` process actually exits. If an overlay is open (Help, ConfirmDelete, RoutingMode, GeoRegions, Error), `q`/`Esc` is forwarded to the daemon as a normal key, which closes the overlay.
- Pressing `Ctrl+C` sends `Quit` to the daemon. The daemon stops sing-box, cleans up the Unix socket, and exits. The TUI waits briefly (300 ms) for cleanup to complete before exiting.
- Running `sudo kvn-tui` again connects to the same daemon and re-attaches, restoring the TUI instantly without restarting sing-box.
- The IPC protocol is NDJSON over a Unix socket. The daemon pushes a full `StateSnapshot` after every state change. The snapshot includes the complete config (`profiles` and `settings`) so the TUI client always renders the current data.
- `handle_ipc_command` unconditionally appends `Effect::BroadcastState` to every IPC command result, ensuring the daemon always pushes state after user interaction.
- `handle_geo_result`, `Msg::ConnectFailed`, and the `handle_tick` idle fallback also append `Effect::BroadcastState` so state mutations that don't produce other broadcast-triggering effects are still visible to the TUI.

### Geo Region Selection
- `settings.geo_region` (`Option<GeoRegion>`) controls which country rule-sets are downloaded and which routing modes are shown.
- `GeoRegion::Ru` — download RU geoip/geosite, enable `Global` / `BypassRu` / `OnlyRu`.
- `GeoRegion::Cn` — download CN geoip/geosite, enable `Global` / `BypassCn` / `OnlyCn`.
- `GeoRegion::Other` — skip geo downloads, only `Global` mode is available.
- On first launch (when `geo_region` is `None`), a modal overlay forces the user to pick a region before the main UI is usable.
- The region can be changed at runtime with the `o` keybinding.

### Auto-Connect
- `settings.auto_connect` (persisted in `profiles.json`) controls whether the app reconnects to the last used profile on startup.
- `settings.last_connected_profile` stores the UUID of the most recently connected profile. It is updated in `update.rs` on `Msg::Connected` and saved via `Effect::SaveConfig`.
- `Model::new()` calls `resolve_startup_state()` to check `auto_connect` + `last_connected_profile`. If both are set and the profile exists, the model starts in `ConnectionState::Connecting` with that profile pre-selected, and the status bar shows `Auto-connecting to {name}…`.
- The user can toggle `auto_connect` at runtime with the `a` keybinding, which triggers `Effect::SaveConfig` immediately.

---

## Configuration Paths

| Resource | Path |
|----------|------|
| Profiles & settings | `~/.config/kvn-tui/profiles.json` |
| Geo rule-sets | `~/.config/kvn-tui/geo/` |
| sing-box logs | `~/.config/kvn-tui/logs/sing-box.log` |
| Temp sing-box config | `$XDG_RUNTIME_DIR/kvn-tui-singbox.json` or `/tmp/kvn-tui-singbox.json` |
| Runtime state (waybar) | `~/.config/kvn-tui/state.json` |
| IPC socket (daemon ↔ TUI) | `~/.config/kvn-tui/kvn-tui.sock` (under `SUDO_USER`) or `$XDG_RUNTIME_DIR/kvn-tui.sock` |

---

## Agent Checklist Before Editing

1. Are you preserving atomic file writes for any new config files?
2. Are you using `anyhow::Result` and `tracing` instead of `println!` / `eprintln!`?
3. Are tests added for new public functions?
4. Are you respecting the Arch + Wayland constraint (no X11 fallbacks added silently)?
5. Does the sing-box config generation remain valid for sing-box 1.12+?
