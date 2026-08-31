#!/bin/bash
# Narrow privileged API used by the kvn-tui daemon through sudoers.
set -euo pipefail

UNIT="kvn-tui-killswitch.service"
RULESET="/etc/kvn-tui/killswitch.nft"
UNIT_FILE="/etc/systemd/system/$UNIT"
SUDOERS="/etc/sudoers.d/kvn-tui-killswitch"

validate_ip() {
    local ip=$1
    if [[ $ip =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}$ ]]; then
        local octet
        local -a octets
        IFS=. read -r -a octets <<<"$ip"
        for octet in "${octets[@]}"; do
            (( 10#$octet <= 255 )) || return 1
        done
        return 0
    fi

    [[ $ip == *:* && $ip =~ ^[0-9a-fA-F:]+$ ]] || return 1
    getent ahosts "$ip" >/dev/null 2>&1
}

validate_allow_args() {
    [[ $# -eq 3 ]] || return 1
    validate_ip "$1" || return 1
    [[ $2 == "tcp" || $2 == "udp" ]] || return 1
    [[ $3 =~ ^[0-9]{1,5}$ ]] || return 1
    (( 10#$3 >= 1 && 10#$3 <= 65535 ))
}

validate_command_args() {
    [[ $# -ge 1 ]] || return 1
    case $1 in
        check|enable|disable|revoke) [[ $# -eq 1 ]] ;;
        allow) validate_allow_args "${@:2}" ;;
        *) return 1 ;;
    esac
}

check_root_file() {
    local path=$1 expected_mode=$2 owner mode
    read -r owner mode < <(stat -c '%u %a' "$path") || return 1
    [[ $owner == 0 && $mode == "$expected_mode" ]]
}

check_installation() {
    command -v nft >/dev/null
    command -v systemctl >/dev/null
    command -v getent >/dev/null
    check_root_file "$0" 755
    check_root_file "$RULESET" 644
    check_root_file "$UNIT_FILE" 644
    check_root_file "$SUDOERS" 440
}

main() {
    if [[ $EUID -ne 0 ]]; then
        echo "kvn-tui kill-switch helper must run as root" >&2
        exit 1
    fi
    if [[ "$(stat -c %u "$0")" != "0" ]]; then
        echo "kvn-tui kill-switch helper is not root-owned; refusing to run" >&2
        exit 1
    fi
    if ! validate_command_args "$@"; then
        echo "usage: $0 {check|enable|disable|revoke|allow <ip> <tcp|udp> <port>}" >&2
        exit 2
    fi

    case "$1" in
        check)
            check_installation
            ;;
        enable|disable)
            exec systemctl "$1" --now "$UNIT"
            ;;
        revoke)
            nft flush set inet kvn_tui_killswitch handshake_v4 2>/dev/null || true
            nft flush set inet kvn_tui_killswitch handshake_v6 2>/dev/null || true
            ;;
        allow)
            if [[ $2 == *:* ]]; then
                exec nft add element inet kvn_tui_killswitch handshake_v6 "{ $2 . $3 . $4 }"
            else
                exec nft add element inet kvn_tui_killswitch handshake_v4 "{ $2 . $3 . $4 }"
            fi
            ;;
    esac
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
    main "$@"
fi
