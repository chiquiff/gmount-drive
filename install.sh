#!/usr/bin/env bash
# Installs GMount Drive for the current user: icons in the hicolor theme + a .desktop launcher,
# so it shows up in the app menu and the dock/taskbar show the brand icon.
# No root required (installs under ~/.local). Run: bash install.sh
set -e
cd "$(dirname "$0")"

APP_ID="io.github.gmountdrive.App"
NAME="GMount Drive"
BRAND="assets/brand"

# Pick the compiled binary (release if it exists, otherwise debug).
if [ -x "target/release/gdrive-mount" ]; then
    BIN="$(pwd)/target/release/gdrive-mount"
elif [ -x "target/debug/gdrive-mount" ]; then
    BIN="$(pwd)/target/debug/gdrive-mount"
else
    echo "Binary not found. Build it first with: bash build.sh" >&2
    exit 1
fi

# 1. Icons in hicolor (several sizes).
ICONDIR="$HOME/.local/share/icons/hicolor"
for s in 16 32 64 128 256 512; do
    install -Dm644 "$BRAND/gmount-drive-icon-$s.png" "$ICONDIR/${s}x${s}/apps/$APP_ID.png"
done

# 2. The .desktop launcher.
APPS="$HOME/.local/share/applications"
mkdir -p "$APPS"
cat > "$APPS/$APP_ID.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=$NAME
Comment=Mount your Google Drive as a disk
Exec=$BIN
Icon=$APP_ID
Terminal=false
Categories=Utility;Network;FileTransfer;
StartupWMClass=$APP_ID
EOF

# 3. Refresh caches (if the tools are available).
gtk-update-icon-cache -f "$ICONDIR" 2>/dev/null || true
update-desktop-database "$APPS" 2>/dev/null || true

echo "✅ Installed for $USER."
echo "   Binary:  $BIN"
echo "   Look for '$NAME' in your applications (you may need to log out and back in for the dock icon)."
