#!/bin/bash
# Install the kvn-tui kill switch:
#   - /etc/kvn-tui/killswitch.nft           (nftables ruleset)
#   - /usr/lib/kvn-tui/killswitch-helper.sh (privileged helper)
#   - /etc/systemd/system/kvn-tui-killswitch.service
#   - /etc/sudoers.d/kvn-tui-killswitch     (NOPASSWD for group `kvn-tui`)
#
# After install, members of the `kvn-tui` group can toggle the kill switch
# from kvn-tui without a password prompt.
set -euo pipefail

if [[ $EUID -ne 0 ]]; then
    echo "This installer must be run as root (e.g. sudo kvn-tui setup --killswitch)" >&2
    exit 1
fi

USER_NAME="${SUDO_USER:-}"
if [[ -z "$USER_NAME" || "$USER_NAME" == "root" ]] || ! id "$USER_NAME" >/dev/null 2>&1; then
    echo "Could not identify a non-root invoking user; run this command via sudo." >&2
    exit 1
fi
HELPER_SOURCE="${1:?missing embedded kill-switch helper source}"
GROUP_NAME="kvn-tui"

echo "Installing kvn-tui kill switch for user '$USER_NAME'…"

if ! getent group "$GROUP_NAME" >/dev/null; then
    groupadd --system "$GROUP_NAME"
    echo "Created system group '$GROUP_NAME'."
fi

# ── 1. nftables ruleset ────────────────────────────────────────────────
install -dm755 /etc/kvn-tui
cat > /etc/kvn-tui/killswitch.nft <<'NFT_EOF'
#!/usr/sbin/nft -f
# kvn-tui kill switch: drop all non-VPN egress.
# Loaded by kvn-tui-killswitch.service at boot when enabled.

# Idempotent atomic replace — add-then-delete-then-add lets `nft -f` succeed
# both on first load (no prior table) and on reload (prior table exists).
add table inet kvn_tui_killswitch
delete table inet kvn_tui_killswitch
table inet kvn_tui_killswitch {
    # Dynamic sets populated by the daemon during the connect handshake.
    # IPv6 uses `meta l4proto` so extension headers are followed to the actual
    # transport protocol; IPv4's protocol field already identifies it directly.
    set handshake_v4 {
        type ipv4_addr . inet_proto . inet_service
        flags interval
    }
    set handshake_v6 {
        type ipv6_addr . inet_proto . inet_service
        flags interval
    }

    chain input {
        type filter hook input priority -10; policy drop;
        iifname "lo" accept
        iifname "tun*" accept
        ct state established,related accept
        meta l4proto { icmp, icmpv6 } accept
        ip saddr { 192.168.0.0/16, 10.0.0.0/8, 172.16.0.0/12 } accept
        ip6 saddr { fc00::/7, fe80::/10 } accept
        udp sport 67 udp dport 68 accept
        udp sport 547 udp dport 546 accept
    }

    chain output {
        type filter hook output priority -10; policy drop;
        oifname "lo" accept
        oifname "tun*" accept
        # Packets marked by sing-box (route.default_mark = 666 / 0x29a). This
        # allows the `direct` outbound used by Bypass/Only routing modes to
        # reach the physical interface; everything else still drops.
        meta mark 0x29a accept
        ct state established,related accept
        ip daddr { 192.168.0.0/16, 10.0.0.0/8, 172.16.0.0/12 } accept
        ip6 daddr { fc00::/7, fe80::/10 } accept
        meta l4proto { icmp, icmpv6 } accept
        ip daddr . ip protocol . th dport @handshake_v4 accept
        ip6 daddr . meta l4proto . th dport @handshake_v6 accept
        udp sport 68 udp dport 67 accept
        udp sport 546 udp dport 547 accept
    }

    chain forward {
        type filter hook forward priority -10; policy drop;
        oifname "tun*" accept
        ct state established,related accept
    }
}
NFT_EOF
chmod 644 /etc/kvn-tui/killswitch.nft

# Syntax-check before installing the unit so we don't ship a broken ruleset.
if ! nft -c -f /etc/kvn-tui/killswitch.nft; then
    echo "FATAL: /etc/kvn-tui/killswitch.nft failed nft syntax check" >&2
    exit 1
