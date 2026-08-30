# System integration

This document describes every system or desktop change made by the kvn-tui
package and its optional setup commands. Review it before running commands with
`sudo`.

## Package installation

The Arch package installs:

| Path | Mode | Purpose |
|------|------|---------|
| `/usr/bin/kvn-tui` | `0755` | Application binary |
| `/usr/lib/systemd/user/kvn-tui.service` | `0644` | Per-user daemon service |
| `/usr/share/libalpm/hooks/kvn-tui-sing-box-capabilities.hook` | `0644` | Restores sing-box capabilities after package updates |
| `/usr/share/licenses/kvn-tui/LICENSE` | `0644` | MIT license for the source package |
| `/usr/share/licenses/kvn-tui-bin/LICENSE` | `0644` | MIT license for the binary package |

The package post-install hook grants `/usr/bin/sing-box` the
`cap_net_admin,cap_net_raw+ep` capabilities required for TUN operation. It does
not enable the daemon service automatically. An ALPM hook restores these
capabilities whenever pacman installs or upgrades `/usr/bin/sing-box`; manually
replaced binaries are not covered. Enable the daemon as your regular user:

```bash
systemctl --user enable --now kvn-tui.service
```

Before removing the package, stop and disable the service:

```bash
systemctl --user disable --now kvn-tui.service
```

Removing kvn-tui removes the ALPM hook but does not remove capabilities already
set on sing-box. If no other software needs them, they can be revoked explicitly:

```bash
sudo setcap -r /usr/bin/sing-box
```

Do not revoke them while another TUN client relies on the same sing-box binary.

## Polkit setup

```bash
sudo kvn-tui setup --polkit
```

The command:

- adds the invoking user to the `network` group if necessary;
- writes `/etc/polkit-1/rules.d/49-kvn-tui.rules` with mode `0644`;
- restarts polkit when its service is active.

The rule allows every member of `network` to perform these actions without an
authentication prompt:

- `org.freedesktop.resolve1.set-dns-servers`
- `org.freedesktop.resolve1.set-domains`
- `org.freedesktop.resolve1.set-default-route`
- `org.freedesktop.NetworkManager.network-control`
- `org.freedesktop.NetworkManager.settings.modify.system`

This authorization is group-wide and is not restricted to the kvn-tui process.
After being added to `network`, log out and back in or run `newgrp network`.

To remove the rule:

```bash
sudo rm /etc/polkit-1/rules.d/49-kvn-tui.rules
sudo systemctl try-restart polkit
```

The setup command does not provide an uninstall action. Do not remove yourself
from `network` without checking whether NetworkManager or other tools use that
membership.

## Kill switch setup

```bash
sudo kvn-tui setup --killswitch
```

The command requires `nftables`, adds the invoking user to `network` when
needed, and installs:

| Path | Owner / mode | Purpose |
|------|--------------|---------|
| `/etc/kvn-tui/killswitch.nft` | `root`, `0644` | nftables ruleset |
| `/usr/lib/kvn-tui/killswitch-helper.sh` | `root:root`, `0755` | Validating privileged helper |
| `/etc/systemd/system/kvn-tui-killswitch.service` | `root`, `0644` | System kill-switch unit |
| `/etc/sudoers.d/kvn-tui-killswitch` | `root:root`, `0440` | Restricted NOPASSWD rule |

The sudoers rule permits members of `network` to invoke only the fixed helper
path without a password. The root-owned helper rejects unknown operations and
accepts only:

- `enable`
- `disable`
- `revoke`
- `allow <ip> <tcp|udp> <port>`

The nftables policy drops other input, output, and forwarded traffic while
allowing loopback, `tun*`, established connections, private LAN ranges,
DHCP, ICMP, and packets marked by sing-box. Temporary IPv4/IPv6 exceptions are
added for VPN and DNS handshakes, then revoked on disconnect.

