# Arch Linux Build

Local PKGBUILD for building `kvn-tui` from source.

## Build & Install

```bash
cd pkg/arch
makepkg -si
```

## Dependencies

- `rust` / `cargo`
- `dbus` (runtime dependency for zbus)
- `sing-box` (optional, required for VPN connections)

## Clean

```bash
cd pkg/arch
makepkg -C
```
