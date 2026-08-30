#!/bin/bash
set -euo pipefail

BACKUP_SUFFIX=".bak.before-kvn-tui"
BACKUP_LIMIT=5
OMAKVN_REPO="https://github.com/yarikov/omakvn.git"
declare -A RUN_BACKUPS=()

backup_file() {
  local file="$1" timestamp backup tmp candidate suffix
  [[ -f $file ]] || return 0

  timestamp=$(date +%Y%m%d%H%M%S)
  backup="${file}${BACKUP_SUFFIX}.${timestamp}"
  while [[ -e $backup ]]; do
    sleep 1
    timestamp=$(date +%Y%m%d%H%M%S)
    backup="${file}${BACKUP_SUFFIX}.${timestamp}"
  done

  tmp=$(mktemp "${backup}.tmp.XXXXXX")
  cp -p -- "$file" "$tmp"
  mv -- "$tmp" "$backup"
  RUN_BACKUPS["$file"]=$backup

  local backups=()
  [[ -f $file$BACKUP_SUFFIX ]] && backups+=("$file$BACKUP_SUFFIX")
  for candidate in "${file}${BACKUP_SUFFIX}."*; do
    [[ -f $candidate ]] || continue
    suffix=${candidate#"${file}${BACKUP_SUFFIX}."}
    [[ $suffix =~ ^[0-9]{14}$ ]] && backups+=("$candidate")
  done
  while (( ${#backups[@]} > BACKUP_LIMIT )); do
    rm -- "${backups[0]}"
    backups=("${backups[@]:1}")
  done
}

restore_current_backup() {
  local file="$1"
  local backup=${RUN_BACKUPS["$file"]:-}
  if [[ -n $backup && -f $backup ]]; then
    local tmp
    tmp=$(mktemp "${file}.tmp.XXXXXX")
    cp -p -- "$backup" "$tmp"
    atomic_replace "$tmp" "$file"
  fi
}

atomic_replace() {
  local source="$1" target="$2"
  if [[ -f $target ]]; then
    chmod --reference="$target" "$source"
  fi
  sync -f "$source" 2>/dev/null || true
  mv -f -- "$source" "$target"
  sync -f "$(dirname "$target")" 2>/dev/null || true
}

replace_if_changed() {
  local source="$1" target="$2"
  if [[ -f $target ]] && cmp -s -- "$source" "$target"; then
    rm -- "$source"
    return
  fi
  backup_file "$target"
  atomic_replace "$source" "$target"
}

append_atomic() {
  local target="$1" content="$2" tmp
  tmp=$(mktemp "${target}.tmp.XXXXXX")
  [[ -f $target ]] && cp -- "$target" "$tmp"
  printf '%s' "$content" >>"$tmp"
  replace_if_changed "$tmp" "$target"
}

install_launcher() {
  local generation="$1"
  local launcher="$HOME/.local/bin/omarchy-launch-kvn-tui"
  [[ -f $launcher ]] && { echo "Launcher script already present."; return; }

  echo "Installing launcher script..."
  mkdir -p "$(dirname "$launcher")"
  local tmp
  tmp=$(mktemp "${launcher}.tmp.XXXXXX")
  if (( generation >= 4 )); then
    cat >"$tmp" <<'EOF'
#!/bin/bash
exec omarchy-launch-or-focus-tui --app-id=org.omarchy.kvn-tui kvn-tui
EOF
  else
    cat >"$tmp" <<'EOF'
#!/bin/bash
exec omarchy-launch-or-focus "org.omarchy.kvn-tui" \
  "uwsm-app -- xdg-terminal-exec --app-id=org.omarchy.kvn-tui -e kvn-tui"
EOF
  fi
  chmod 0755 "$tmp"
  atomic_replace "$tmp" "$launcher"
}

detect_omarchy_major() {
  command -v omarchy >/dev/null 2>&1 || {
    echo "Error: omarchy command not found." >&2
    return 1
  }
  local version major
  version=$(omarchy version 2>/dev/null || true)
  major=$(sed -nE 's/^[^0-9]*([0-9]+).*/\1/p' <<<"$version")
  [[ -n $major ]] || {
    echo "Error: could not determine Omarchy version from: ${version:-<empty>}" >&2
    return 1
  }
  printf '%s\n' "$major"
}

install_omarchy_v3() {
  local waybar_config="$HOME/.config/waybar/config.jsonc"
  local waybar_style="$HOME/.config/waybar/style.css"
  local hypr_autostart="$HOME/.config/hypr/autostart.conf"
  local hypr_bindings="$HOME/.config/hypr/bindings.conf"
  local hypr_main="$HOME/.config/hypr/hyprland.conf"

  if [[ -f $waybar_config ]]; then
    if ! grep -q '"custom/kvn-tui"' "$waybar_config"; then
      echo "Adding kvn-tui module to Waybar config..."
      if tail -n 1 "$waybar_config" | grep -q '^}$'; then
        local tmp
        tmp=$(mktemp "${waybar_config}.tmp.XXXXXX")
        cp -- "$waybar_config" "$tmp"
        if grep -q '"bluetooth"' "$tmp"; then
          sed -i '/"modules-right": \[/,/\],/{s/"bluetooth"/"custom\/kvn-tui",\n    "bluetooth"/}' "$tmp"
        fi
        sed -i '$d' "$tmp"
        sed -i '$ s/[[:space:]]*$/,/' "$tmp"
        cat >>"$tmp" <<'EOF'
  "custom/kvn-tui": {
    "exec": "kvn-tui --waybar-status",
    "return-type": "json",
    "interval": 5,
    "on-click": "omarchy-launch-kvn-tui",
    "tooltip-format": "kvn-tui VPN client"
  }
}
EOF
        replace_if_changed "$tmp" "$waybar_config"
      else
        echo "Warning: Waybar config does not end with '}' on its own line; skipping module definition."
      fi
    else
      echo "Waybar module already present."
    fi
  else
    echo "Warning: Waybar config not found at $waybar_config"
  fi

  if [[ -f $waybar_style ]]; then
    if ! grep -q '#custom-kvn-tui' "$waybar_style"; then
      echo "Adding kvn-tui styles to Waybar CSS..."
      append_atomic "$waybar_style" $'\n#custom-kvn-tui {\n  margin-right: 18px;\n}\n'
    else
      echo "Waybar CSS already present."
    fi
  else
    echo "Warning: Waybar style not found at $waybar_style"
  fi

  install_launcher 3

  if [[ -f $hypr_autostart ]] && grep -qE '^[[:space:]]*exec-once[[:space:]]*=[[:space:]]*kvn-tui --daemon[[:space:]]*$' "$hypr_autostart"; then
    echo "Removing legacy kvn-tui daemon entry from Hyprland autostart..."
    local tmp
    tmp=$(mktemp "${hypr_autostart}.tmp.XXXXXX")
    sed '/^[[:space:]]*exec-once[[:space:]]*=[[:space:]]*kvn-tui --daemon[[:space:]]*$/d' "$hypr_autostart" >"$tmp"
    replace_if_changed "$tmp" "$hypr_autostart"
  fi

  if [[ -f $hypr_bindings ]] && grep -q "omarchy-launch-kvn-tui" "$hypr_bindings"; then
    echo "Hyprland keybinding already configured."
  else
    echo
    read -r -p "Add Hyprland keybinding to launch kvn-tui? [y/N] " binding_answer
    if [[ $binding_answer =~ ^[Yy]$ ]]; then
      echo
      echo "Press Enter to accept the default, or type a custom Hyprland keybinding."
      echo "Examples: SUPER CTRL, K    SUPER SHIFT, V    SUPER ALT, K"
      read -r -p "Keybinding (default: SUPER CTRL, K): " binding_input
      binding_input=${binding_input:-SUPER CTRL, K}
      echo "Adding Hyprland keybinding ($binding_input)..."
      mkdir -p "$(dirname "$hypr_bindings")"
      append_atomic "$hypr_bindings" $'\nbind = '"$binding_input"$', exec, omarchy-launch-kvn-tui\n'
    else
      echo "Skipping keybinding."
    fi
  fi

  if [[ -f $hypr_main ]] && ! grep -Fq "org.omarchy.kvn-tui" "$hypr_main"; then
    echo "Adding Hyprland window rule for kvn-tui..."
    append_atomic "$hypr_main" $'\n# kvn-tui: float, center, and size like other Omarchy TUIs\nwindowrule = tag +floating-window, match:class org.omarchy.kvn-tui\n'
  fi

  echo "Restarting Waybar..."
  omarchy restart waybar
  sleep 2
  if ! pgrep -x waybar >/dev/null 2>&1; then
    echo "Error: Waybar failed to start. Restoring backups..." >&2
    restore_current_backup "$waybar_config"
    restore_current_backup "$waybar_style"
    omarchy restart waybar
    return 1
  fi
}

append_marker_block() {
  local file="$1" marker="$2" block="$3"
  if grep -Fq -- "-- kvn-tui ${marker}: begin" "$file"; then
    echo "${marker^} already configured."
    return
  fi
  local tmp
  tmp=$(mktemp "${file}.tmp.XXXXXX")
  cp -- "$file" "$tmp"
  printf '\n%s\n' "$block" >>"$tmp"
  replace_if_changed "$tmp" "$file"
}

plugin_dir_created=0
legacy_plugin_staged=0

# Install or update the standalone Git-managed Quickshell plugin. Legacy
# releases copied the QML files directly; stage that copy until the remote
# install succeeds so a network failure cannot leave the user without it.
install_omarchy_v4_plugin() {
  local dir="$HOME/.config/omarchy/plugins/kvn.tui"
  local origin=""

  omarchy plugin add --help >/dev/null 2>&1 || {
    echo "This Omarchy build lacks the shell plugin registry;" >&2
    echo "falling back to the command bar module." >&2
    return 1
  }

  if [[ -d $dir/.git ]]; then
    origin=$(git -C "$dir" remote get-url origin 2>/dev/null || true)
    case "$origin" in
    https://github.com/yarikov/omakvn | https://github.com/yarikov/omakvn.git | git@github.com:yarikov/omakvn.git)
      echo "Updating the kvn.tui bar plugin from $OMAKVN_REPO..."
      omarchy plugin update kvn.tui --yes
      return 0
      ;;
    *)
      echo "Error: kvn.tui is managed by a different Git repository: ${origin:-<unknown>}" >&2
      echo "Refusing to overwrite $dir." >&2
      return 2
      ;;
    esac
  fi

  if [[ -e $dir ]]; then
    if ! jq -e '.id == "kvn.tui"' "$dir/manifest.json" >/dev/null 2>&1; then
      echo "Error: refusing to replace unrecognized plugin directory: $dir" >&2
      return 2
    fi
    echo "Migrating the embedded kvn.tui plugin to its standalone repository..."
    mv -- "$dir" "$V4_TRANSACTION_DIR/legacy-kvn.tui"
    legacy_plugin_staged=1
  else
    plugin_dir_created=1
  fi

  echo "Installing the kvn.tui bar plugin from $OMAKVN_REPO..."
  if omarchy plugin add "$OMAKVN_REPO" --yes; then
    return 0
  fi

  # `omarchy plugin add` may finish cloning and then fail only because no live
  # shell is available for its final rescan. A valid checkout is installed and
  # will be discovered on the next login, so accept that state.
  if [[ -d $dir/.git ]] && omarchy plugin validate "$dir" >/dev/null 2>&1; then
    echo "Plugin installed; Omarchy Shell will discover it at next login."
    return 0
  fi

  rm -rf -- "$dir"
  if (( legacy_plugin_staged )); then
    mv -- "$V4_TRANSACTION_DIR/legacy-kvn.tui" "$dir"
    legacy_plugin_staged=0
    plugin_dir_created=0
    echo "Warning: remote plugin install failed; restored the existing kvn.tui plugin." >&2
    return 0
  fi

  echo "Remote plugin install failed; falling back to the command bar module." >&2
  return 1
}


install_omarchy_v4() {
  command -v jq >/dev/null 2>&1 || {
    echo "Error: jq is required for Omarchy Shell integration." >&2
    return 1
  }

  local shell_config="$HOME/.config/omarchy/shell.json"
  local hypr_bindings="$HOME/.config/hypr/bindings.lua"
  local hypr_main="$HOME/.config/hypr/hyprland.lua"
  for file in "$shell_config" "$hypr_bindings" "$hypr_main"; do
    [[ -f $file ]] || {
      echo "Error: required Omarchy 4 config not found: $file" >&2
      return 1
    }
  done

  V4_SHELL_CONFIG=$shell_config
  V4_HYPR_BINDINGS=$hypr_bindings
  V4_HYPR_MAIN=$hypr_main
  V4_TRANSACTION_ACTIVE=1
  V4_TRANSACTION_DIR=$(mktemp -d)
  cp -p -- "$shell_config" "$V4_TRANSACTION_DIR/shell.json"
  cp -p -- "$hypr_bindings" "$V4_TRANSACTION_DIR/bindings.lua"
  cp -p -- "$hypr_main" "$V4_TRANSACTION_DIR/hyprland.lua"

  rollback_v4() {
    local snapshot target tmp
    while read -r snapshot target; do
      tmp=$(mktemp "${target}.tmp.XXXXXX")
      cp -p -- "$snapshot" "$tmp"
      atomic_replace "$tmp" "$target"
    done <<EOF
$V4_TRANSACTION_DIR/shell.json $V4_SHELL_CONFIG
$V4_TRANSACTION_DIR/bindings.lua $V4_HYPR_BINDINGS
$V4_TRANSACTION_DIR/hyprland.lua $V4_HYPR_MAIN
EOF
    if (( plugin_dir_created )); then
      rm -rf -- "$HOME/.config/omarchy/plugins/kvn.tui"
    fi
    if (( legacy_plugin_staged )) && [[ -d $V4_TRANSACTION_DIR/legacy-kvn.tui ]]; then
      rm -rf -- "$HOME/.config/omarchy/plugins/kvn.tui"
      mv -- "$V4_TRANSACTION_DIR/legacy-kvn.tui" "$HOME/.config/omarchy/plugins/kvn.tui"
    fi
    hyprctl reload >/dev/null 2>&1 || true
  }

  cleanup_v4() {
    local status=$?
    trap - EXIT
    if (( V4_TRANSACTION_ACTIVE && status != 0 )); then
      echo "Rolling back Omarchy 4 integration changes..." >&2
      rollback_v4
    fi
    rm -rf -- "$V4_TRANSACTION_DIR"
    exit "$status"
  }
  trap cleanup_v4 EXIT

  echo "Adding kvn-tui module to Omarchy Shell..."
  local module plugin_installed=0 tmp
  local plugin_status=0
  install_omarchy_v4_plugin || plugin_status=$?
  if (( plugin_status == 0 )); then
    plugin_installed=1
    module='{"id":"kvn.tui"}'
  elif (( plugin_status == 2 )); then
    return 1
  else
    module='{"id":"kvn-tui","type":"command","exec":"kvn-tui --waybar-status","interval":5,"tooltip":"kvn-tui VPN client","onClick":"omarchy-launch-kvn-tui"}'
  fi
  tmp=$(mktemp "${shell_config}.tmp.XXXXXX")
  jq --argjson module "$module" '
    def entry_id: if type == "object" then (.id // "") else tostring end;
    def kvn_entry: entry_id == "kvn-tui" or entry_id == "kvn.tui";
    .bar.layout.left = (.bar.layout.left // [])
    | .bar.layout.center = (.bar.layout.center // [])
    | .bar.layout.right = (.bar.layout.right // [])
    | ([.bar.layout.left[], .bar.layout.center[], .bar.layout.right[]]
       | map(select(kvn_entry) | if type == "object" then del(.type, .exec, .interval, .tooltip, .onClick) else . end)
       | first // {}) as $existing
    | ($existing + $module) as $entry
    | .bar.layout.left |= map(select(kvn_entry | not))
    | .bar.layout.center |= map(select(kvn_entry | not))
    | .bar.layout.right |= map(select(kvn_entry | not))
    | (.bar.layout.right | map(entry_id)
       | (index("omarchy.bluetooth") // index("omarchy.network"))) as $index
    | if $index == null then
        .bar.layout.right += [$entry]
      else
        .bar.layout.right = (.bar.layout.right[0:$index] + [$entry] + .bar.layout.right[$index:])
      end
  ' "$shell_config" >"$tmp"
  jq -e '.version == 1 and (.bar.layout | type == "object")' "$tmp" >/dev/null
  replace_if_changed "$tmp" "$shell_config"

  # Poke a live shell so the widget appears without a re-login: rescan picks
  # up the freshly installed plugin files, and `bar put` is an idempotent
  # no-op when the watcher already applied the shell.json change above.
  if (( plugin_installed )) && command -v omarchy-shell >/dev/null 2>&1; then
    timeout 15 omarchy-shell shell rescanPlugins >/dev/null 2>&1 || true
    if timeout 15 omarchy bar put kvn.tui --before omarchy.bluetooth >/dev/null 2>&1; then
      echo "Placed kvn.tui on the bar."
    else
      echo "Note: omarchy-shell is not running; kvn.tui appears on the bar at next login."
    fi
  fi

  install_launcher 4

  if grep -Fq -- "-- kvn-tui keybinding: begin" "$hypr_bindings" ||
    grep -Fq "omarchy-launch-kvn-tui" "$hypr_bindings"; then
    echo "Keybinding already configured."
  else
    echo
    read -r -p "Add Hyprland keybinding to launch kvn-tui? [y/N] " binding_answer
    if [[ $binding_answer =~ ^[Yy]$ ]]; then
      echo
      echo "Press Enter to accept the default, or type a custom Hyprland keybinding."
      echo "Examples: SUPER CTRL, K    SUPER SHIFT, V    SUPER ALT, K"
      local binding_input modifiers key binding_lua block
      while true; do
        read -r -p "Keybinding (default: SUPER CTRL, K): " binding_input
        binding_input=${binding_input:-SUPER CTRL, K}
        if [[ $binding_input != *,* ]]; then
          echo "Invalid keybinding. Use the format MODIFIERS, KEY (for example: SUPER SHIFT, V)." >&2
          continue
        fi
        modifiers=${binding_input%%,*}
        key=${binding_input#*,}
        modifiers=$(sed -E 's/^[[:space:]]+//; s/[[:space:]]+$//; s/[[:space:]]+/ + /g' <<<"$modifiers")
        key=$(sed -E 's/^[[:space:]]+//; s/[[:space:]]+$//' <<<"$key")
        if [[ $modifiers =~ ^(SUPER|CTRL|SHIFT|ALT)([[:space:]]\+[[:space:]](SUPER|CTRL|SHIFT|ALT))*$ &&
          $key =~ ^[A-Za-z0-9_:.-]+$ ]]; then
          break
        fi
        echo "Invalid keybinding. Allowed modifiers: SUPER, CTRL, SHIFT, ALT." >&2
      done

      binding_lua="$modifiers + $key"
      if [[ $binding_lua == "SUPER + CTRL + K" ]]; then
        echo "Note: SUPER+CTRL+K is currently bound to Herdr keybindings."
      fi
      echo "The selected shortcut will be explicitly replaced with hl.unbind()."
      printf -v block '%s\n%s\n%s\n%s\n%s' \
        '-- kvn-tui keybinding: begin' \
        '-- Unbind the selected shortcut before assigning kvn-tui.' \
        "hl.unbind(\"$binding_lua\")" \
        "o.bind(\"$binding_lua\", \"kvn-tui VPN client\", \"omarchy-launch-kvn-tui\")" \
        '-- kvn-tui keybinding: end'
      append_marker_block "$hypr_bindings" "keybinding" "$block"
    else
      echo "Skipping keybinding."
    fi
  fi

  append_marker_block "$hypr_main" "window rule" '-- kvn-tui window rule: begin
-- Float, center, and size kvn-tui like other Omarchy TUIs.
o.window("^org\\.omarchy\\.kvn-tui$", { tag = "+floating-window" })
-- kvn-tui window rule: end'

  if command -v hyprctl >/dev/null 2>&1; then
    local before_errors after_errors
    before_errors=$(hyprctl configerrors 2>/dev/null || true)
    if hyprctl reload >/dev/null 2>&1; then
      after_errors=$(hyprctl configerrors 2>/dev/null || true)
      if [[ -n $after_errors && $after_errors != "$before_errors" ]]; then
        echo "Error: Hyprland reported new config errors:" >&2
        echo "$after_errors" >&2
        return 1
      fi
      if [[ -n $after_errors ]]; then
        echo "Warning: Hyprland still reports pre-existing config errors:" >&2
        echo "$after_errors" >&2
      else
        echo "Hyprland configuration reloaded without errors."
      fi
    else
      echo "Warning: no live Hyprland session; Lua changes will apply at the next login."
    fi
  fi

  V4_TRANSACTION_ACTIVE=0
  rm -rf -- "$V4_TRANSACTION_DIR"
  trap - EXIT
}

echo "Installing kvn-tui Omarchy integration..."
omarchy_major=$(detect_omarchy_major)
if (( omarchy_major >= 4 )); then
  install_omarchy_v4
else
  install_omarchy_v3
fi
echo "Done."
