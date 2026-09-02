#!/bin/bash
set -euo pipefail

UNIT="kvn-tui-killswitch.service"
RULESET="/etc/kvn-tui/killswitch.nft"
HELPER="/usr/lib/kvn-tui/killswitch-helper.sh"
UNIT_FILE="/etc/systemd/system/$UNIT"
SUDOERS="/etc/sudoers.d/kvn-tui-killswitch"
POLKIT_RULE="/etc/polkit-1/rules.d/49-kvn-tui.rules"
GROUP_NAME="kvn-tui"

cleanup_group_if_unused() {
    if [[ -e "$POLKIT_RULE" ]]; then
        echo "The '$GROUP_NAME' group was preserved because the polkit rule still uses it."
        return
    fi
    if ! getent group "$GROUP_NAME" >/dev/null; then
        echo "The '$GROUP_NAME' group is not present."
        return
    fi
    if groupdel "$GROUP_NAME"; then
        echo "Removed unused '$GROUP_NAME' group and its membership records."
    else
        echo "Warning: could not remove unused '$GROUP_NAME' group; remove it manually." >&2
    fi
}

if [[ $EUID -ne 0 ]]; then
    echo "This cleanup must be run as root (e.g. sudo kvn-tui clean --killswitch)" >&2
    exit 1
fi

USER_NAME="${SUDO_USER:-}"
if [[ -z "$USER_NAME" || "$USER_NAME" == "root" ]] || ! id "$USER_NAME" >/dev/null 2>&1; then
    echo "Could not identify a non-root invoking user; run this command via sudo." >&2
    exit 1
fi

if systemctl is-active --quiet "$UNIT"; then
    systemctl disable --now "$UNIT"
    if systemctl is-active --quiet "$UNIT"; then
        echo "Failed to stop $UNIT; refusing to remove its files." >&2
        exit 1
    fi
else
    systemctl disable "$UNIT" >/dev/null 2>&1 || true
fi

# Remove a table left behind after a prior interrupted service stop.
nft delete table inet kvn_tui_killswitch >/dev/null 2>&1 || true

rm -f -- "$SUDOERS" "$HELPER" "$UNIT_FILE" "$RULESET"
rmdir /usr/lib/kvn-tui 2>/dev/null || true
rmdir /etc/kvn-tui 2>/dev/null || true
systemctl daemon-reload
systemctl reset-failed "$UNIT" >/dev/null 2>&1 || true

echo "Removed kvn-tui kill-switch system integration."
cleanup_group_if_unused
echo "Restart the user kvn-tui daemon so its persisted state is reconciled."
