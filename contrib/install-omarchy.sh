#!/bin/bash
set -e

WAYBAR_CONFIG="${HOME}/.config/waybar/config.jsonc"
WAYBAR_STYLE="${HOME}/.config/waybar/style.css"

echo "Installing kvn-tui Omarchy integration..."

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

    # Read accent colors from the current Omarchy theme.
    THEME_COLORS="${HOME}/.config/omarchy/current/theme/colors.toml"
    if [ -f "$THEME_COLORS" ]; then
      COLOR_CONNECTED=$(grep '^color2' "$THEME_COLORS" | head -n1 | cut -d'"' -f2)
      COLOR_DISCONNECTED=$(grep '^color0' "$THEME_COLORS" | head -n1 | cut -d'"' -f2)
    fi
    # Fallbacks if theme colors are not found.
    : "${COLOR_CONNECTED:=#adda78}"
    : "${COLOR_DISCONNECTED:=#72696a}"

    cat >> "$WAYBAR_STYLE" <<EOF

#custom-kvn-tui {
  margin-right: 18px;
}
#custom-kvn-tui.connected {
  color: ${COLOR_CONNECTED};
}
#custom-kvn-tui.disconnected {
  color: ${COLOR_DISCONNECTED};
}
EOF
  else
    echo "Waybar CSS already present."
  fi
else
  echo "Warning: waybar style not found at $WAYBAR_STYLE"
fi

# ── Restart waybar ──
if command -v omarchy &> /dev/null; then
  echo "Restarting waybar..."
  omarchy restart waybar
else
  echo "Warning: omarchy command not found. Please restart waybar manually."
fi

echo "Done."
