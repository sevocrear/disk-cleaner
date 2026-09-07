#!/usr/bin/env bash
# Remove Disk Cleaner CLI, GUI, desktop entry, and icons from a user prefix.
set -euo pipefail

PREFIX="${PREFIX:-$HOME/.local}"
BIN_DIR="$PREFIX/bin"
APP_DIR="$PREFIX/share/applications"
ICON_BASE="$PREFIX/share/icons/hicolor"

rm -f "$BIN_DIR/disk-cleaner" \
  "$BIN_DIR/disk-cleaner-app" \
  "$BIN_DIR/disk-cleaner-uninstall"

rm -f "$APP_DIR/com.diskcleaner.app.desktop"

for size in 32 128 256 512; do
  rm -f "$ICON_BASE/${size}x${size}/apps/com.diskcleaner.app.png"
done

if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$APP_DIR" 2>/dev/null || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -f -t "$ICON_BASE" 2>/dev/null || true
fi

echo "Disk Cleaner removed from $PREFIX"
echo "Config left in place: ~/.config/disk-cleaner/ (delete manually if unwanted)"
