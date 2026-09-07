#!/usr/bin/env bash
# Build release binaries and pack a Linux x86_64 tarball for GitHub Releases.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIST="$ROOT/dist"
STAGE="$DIST/disk-cleaner-linux-x86_64"
ASSET="$DIST/disk-cleaner-linux-x86_64.tar.gz"

source "$HOME/.cargo/env" 2>/dev/null || true

cd "$ROOT"

if [[ -d "$ROOT/.deps/prefix" ]]; then
  export PKG_CONFIG_PATH="$ROOT/.deps/prefix/usr/lib/x86_64-linux-gnu/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
fi

echo "Building UI…"
(
  cd "$ROOT/ui"
  if [[ -f package-lock.json ]]; then
    npm ci
  else
    npm install
  fi
  npm run build
)

echo "Building CLI + GUI (release)…"
cargo build --release -p disk-cleaner-cli -p disk-cleaner-app

CLI="$ROOT/target/release/disk-cleaner"
APP="$ROOT/target/release/disk-cleaner-app"
[[ -x "$CLI" && -x "$APP" ]] || {
  echo "error: release binaries missing" >&2
  exit 1
}

if command -v strip >/dev/null 2>&1; then
  strip -s "$CLI" "$APP" || true
fi

rm -rf "$STAGE"
mkdir -p "$STAGE/bin" \
  "$STAGE/share/applications" \
  "$STAGE/share/icons/hicolor/32x32/apps" \
  "$STAGE/share/icons/hicolor/128x128/apps" \
  "$STAGE/share/icons/hicolor/256x256/apps" \
  "$STAGE/share/icons/hicolor/512x512/apps"

install -m 755 "$CLI" "$STAGE/bin/disk-cleaner"
install -m 755 "$APP" "$STAGE/bin/disk-cleaner-app"
install -m 644 "$ROOT/assets/com.diskcleaner.app.desktop" \
  "$STAGE/share/applications/com.diskcleaner.app.desktop"
install -m 755 "$ROOT/scripts/uninstall.sh" "$STAGE/uninstall.sh"

copy_icon() {
  local size="$1"
  local src="$2"
  local dest="$STAGE/share/icons/hicolor/${size}x${size}/apps/com.diskcleaner.app.png"
  if [[ -f "$src" ]]; then
    install -m 644 "$src" "$dest"
  fi
}

copy_icon 32 "$ROOT/src-tauri/icons/32x32.png"
copy_icon 128 "$ROOT/src-tauri/icons/128x128.png"
copy_icon 256 "$ROOT/src-tauri/icons/128x128@2x.png"
copy_icon 512 "$ROOT/src-tauri/icons/icon.png"

# Desktop entry uses bare binary name; install.sh rewrites Exec to an absolute path.
sed -i 's|^Exec=.*|Exec=disk-cleaner-app|' \
  "$STAGE/share/applications/com.diskcleaner.app.desktop"

tar -C "$DIST" -czf "$ASSET" "$(basename "$STAGE")"
echo "Wrote $ASSET ($(du -h "$ASSET" | awk '{print $1}'))"
