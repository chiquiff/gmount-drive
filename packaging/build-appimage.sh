#!/usr/bin/env bash
# Builds a universal AppImage of GMount Drive (runs on any distro, without installing GTK).
# Bundles the GTK4/libadwaita runtime (via linuxdeploy-plugin-gtk) + a static rclone.
# Requirements on the build machine: curl, unzip, and GTK4 dev headers.
# Run: bash packaging/build-appimage.sh
#
# NOTE: the GTK4 AppImage is finicky — the bundled runtime may not pick up the system font/icons,
# so the result can look "rough". The .deb is the recommended, native-looking package.
set -e
cd "$(dirname "$0")/.."   # project root

APP_ID="io.github.gmountdrive.App"
VERSION="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
BIN_SRC="target/release/gdrive-mount"
ARCH="$(uname -m)"
TOOLS="packaging/.tools"

echo ">> Building release…"
source "$HOME/.cargo/env" 2>/dev/null || true
export RUST_MIN_STACK=268435456
cargo build --release
[ -x "$BIN_SRC" ] || { echo "ERROR: $BIN_SRC didn't build" >&2; exit 1; }

mkdir -p "$TOOLS"
fetch() {  # url dest
    if [ ! -f "$2" ]; then echo ">> downloading $(basename "$2")…"; curl -fL "$1" -o "$2"; fi
}

# Packaging tools.
fetch "https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-${ARCH}.AppImage" "$TOOLS/linuxdeploy.AppImage"
fetch "https://raw.githubusercontent.com/linuxdeploy/linuxdeploy-plugin-gtk/master/linuxdeploy-plugin-gtk.sh" "$TOOLS/linuxdeploy-plugin-gtk.sh"
chmod +x "$TOOLS/linuxdeploy.AppImage" "$TOOLS/linuxdeploy-plugin-gtk.sh"

# Official static rclone (bundled so we don't depend on the system).
if [ ! -x "$TOOLS/rclone" ]; then
    rc_arch=amd64; [ "$ARCH" = "aarch64" ] && rc_arch=arm64
    fetch "https://downloads.rclone.org/rclone-current-linux-${rc_arch}.zip" "$TOOLS/rclone.zip"
    tmp="$(mktemp -d)"; unzip -q "$TOOLS/rclone.zip" -d "$tmp"
    cp "$tmp"/rclone-*/rclone "$TOOLS/rclone"; chmod +x "$TOOLS/rclone"
    rm -rf "$tmp" "$TOOLS/rclone.zip"
fi

# Assemble the AppDir.
APPDIR="$(pwd)/packaging/AppDir"
rm -rf "$APPDIR"
install -Dm755 "$BIN_SRC" "$APPDIR/usr/bin/gmount-drive"
install -Dm755 "$TOOLS/rclone" "$APPDIR/usr/bin/rclone"   # stays on PATH inside the AppImage
install -Dm644 "assets/brand/gmount-drive-icon-256.png" \
    "$APPDIR/usr/share/icons/hicolor/256x256/apps/$APP_ID.png"

DESKTOP="packaging/$APP_ID.desktop"
cat > "$DESKTOP" <<EOF
[Desktop Entry]
Type=Application
Name=GMount Drive
Comment=Mount your Google Drive as a disk
Exec=gmount-drive
Icon=$APP_ID
Terminal=false
Categories=Utility;Network;FileTransfer;
StartupWMClass=$APP_ID
EOF

# --- Appearance: consistent font + icon theme inside the bundle ---
# settings.ini that GTK reads via XDG_CONFIG_DIRS (pointed at by the hook below). Forces the
# Ubuntu font and the Adwaita icon theme (which the plugin bundles), instead of a half-bundled Yaru.
install -d "$APPDIR/etc/xdg/gtk-4.0"
cat > "$APPDIR/etc/xdg/gtk-4.0/settings.ini" <<EOF
[Settings]
gtk-font-name=Ubuntu 11
gtk-icon-theme-name=Adwaita
gtk-application-prefer-dark-theme=false
EOF

# AppRun hook: runs AFTER the GTK plugin hook (the 'zz-' prefix) to override its env vars.
install -d "$APPDIR/apprun-hooks"
cat > "$APPDIR/apprun-hooks/zz-gmount.sh" <<'EOF'
# Use the host's fontconfig so real fonts resolve (Ubuntu, DejaVu, etc.) instead of the minimal
# one the plugin sometimes bundles (the cause of the "ugly font").
unset FONTCONFIG_FILE FONTCONFIG_PATH
# Make GTK read our settings.ini (font + icon theme).
export XDG_CONFIG_DIRS="$APPDIR/etc/xdg:${XDG_CONFIG_DIRS:-/etc/xdg}"
EOF

# Build the AppImage. The GTK plugin bundles GTK4/libadwaita + schemas + loaders + Adwaita theme.
export PATH="$(pwd)/$TOOLS:$PATH"
export DEPLOY_GTK_VERSION=4
export OUTPUT="GMount_Drive-${VERSION}-${ARCH}.AppImage"

"$TOOLS/linuxdeploy.AppImage" --appimage-extract-and-run \
    --appdir "$APPDIR" \
    --executable "$APPDIR/usr/bin/gmount-drive" \
    --desktop-file "$DESKTOP" \
    --icon-file "$APPDIR/usr/share/icons/hicolor/256x256/apps/$APP_ID.png" \
    --plugin gtk \
    --output appimage

echo ">> ✅ AppImage: $(pwd)/$OUTPUT"
echo "   Try it:  chmod +x $OUTPUT && ./$OUTPUT"
