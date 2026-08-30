#!/bin/bash
set -e

BACKUP_SUFFIX=".bak.before-kvn-tui"
FILES=(
  "${HOME}/.config/omarchy/shell.json"
  "${HOME}/.config/hypr/bindings.lua"
  "${HOME}/.config/hypr/hyprland.lua"
  "${HOME}/.config/waybar/config.jsonc"
  "${HOME}/.config/waybar/style.css"
  "${HOME}/.config/hypr/autostart.conf"
  "${HOME}/.config/hypr/bindings.conf"
  "${HOME}/.config/hypr/hyprland.conf"
)

removed=0
for file in "${FILES[@]}"; do
  for backup in "${file}${BACKUP_SUFFIX}" "${file}${BACKUP_SUFFIX}."*; do
    if [[ ! -f $backup ]]; then
      continue
    fi
    if [[ $backup != "${file}${BACKUP_SUFFIX}" ]]; then
      suffix=${backup#"${file}${BACKUP_SUFFIX}."}
      [[ $suffix =~ ^[0-9]{14}$ ]] || continue
    fi
    rm -- "$backup"
    echo "Removed $backup"
    removed=$((removed + 1))
  done
done

if [ "$removed" -eq 0 ]; then
  echo "No kvn-tui Omarchy backup files found."
else
  echo "Removed $removed kvn-tui Omarchy backup file(s)."
fi

for plugin_id in omakvn kvn.tui; do
  plugin_dir="${HOME}/.config/omarchy/plugins/${plugin_id}"
  if [ -d "$plugin_dir" ]; then
    rm -rf -- "$plugin_dir"
    echo "Removed $plugin_dir (the $plugin_id bar plugin)."
  fi
done

shell_config="${HOME}/.config/omarchy/shell.json"
if [ -f "$shell_config" ] && command -v jq >/dev/null 2>&1; then
  tmp=$(mktemp "${shell_config}.tmp.XXXXXX")
  jq '
    def entry_id: if type == "object" then (.id // "") else tostring end;
    def is_kvn: entry_id == "omakvn" or entry_id == "kvn.tui";
    .bar.layout.left = ((.bar.layout.left // []) | map(select(is_kvn | not)))
    | .bar.layout.center = ((.bar.layout.center // []) | map(select(is_kvn | not)))
    | .bar.layout.right = ((.bar.layout.right // []) | map(select(is_kvn | not)))
    | .plugins = ((.plugins // []) | map(select(is_kvn | not)))
  ' "$shell_config" >"$tmp"
  if cmp -s -- "$tmp" "$shell_config"; then
    rm -- "$tmp"
  else
    chmod --reference="$shell_config" "$tmp"
    mv -f -- "$tmp" "$shell_config"
    echo "Removed omakvn from $shell_config."
  fi
fi

omarchy-shell shell rescanPlugins >/dev/null 2>&1 || true
