#!/bin/bash
set -euo pipefail

UNIT="kvn-tui-killswitch.service"
RULESET="/etc/kvn-tui/killswitch.nft"
HELPER="/usr/lib/kvn-tui/killswitch-helper.sh"
UNIT_FILE="/etc/systemd/system/$UNIT"
SUDOERS="/etc/sudoers.d/kvn-tui-killswitch"

if [[ $EUID -ne 0 ]]; then
    echo "This cleanup must be run as root (e.g. sudo kvn-tui clean --killswitch)" >&2
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
echo "The 'kvn-tui' group and memberships were preserved."
echo "Restart the user kvn-tui daemon so its persisted state is reconciled."