fi

# ── 2. Helper script ───────────────────────────────────────────────────
install -dm755 /usr/lib/kvn-tui
HELPER_TMP="$(mktemp)"
SUDOERS_TMP=""
trap 'rm -f "$HELPER_TMP" "$SUDOERS_TMP"' EXIT
printf '%s\n' "$HELPER_SOURCE" >"$HELPER_TMP"
bash -n "$HELPER_TMP"
install -m 0755 -o root -g root "$HELPER_TMP" /usr/lib/kvn-tui/killswitch-helper.sh

# ── 3. systemd unit ────────────────────────────────────────────────────
cat > /etc/systemd/system/kvn-tui-killswitch.service <<'UNIT_EOF'
[Unit]
Description=kvn-tui kill switch (drop non-VPN egress)
DefaultDependencies=no
Conflicts=shutdown.target
Before=network-pre.target shutdown.target
Wants=network-pre.target
ConditionPathExists=/etc/kvn-tui/killswitch.nft

[Service]
Type=oneshot
ExecStart=/usr/sbin/nft -f /etc/kvn-tui/killswitch.nft
ExecStop=/usr/sbin/nft delete table inet kvn_tui_killswitch
ExecStopPost=-/usr/sbin/nft delete table inet kvn_tui_killswitch
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
WantedBy=network-pre.target
UNIT_EOF
chmod 644 /etc/systemd/system/kvn-tui-killswitch.service

# ── 4. sudoers fragment (validated before installing) ──────────────────
SUDOERS_TMP="$(mktemp)"
cat > "$SUDOERS_TMP" <<'SUDOERS_EOF'
# Allow group `kvn-tui` to invoke the kvn-tui kill-switch helper without a
# password. The helper itself validates its arguments; nothing else is
# whitelisted, and the helper path is fixed.
Defaults!/usr/lib/kvn-tui/killswitch-helper.sh env_reset, secure_path="/usr/sbin:/usr/bin"
%kvn-tui ALL=(root) NOPASSWD: /usr/lib/kvn-tui/killswitch-helper.sh
SUDOERS_EOF
if ! visudo -cf "$SUDOERS_TMP" >/dev/null; then
    echo "FATAL: sudoers fragment failed validation" >&2
    exit 1
fi
install -m 0440 -o root -g root "$SUDOERS_TMP" /etc/sudoers.d/kvn-tui-killswitch

# ── 5. Ensure user is in the dedicated group ──────────────────────────
if ! id -nG "$USER_NAME" | tr ' ' '\n' | grep -Fxq "$GROUP_NAME"; then
    echo "Adding user '$USER_NAME' to the '$GROUP_NAME' group…"
    usermod -aG "$GROUP_NAME" "$USER_NAME"
    NEW_GROUP=1
else
    NEW_GROUP=0
fi

# ── 6. Reload systemd ──────────────────────────────────────────────────
systemctl daemon-reload

# If the unit is already active, reload the ruleset so changes to the template
# take effect immediately (e.g. when re-running this installer after an upgrade).
if systemctl is-active --quiet kvn-tui-killswitch.service; then
    echo "kvn-tui-killswitch.service is active — reloading ruleset…"
    systemctl restart kvn-tui-killswitch.service
fi

echo
echo "Kill switch components installed:"
echo "  /etc/kvn-tui/killswitch.nft"
echo "  /usr/lib/kvn-tui/killswitch-helper.sh"
echo "  /etc/systemd/system/kvn-tui-killswitch.service"
echo "  /etc/sudoers.d/kvn-tui-killswitch"
echo
echo "Toggle the kill switch from the TUI with Shift+K."
if [[ "$NEW_GROUP" == "1" ]]; then
    echo "User '$USER_NAME' was added to '$GROUP_NAME' — log out and back in,"
    echo "then restart kvn-tui.service before toggling from the TUI."
fi
if id -nG "$USER_NAME" | tr ' ' '\n' | grep -Fxq network; then
    echo
    echo "Note: kvn-tui no longer uses the 'network' group. Existing membership"
    echo "was preserved because another application may rely on it."
fi
