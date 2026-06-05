#!/bin/bash
set -e

WAYBAR_CONFIG="${HOME}/.config/waybar/config.jsonc"
WAYBAR_STYLE="${HOME}/.config/waybar/style.css"
HYPR_AUTOSTART="${HOME}/.config/hypr/autostart.conf"

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
    "on-click": "omarchy-launch-or-focus-tui sudo kvn-tui",
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

# ── Desktop entry for Walker / Super+Space ──
DESKTOP_FILE="${HOME}/.local/share/applications/kvn-tui.desktop"
if [ ! -f "$DESKTOP_FILE" ]; then
  echo "Installing desktop entry..."
  mkdir -p "$(dirname "$DESKTOP_FILE")"
  cat > "$DESKTOP_FILE" <<'EOF'
[Desktop Entry]
Name=kvn-tui
Comment=Terminal VPN client
Exec=omarchy-launch-or-focus-tui sudo kvn-tui
Type=Application
Terminal=false
Categories=Network;VPN;
Keywords=vpn;network;sing-box;vless;
Icon=network-vpn-symbolic
EOF
else
  echo "Desktop entry already present."
fi

# ── Hyprland autostart ──
echo
read -r -p "Enable kvn-tui autostart on login? [y/N] " autostart_answer
if [[ "$autostart_answer" =~ ^[Yy]$ ]]; then
  echo
  echo "Choose workspace:"
  echo "  1-5  — regular workspace number"
  echo "  s    — special:scratchpad (default)"
  read -r -p "Workspace [s]: " workspace_answer
  workspace_answer=${workspace_answer:-s}

  case "$workspace_answer" in
    1|2|3|4|5)
      exec_line="exec-once = [workspace $workspace_answer silent] omarchy-launch-or-focus-tui sudo kvn-tui"
      ;;
    s|S|scratchpad|"")
      exec_line="exec-once = [workspace special:scratchpad silent] omarchy-launch-or-focus-tui sudo kvn-tui"
      ;;
    *)
      echo "Invalid choice. Skipping autostart."
      exec_line=""
      ;;
  esac

  if [ -n "$exec_line" ]; then
    if [ -f "$HYPR_AUTOSTART" ] && grep -q "omarchy-launch-or-focus-tui sudo kvn-tui" "$HYPR_AUTOSTART"; then
      echo "Hyprland autostart entry already present."
    else
      echo "Adding kvn-tui to hyprland autostart..."
      mkdir -p "$(dirname "$HYPR_AUTOSTART")"
      printf '\n%s\n' "$exec_line" >> "$HYPR_AUTOSTART"
    fi
  fi
else
  echo "Skipping autostart."
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
