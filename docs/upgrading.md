# Upgrading kvn-tui

Upgrade kvn-tui through your usual package source. Review the applicable
migration guides below when crossing a version that requires additional steps.

## v0.28.0

All users upgrading from v0.27.1 or earlier should review the
[v0.28.0 migration guide](migrations/v0.28.0.md). Existing polkit and kill-switch
installations must be refreshed, while the schema v5 configuration migration is
automatic.

## v0.27.0 on Omarchy 4

Omarchy 4 users upgrading from an earlier kvn-tui release should follow the
[v0.27.0 migration guide](migrations/v0.27.0.md) to install the standalone
`yarikov.omakvn` bar plugin.

## v0.22.0 on Omarchy

Omarchy users upgrading from an earlier kvn-tui release should follow the
[v0.22.0 migration guide](migrations/v0.22.0.md) to reinstall the desktop
integration for Omarchy 3 or 4.

## v0.20.0

Users upgrading from v0.19.1 or earlier should follow the
[v0.20.0 migration guide](migrations/v0.20.0.md) to move daemon startup from
Hyprland autostart to the systemd user service.
