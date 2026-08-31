#!/bin/bash
set -euo pipefail

RULE_FILE="/etc/polkit-1/rules.d/49-kvn-tui.rules"

if [[ $EUID -ne 0 ]]; then
    echo "This cleanup must be run as root (e.g. sudo kvn-tui clean --polkit)" >&2
    exit 1
fi

if [[ -e "$RULE_FILE" ]]; then
    rm -- "$RULE_FILE"
    echo "Removed $RULE_FILE"
else
    echo "kvn-tui polkit rule is not installed."
fi

echo "The 'kvn-tui' group and memberships were preserved."
echo "Remove them manually only after both optional system integrations are gone."
