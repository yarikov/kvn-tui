#!/bin/bash
set -euo pipefail

VERSION="${1:-}"
if [[ -z "$VERSION" ]]; then
    echo "Usage: $0 <version>"
    exit 1
fi

TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

# Download tarball to compute checksum
curl -sL "https://github.com/yarikov/kvn-tui/releases/download/v${VERSION}/kvn-tui-${VERSION}-x86_64-linux.tar.gz" -o "$TMPDIR/pkg.tar.gz"
SHA256=$(sha256sum "$TMPDIR/pkg.tar.gz" | awk '{print $1}')

# Clone AUR repo
git clone ssh://aur@aur.archlinux.org/kvn-tui-bin.git "$TMPDIR/aur"
cd "$TMPDIR/aur"

# Generate PKGBUILD from template
sed -e "s/{{VERSION}}/${VERSION}/g" \
    -e "s/{{SHA256SUM}}/${SHA256}/g" \
    "$(git rev-parse --show-toplevel)/pkg/aur/PKGBUILD.bin" > PKGBUILD

# Generate .SRCINFO
makepkg --printsrcinfo > .SRCINFO

# Commit and push
git add PKGBUILD .SRCINFO
if git diff --cached --quiet; then
    echo "No changes to commit"
    exit 0
fi
git commit -m "chore(release): update to v${VERSION}"
git push
