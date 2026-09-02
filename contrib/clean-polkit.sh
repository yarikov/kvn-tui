#!/bin/bash
set -euo pipefail

RULE_FILE="/etc/polkit-1/rules.d/49-kvn-tui.rules"
KILLSWITCH_SUDOERS="/etc/sudoers.d/kvn-tui-killswitch"
GROUP_NAME="kvn-tui"

cleanup_group_if_unused() {
    if [[ -e "$KILLSWITCH_SUDOERS" ]]; then
        echo "The '$GROUP_NAME' group was preserved because the kill switch still uses it."
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
    echo "This cleanup must be run as root (e.g. sudo kvn-tui clean --polkit)" >&2
    exit 1
fi

USER_NAME="${SUDO_USER:-}"
if [[ -z "$USER_NAME" || "$USER_NAME" == "root" ]] || ! id "$USER_NAME" >/dev/null 2>&1; then
    echo "Could not identify a non-root invoking user; run this command via sudo." >&2
    exit 1
fi

if [[ -e "$RULE_FILE" ]]; then
    rm -- "$RULE_FILE"
    echo "Removed $RULE_FILE"
else
    echo "kvn-tui polkit rule is not installed."
fi

cleanup_group_if_unused
