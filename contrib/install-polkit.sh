#!/bin/bash
set -e

RULE_FILE="/etc/polkit-1/rules.d/49-kvn-tui.rules"
USER_NAME="${SUDO_USER:-$USER}"

echo "Installing kvn-tui polkit rule..."

# ── Ensure user is in the 'network' group ──
if ! groups "$USER_NAME" | grep -qw network; then
    echo "Adding user '$USER_NAME' to the 'network' group..."
    usermod -aG network "$USER_NAME"
    echo "Group added. You may need to log out and back in for it to take full effect."
else
    echo "User '$USER_NAME' is already in the 'network' group."
fi

# ── Create polkit rule ──
echo "Creating polkit rule at $RULE_FILE..."
cat > "$RULE_FILE" <<'EOF'
polkit.addRule(function(action, subject) {
    if (
        (
            action.id == "org.freedesktop.resolve1.set-dns-servers"   ||
            action.id == "org.freedesktop.resolve1.set-domains"       ||
            action.id == "org.freedesktop.resolve1.set-default-route" ||
            action.id == "org.freedesktop.NetworkManager.network-control" ||
            action.id == "org.freedesktop.NetworkManager.settings.modify.system"
        ) &&
        subject.isInGroup("network")
    ) {
        return polkit.Result.YES;
    }
});
EOF

chmod 644 "$RULE_FILE"

# ── Restart polkit ──
echo "Restarting polkit..."
if systemctl is-active --quiet polkit; then
    systemctl restart polkit
else
    echo "Warning: polkit service is not active. Please start it manually."
fi

echo "Done."
echo ""
echo "If you were just added to the 'network' group, run 'newgrp network'"
echo "in your current shell or log out and back in before testing kvn-tui."
