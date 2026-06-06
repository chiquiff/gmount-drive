#!/usr/bin/env bash
# Builds an installable .deb package of GMount Drive (Ubuntu/Debian).
# Compiles in release, assembles the package tree (binary + .desktop + icons + metainfo)
# and packs it with dpkg-deb. Run: bash packaging/build-deb.sh
set -e
cd "$(dirname "$0")/.."   # project root

APP_ID="io.github.gmountdrive.App"
PKG="gmount-drive"
VERSION="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
ARCH="$(dpkg --print-architecture)"
BIN_SRC="target/release/gdrive-mount"

echo ">> Building release (this can take a few minutes)…"
source "$HOME/.cargo/env" 2>/dev/null || true
export RUST_MIN_STACK=268435456
cargo build --release

if [ ! -x "$BIN_SRC" ]; then
    echo "ERROR: binary not found at $BIN_SRC" >&2
    exit 1
fi

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
ROOT="$STAGE/pkg"

# Binary.
install -Dm755 "$BIN_SRC" "$ROOT/usr/bin/$PKG"

# Icons (hicolor, several sizes).
for s in 16 32 64 128 256 512; do
    install -Dm644 "assets/brand/gmount-drive-dock-$s.png" \
        "$ROOT/usr/share/icons/hicolor/${s}x${s}/apps/$APP_ID.png"
done

# The .desktop launcher.
install -d "$ROOT/usr/share/applications"
cat > "$ROOT/usr/share/applications/$APP_ID.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=GMount Drive
Comment=Mount your Google Drive as a disk
Exec=$PKG
Icon=$APP_ID
Terminal=false
Categories=Utility;Network;FileTransfer;
StartupWMClass=$APP_ID
EOF

# Metainfo (AppStream, for stores like GNOME Software).
install -d "$ROOT/usr/share/metainfo"
cat > "$ROOT/usr/share/metainfo/$APP_ID.metainfo.xml" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<component type="desktop-application">
  <id>$APP_ID</id>
  <name>GMount Drive</name>
  <summary>Cloud mount for desktop</summary>
  <metadata_license>CC0-1.0</metadata_license>
  <project_license>GPL-3.0-or-later</project_license>
  <description>
    <p>Mount your Google Drive as a disk on Linux. A free, open-source alternative to Insync,
       with a native GTK4/libadwaita interface and rclone as the mount engine.</p>
  </description>
  <launchable type="desktop-id">$APP_ID.desktop</launchable>
</component>
EOF

# Package control metadata.
INSTALLED_SIZE="$(du -ks "$ROOT/usr" | cut -f1)"
install -d "$ROOT/DEBIAN"
cat > "$ROOT/DEBIAN/control" <<EOF
Package: $PKG
Version: $VERSION
Section: utils
Priority: optional
Architecture: $ARCH
Depends: libgtk-4-1, libadwaita-1-0, libdbus-1-3, fuse3
Recommends: rclone
Installed-Size: $INSTALLED_SIZE
Maintainer: GMount Drive <gmount-drive@localhost>
Description: Mount your Google Drive as a disk on Linux
 A free, open-source alternative to Insync. Mounts Google Drive via rclone + FUSE and
 exposes it as a drive on the system, with a native GTK4/libadwaita interface.
EOF

# Post-install triggers: refresh the icon and .desktop caches.
cat > "$ROOT/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -f /usr/share/icons/hicolor >/dev/null 2>&1 || true
fi
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database /usr/share/applications >/dev/null 2>&1 || true
fi
EOF
chmod 755 "$ROOT/DEBIAN/postinst"

OUT="$(pwd)/${PKG}_${VERSION}_${ARCH}.deb"
dpkg-deb --build --root-owner-group "$ROOT" "$OUT"

echo ">> ✅ .deb built: $OUT"
echo "   Install it with:   sudo apt install $OUT"
echo "   (or:               sudo dpkg -i $OUT && sudo apt -f install)"
