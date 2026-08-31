#!/bin/bash
set -euo pipefail

RULE_FILE="/etc/polkit-1/rules.d/49-kvn-tui.rules"
GROUP_NAME="kvn-tui"

if [[ $EUID -ne 0 ]]; then
    echo "This installer must be run as root (e.g. sudo kvn-tui setup --polkit)" >&2
    exit 1
fi

USER_NAME="${SUDO_USER:-}"
if [[ -z "$USER_NAME" || "$USER_NAME" == "root" ]] || ! id "$USER_NAME" >/dev/null 2>&1; then
    echo "Could not identify a non-root invoking user; run this command via sudo." >&2
    exit 1
fi

if ! getent group "$GROUP_NAME" >/dev/null; then
    groupadd --system "$GROUP_NAME"
    echo "Created system group '$GROUP_NAME'."
fi

if ! id -nG "$USER_NAME" | tr ' ' '\n' | grep -Fxq "$GROUP_NAME"; then
    usermod -aG "$GROUP_NAME" "$USER_NAME"
    ADDED_TO_GROUP=1
else
    ADDED_TO_GROUP=0
fi

RULE_TMP="$(mktemp)"
trap 'rm -f "$RULE_TMP"' EXIT
cat >"$RULE_TMP" <<'EOF'
// kvn-tui: allow unattended sing-box DNS setup for explicitly enrolled users.
polkit.addRule(function(action, subject) {
    if (
        (
            action.id == "org.freedesktop.resolve1.set-dns-servers" ||
            action.id == "org.freedesktop.resolve1.set-domains" ||
            action.id == "org.freedesktop.resolve1.set-default-route"
        ) &&
        subject.isInGroup("kvn-tui")
    ) {
        return polkit.Result.YES;
    }
});
EOF

install -m 0644 -o root -g root "$RULE_TMP" "$RULE_FILE"

echo "Installed $RULE_FILE with three systemd-resolved permissions."
echo "NetworkManager permissions are not granted."
if [[ "$ADDED_TO_GROUP" == "1" ]]; then
    echo "User '$USER_NAME' was added to '$GROUP_NAME'. Log out and back in,"
    echo "then restart kvn-tui.service before using unattended DNS setup."
fi
if id -nG "$USER_NAME" | tr ' ' '\n' | grep -Fxq network; then
    echo
    echo "Note: kvn-tui no longer uses the 'network' group. Existing membership"
    echo "was preserved because another application may rely on it."
fi