Marked traffic includes the sing-box `direct` outbound. This is required for
Bypass/Only modes and means an explicit Direct service route can leave through
the physical network while the kill switch is active.

Toggling the kill switch with `K` runs `systemctl enable --now` or
`disable --now`; an enabled kill switch therefore loads again at boot.

### Remove the kill switch

Stop the unit before deleting any files:

```bash
sudo systemctl disable --now kvn-tui-killswitch.service
sudo rm /etc/kvn-tui/killswitch.nft
sudo rm /usr/lib/kvn-tui/killswitch-helper.sh
sudo rm /etc/systemd/system/kvn-tui-killswitch.service
sudo rm /etc/sudoers.d/kvn-tui-killswitch
sudo systemctl daemon-reload
```

These commands leave the shared `network` group membership unchanged.

## Omarchy setup

```bash
kvn-tui setup --omarchy
```

This command runs without sudo and changes only the current user's files. Both
Omarchy generations install this executable launcher:

```text
~/.local/bin/omarchy-launch-kvn-tui
```

The launcher mode is `0755`.

### Omarchy 3

The installer may update:

- `~/.config/waybar/config.jsonc` — status module and click action;
- `~/.config/waybar/style.css` — module spacing;
- `~/.config/hypr/autostart.conf` — removes the legacy daemon autostart line;
- `~/.config/hypr/bindings.conf` — optional launcher binding;
- `~/.config/hypr/hyprland.conf` — floating-window rule.

It restarts Waybar and restores the Waybar configuration from the current
backup if the restart fails.

### Omarchy 4

The installer updates:

- `~/.config/omarchy/plugins/yarikov.omakvn/` — a Git-managed checkout of the
  standalone [omakvn](https://github.com/yarikov/omakvn) Quickshell plugin,
  installed when the shell plugin registry is available. The widget connects
  to the daemon's Unix socket (`$XDG_RUNTIME_DIR/kvn-tui.sock`) and exchanges
  NDJSON: snapshots in, semantic commands (`ConnectProfile`, `Disconnect`,
  `SetRoutingMode`, `SetGeoRegion`, `SetKillSwitch`, `SetAutoConnect`) out.
  Existing embedded copies are migrated automatically. Without the plugin
  registry or when a fresh remote install fails, the installer falls back to
  a `command` bar module running `kvn-tui --waybar-status`;
- `~/.config/omarchy/shell.json` — the `yarikov.omakvn` bar entry (or the legacy
  `kvn-tui` command module on fallback), inserted before `omarchy.bluetooth`;
- `~/.config/hypr/bindings.lua` — optional launcher binding;
- `~/.config/hypr/hyprland.lua` — floating-window rule.

When `omarchy-shell` is running, the installer also triggers a plugin rescan
and an idempotent `omarchy bar put yarikov.omakvn` so the widget appears without a
re-login. Upgrades from the command-module integration replace the old entry.

The selected shortcut is explicitly unbound before being assigned to kvn-tui.
The suggested `Super + Ctrl + K` shortcut replaces the default Herdr binding.
Changes are applied as a transaction and rolled back if setup fails or
Hyprland reports new configuration errors.

### Backups and removal

Changed configuration files receive timestamped backups such as:

```text
bindings.lua.bak.before-kvn-tui.20260821143012
```

At most five kvn-tui backups are retained for each file. To fully remove the
integration on Omarchy 4, first remove the plugin with Omarchy's plugin manager:

```bash
omarchy plugin remove yarikov.omakvn
```

Then restore a suitable backup or manually remove the kvn-tui module, binding,
and window-rule entries. Remove the launcher separately:

```bash
rm ~/.local/bin/omarchy-launch-kvn-tui
```

After confirming the active configuration no longer needs the backups, delete
only the backups created by kvn-tui with:

```bash
kvn-tui clean --omarchy
```

`clean --omarchy` removes only these backups. It does not remove the plugin,
launcher, keybinding, window rule, or undo the Omarchy 3 Waybar integration.
