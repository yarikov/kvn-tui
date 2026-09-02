#!/usr/bin/env bash
set -euo pipefail

skill_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
repo_dir=$(cd -- "$skill_dir/../../.." && pwd)
mode=${1:---all}
gallery_dir="$repo_dir/assets/themes"
mapfile -t themes < <(find "$repo_dir/themes" -maxdepth 1 -type f -name '*.toml' -printf '%f\n' | sed 's/\.toml$//' | sort)

check_gallery() {
  local failed=0 theme image dimensions dark_ratio extra
  [[ ${#themes[@]} -eq 22 ]] || { echo "expected 22 bundled themes, found ${#themes[@]}" >&2; failed=1; }
  for theme in "${themes[@]}"; do
    image="$gallery_dir/$theme.png"
    [[ -f $image ]] || { echo "missing $image" >&2; failed=1; continue; }
    dimensions=$(identify -format '%wx%h' "$image")
    [[ $dimensions == 1400x960 ]] || { echo "$image is $dimensions, expected 1400x960" >&2; failed=1; }
    rg -q "assets/themes/$theme\.png" "$repo_dir/docs/themes.md" || { echo "docs/themes.md does not reference $theme" >&2; failed=1; }
    if rg -q '^mode = "light"' "$repo_dir/themes/$theme.toml"; then
      dark_ratio=$(magick "$image" -colorspace gray -threshold 50% -format '%[fx:1-mean]' info:)
      awk -v ratio="$dark_ratio" 'BEGIN { exit !(ratio >= 0.002) }' || { echo "$image has no readable dark foreground" >&2; failed=1; }
    fi
  done
  while IFS= read -r extra; do
    [[ -z $extra ]] && continue
    [[ -f $repo_dir/themes/${extra%.png}.toml ]] || { echo "stale gallery image: $extra" >&2; failed=1; }
  done < <(find "$gallery_dir" -maxdepth 1 -type f -name '*.png' -printf '%f\n' 2>/dev/null | sort)
  [[ -f $repo_dir/assets/screenshot.png ]] || { echo "missing assets/screenshot.png" >&2; failed=1; }
  [[ $failed -eq 0 ]]
}

if [[ $mode == --check ]]; then command -v identify >/dev/null; command -v magick >/dev/null; check_gallery; exit; fi
case $mode in --all|--readme|--themes) ;; *) echo "usage: $0 [--all|--readme|--themes|--check]" >&2; exit 2 ;; esac
for command in hyprctl grim jq magick omarchy omarchy-launch-tui cargo wtype rg; do command -v "$command" >/dev/null || { echo "missing required command: $command" >&2; exit 1; }; done
[[ ${XDG_SESSION_TYPE:-} == wayland && -n ${HYPRLAND_INSTANCE_SIGNATURE:-} ]] || { echo "a Hyprland Wayland session is required" >&2; exit 1; }

mkdir -p "$gallery_dir"
tmp_dir=$(mktemp -d)
clients_before="$tmp_dir/clients-before.json"
original_workspace=$(hyprctl activeworkspace -j | jq -r '.id')
original_focus=$(hyprctl activewindow -j | jq -r '.address // empty')
original_system_theme=$(omarchy theme current)
current_theme_dir="${XDG_STATE_HOME:-$HOME/.local/state}/omarchy/current/theme"
original_background=$(readlink "${XDG_STATE_HOME:-$HOME/.local/state}/omarchy/current/background" 2>/dev/null || true)
preview_address=
windows_stashed=false
system_theme_changed=false

dispatch() { hyprctl dispatch "$1" >/dev/null; }
focus_workspace() { dispatch "hl.dsp.focus({ workspace = \"$1\" })"; }
focus_window() { dispatch "hl.dsp.focus({ window = \"address:$1\" })"; }

restore_windows() {
  $windows_stashed || return 0
  while IFS=$'\t' read -r address workspace; do
    hyprctl clients -j | jq -e --arg address "$address" '.[] | select(.address == $address)' >/dev/null || continue
    focus_window "$address" || true
    dispatch "hl.dsp.window.move({ workspace = \"$workspace\", follow = false })" || true
  done < <(jq -r '[.[] | select(.workspace.id > 0)] | reverse[] | [.address, (.workspace.id | tostring)] | @tsv' "$clients_before")
  focus_workspace "$original_workspace" || true
  [[ -z $original_focus ]] || focus_window "$original_focus" || true
  windows_stashed=false
}

close_preview() {
  [[ -z $preview_address ]] && return 0
  if hyprctl clients -j | jq -e --arg address "$preview_address" '.[] | select(.address == $address)' >/dev/null; then
    focus_window "$preview_address" || true
    wtype q || true
    for _ in {1..30}; do
      hyprctl clients -j | jq -e --arg address "$preview_address" '.[] | select(.address == $address)' >/dev/null || break
      sleep 0.1
    done
  fi
  preview_address=
}

restore_system_theme() {
  $system_theme_changed || return 0
  omarchy theme set "$original_system_theme" >/dev/null || true
  if [[ -n $original_background ]]; then
    if [[ $original_background == "$current_theme_dir/"* ]]; then
      original_background="$current_theme_dir/${original_background#"$current_theme_dir/"}"
    fi
    [[ -f $original_background ]] && omarchy theme bg set "$original_background" >/dev/null || true
  fi
  system_theme_changed=false
}

cleanup() {
  close_preview
  restore_system_theme
  restore_windows
  find "$tmp_dir" -mindepth 1 -delete 2>/dev/null || true
  rmdir "$tmp_dir" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

cd "$repo_dir"
cargo build --release
hyprctl clients -j >"$clients_before"
focus_workspace 1
while IFS= read -r address; do
  focus_window "$address"
  dispatch 'hl.dsp.window.move({ workspace = "special:scratchpad", follow = false })'
done < <(jq -r '.[] | select(.workspace.id > 0) | .address' "$clients_before")
windows_stashed=true
focus_workspace 1

launch_preview() {
  local theme=$1 address= dimensions=
  preview_address=
  hyprctl clients -j | jq -r '.[].address' >"$tmp_dir/addresses-before-preview"
  env -u NO_COLOR omarchy-launch-tui --app-id=org.omarchy.kvn-tui "$repo_dir/target/release/kvn-tui" docs-preview --theme "$theme" >/dev/null 2>&1 &
  for _ in {1..100}; do
    while IFS= read -r address; do
      if ! rg -Fxq "$address" "$tmp_dir/addresses-before-preview"; then preview_address=$address; break 2; fi
    done < <(hyprctl clients -j | jq -r '.[] | select(.class == "org.omarchy.kvn-tui") | .address')
    sleep 0.1
  done
  [[ -n $preview_address ]] || { echo "preview window did not appear" >&2; exit 1; }
  for _ in {1..30}; do
    dimensions=$(hyprctl clients -j | jq -r --arg address "$preview_address" '.[] | select(.address == $address) | "\(.size[0])x\(.size[1])"')
    [[ $dimensions == 875x600 ]] && break
    sleep 0.1
  done
  [[ $dimensions == 875x600 ]] || { echo "preview window is $dimensions, expected 875x600" >&2; exit 1; }
  sleep 0.5
}

capture_window() {
  local output=$1 geometry
  geometry=$(hyprctl clients -j | jq -r --arg address "$preview_address" '.[] | select(.address == $address) | "\(.at[0]),\(.at[1]) \(.size[0])x\(.size[1])"')
  grim -g "$geometry" "$tmp_dir/capture.png"
  magick "$tmp_dir/capture.png" -strip "$tmp_dir/normalized.png"
  mv "$tmp_dir/normalized.png" "$output"
  close_preview
}

if [[ $mode == --all || $mode == --themes ]]; then
  for theme in "${themes[@]}"; do echo "capturing $theme"; launch_preview "$theme"; capture_window "$gallery_dir/$theme.png"; done
fi
if [[ $mode == --all || $mode == --readme ]]; then
  if [[ $original_system_theme != "Tokyo Night" ]]; then
    system_theme_changed=true
    omarchy theme set "Tokyo Night" >/dev/null
    sleep 4
  fi
  launch_preview tokyo-night
  monitor=$(hyprctl activeworkspace -j | jq -r '.monitor')
  grim -o "$monitor" "$tmp_dir/desktop.png"
  dimensions=$(identify -format '%wx%h' "$tmp_dir/desktop.png")
  [[ $dimensions == 1920x1080 ]] || { echo "active monitor is $dimensions, expected 1920x1080" >&2; exit 1; }
  magick "$tmp_dir/desktop.png" -strip "$tmp_dir/readme.png"
  mv "$tmp_dir/readme.png" "$repo_dir/assets/screenshot.png"
  close_preview
  restore_system_theme
fi

magick montage "$gallery_dir"/*.png -thumbnail 292x200 -tile 4x -geometry +8+8 "$repo_dir/target/theme-gallery-contact-sheet.png"
restore_windows
before_mapping=$(jq -S '[.[] | select(.workspace.id > 0) | {address, workspace: .workspace.id}] | sort_by(.address)' "$clients_before")
after_mapping=$(hyprctl clients -j | jq -S --argjson before "$before_mapping" '[.[] | select(.address as $address | $before | any(.address == $address)) | {address, workspace: .workspace.id}] | sort_by(.address)')
[[ $before_mapping == "$after_mapping" ]] || { echo "window workspace mapping was not restored exactly" >&2; diff -u <(printf '%s\n' "$before_mapping") <(printf '%s\n' "$after_mapping") || true; exit 1; }
echo "contact sheet: $repo_dir/target/theme-gallery-contact-sheet.png"
check_gallery
