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

plugin_dir="${HOME}/.config/omarchy/plugins/kvn.tui"
if [ -d "$plugin_dir" ]; then
  rm -rf -- "$plugin_dir"
  echo "Removed $plugin_dir (the kvn.tui bar plugin)."
fi
