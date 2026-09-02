---
name: update-doc-screenshots
description: Refresh kvn-tui's README screenshot and bundled-theme gallery. Use when documentation screenshots or theme previews need to be regenerated; requires a Hyprland Wayland session.
---

# Update documentation screenshots

Regenerate images from the repository's side-effect-free `docs-preview` command. Never capture the user's normal kvn-tui session or configuration.

1. Read `AGENTS.md` and preserve unrelated work in target files.
2. Run `scripts/capture.sh --check` to validate dependencies and the existing gallery.
3. Immediately before moving windows, changing workspace, or changing the system theme, request approval to run `scripts/capture.sh --all` outside the sandbox. It stashes every normal window in `special:scratchpad`, captures on workspace 1 through Omarchy's configured TUI terminal, temporarily applies the Tokyo Night system theme for the README desktop capture, and restores the exact original theme, background, workspace mapping, and focus on every exit path.
4. Keep `docs/themes.md` synchronized with the alphabetically sorted `themes/*.toml` slugs. The dynamic `omarchy` sentinel gets no screenshot.
5. Inspect `assets/screenshot.png` and the contact sheet emitted by the script for framing, contrast, and unexpected data.
6. Re-run `scripts/capture.sh --check`, relevant Rust checks, and the skill validator.

The README image is a 1920×1080 full-desktop capture. Theme images use the native 875×600 logical Foot window; at the current 1.6 monitor scale they are stored as 1400×960 PNGs in `assets/themes/<slug>.png`. Do not substitute another terminal or override its font. Do not commit unless asked.
