#!/bin/bash
set -e

WAYBAR_CONFIG="${HOME}/.config/waybar/config.jsonc"
WAYBAR_STYLE="${HOME}/.config/waybar/style.css"
HYPR_AUTOSTART="${HOME}/.config/hypr/autostart.conf"
HYPR_BINDINGS="${HOME}/.config/hypr/bindings.conf"
HYPR_MAIN="${HOME}/.config/hypr/hyprland.conf"

backup_file() {
  local file="$1"
  if [ -f "$file" ] && [ ! -f "$file.bak.before-kvn-tui" ]; then
    cp "$file" "$file.bak.before-kvn-tui"
  fi
}

restore_file() {
  local file="$1"
  if [ -f "$file.bak.before-kvn-tui" ]; then
    cp "$file.bak.before-kvn-tui" "$file"
  fi
}

echo "Installing kvn-tui Omarchy integration..."

# ── Backup waybar & hyprland files ──
backup_file "$WAYBAR_CONFIG"
backup_file "$WAYBAR_STYLE"
backup_file "$HYPR_AUTOSTART"
backup_file "$HYPR_BINDINGS"
backup_file "$HYPR_MAIN"

# ── Waybar module ──
if [ -f "$WAYBAR_CONFIG" ]; then
  if ! grep -q '"custom/kvn-tui"' "$WAYBAR_CONFIG"; then
    echo "Adding kvn-tui module to waybar config..."

    # Add module reference to modules-right before "bluetooth"
    if grep -q '"bluetooth"' "$WAYBAR_CONFIG"; then
      # Only replace within the modules-right array to avoid breaking other "bluetooth" keys.
      sed -i '/"modules-right": \[/,/\],/{s/"bluetooth"/"custom\/kvn-tui",\n    "bluetooth"/}' "$WAYBAR_CONFIG"
    fi

    # Add module definition before the last closing brace.
    # This assumes the file ends with '}' on its own line.
    if tail -n 1 "$WAYBAR_CONFIG" | grep -q '^}$'; then
      tmp=$(mktemp)
      head -n -1 "$WAYBAR_CONFIG" > "$tmp"
      # Ensure the new last line ends with a comma so the appended module is valid JSON.
      sed -i '$ s/[[:space:]]*$/,/' "$tmp"
      cat >> "$tmp" <<'EOF'
  "custom/kvn-tui": {
    "exec": "sudo kvn-tui --waybar-status",
    "return-type": "json",
    "interval": 5,
    "on-click": "omarchy-launch-kvn-tui",
    "tooltip-format": "kvn-tui VPN client"
  }
}
EOF
      mv "$tmp" "$WAYBAR_CONFIG"
    else
      echo "Warning: waybar config does not end with '}' on its own line. Skipping module definition."
    fi
  else
    echo "Waybar module already present."
  fi
else
  echo "Warning: waybar config not found at $WAYBAR_CONFIG"
fi

# ── Waybar CSS ──
if [ -f "$WAYBAR_STYLE" ]; then
  if ! grep -q '#custom-kvn-tui' "$WAYBAR_STYLE"; then
    echo "Adding kvn-tui styles to waybar CSS..."

    cat >> "$WAYBAR_STYLE" <<'EOF'

#custom-kvn-tui {
  margin-right: 18px;
}
EOF
  else
    echo "Waybar CSS already present."
  fi
else
  echo "Warning: waybar style not found at $WAYBAR_STYLE"
fi

# ── Launcher script ──
LAUNCHER_SCRIPT="${HOME}/.local/bin/omarchy-launch-kvn-tui"
if [ ! -f "$LAUNCHER_SCRIPT" ]; then
  echo "Installing launcher script..."
  mkdir -p "$(dirname "$LAUNCHER_SCRIPT")"
  cat > "$LAUNCHER_SCRIPT" <<'EOF'
#!/bin/bash
exec omarchy-launch-or-focus "org.omarchy.kvn-tui" \
  "uwsm-app -- xdg-terminal-exec --app-id=org.omarchy.kvn-tui -e sudo kvn-tui"
EOF
  chmod +x "$LAUNCHER_SCRIPT"
else
  echo "Launcher script already present."
fi

# ── Hyprland autostart (daemon) ──
autostart_already=false

if [ -f "$HYPR_AUTOSTART" ] && grep -q "kvn-tui --daemon" "$HYPR_AUTOSTART"; then
  autostart_already=true
  echo "Hyprland autostart already configured."
fi

if [ "$autostart_already" = false ]; then
  echo
  read -r -p "Enable kvn-tui daemon autostart on login? [y/N] " autostart_answer
  if [[ "$autostart_answer" =~ ^[Yy]$ ]]; then
    echo "Adding kvn-tui daemon to hyprland autostart..."
    mkdir -p "$(dirname "$HYPR_AUTOSTART")"
    printf '\n%s\n' "exec-once = sudo kvn-tui --daemon" >> "$HYPR_AUTOSTART"
  else
    echo "Skipping autostart."
  fi
fi

# ── Hyprland keybinding ──
if [ -f "$HYPR_BINDINGS" ] && grep -q "omarchy-launch-kvn-tui" "$HYPR_BINDINGS"; then
  echo "Hyprland keybinding already configured."
else
  echo
  read -r -p "Add Hyprland keybinding to launch kvn-tui? [y/N] " binding_answer
  if [[ "$binding_answer" =~ ^[Yy]$ ]]; then
    echo
    echo "Press Enter to accept the default, or type a custom Hyprland keybinding."
    echo "Examples: SUPER CTRL, K    SUPER SHIFT, V    SUPER ALT, K"
    read -r -p "Keybinding (default: SUPER CTRL, K): " binding_input
    binding_input=${binding_input:-SUPER CTRL, K}

    binding_line="bind = ${binding_input}, exec, omarchy-launch-kvn-tui"

    echo "Adding Hyprland keybinding ($binding_input)..."
    mkdir -p "$(dirname "$HYPR_BINDINGS")"
    printf '\n%s\n' "$binding_line" >> "$HYPR_BINDINGS"
  else
    echo "Skipping keybinding."
  fi
fi

# ── Hyprland window rule ──
HYPR_MAIN="${HOME}/.config/hypr/hyprland.conf"
if [ -f "$HYPR_MAIN" ] && ! grep -q "org.omarchy.kvn-tui" "$HYPR_MAIN"; then
  echo "Adding Hyprland window rule for kvn-tui..."
  printf '\n# kvn-tui: float, center, and size like other Omarchy TUIs\nwindowrule = tag +floating-window, match:class org.omarchy.kvn-tui\n' >> "$HYPR_MAIN"
fi

# ── Restart waybar ──
if command -v omarchy &> /dev/null; then
  echo "Restarting waybar..."
  omarchy restart waybar
  sleep 2
  if ! pgrep -x waybar > /dev/null 2>&1; then
    echo "Error: waybar failed to start. Restoring backups..."
    restore_file "$WAYBAR_CONFIG"
    restore_file "$WAYBAR_STYLE"
    omarchy restart waybar
    echo "Backups restored. Please check waybar config manually."
    exit 1
  fi
else
  echo "Warning: omarchy command not found. Please restart waybar manually."
fi

echo "Done."
