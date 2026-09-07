#!/usr/bin/env bash
# Disk Cleaner — one-command install for Linux x86_64
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/sevocrear/disk-cleaner/main/install.sh | bash
#   PREFIX=$HOME/.local bash install.sh
#   DISK_CLEANER_VERSION=v2.3.0 bash install.sh
#   DISK_CLEANER_TARBALL=./dist/disk-cleaner-linux-x86_64.tar.gz bash install.sh
set -euo pipefail

REPO="${DISK_CLEANER_REPO:-sevocrear/disk-cleaner}"
PREFIX="${PREFIX:-$HOME/.local}"
VERSION="${DISK_CLEANER_VERSION:-latest}"
ASSET_NAME="disk-cleaner-linux-x86_64.tar.gz"
BIN_DIR="$PREFIX/bin"
APP_DIR="$PREFIX/share/applications"
ICON_BASE="$PREFIX/share/icons/hicolor"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "error: required command not found: $1" >&2
    exit 1
  }
}

need_cmd curl
need_cmd tar
need_cmd install

arch="$(uname -m)"
os="$(uname -s)"
if [[ "$os" != "Linux" ]]; then
  echo "error: only Linux is supported (got $os)" >&2
  exit 1
fi
if [[ "$arch" != "x86_64" && "$arch" != "amd64" ]]; then
  echo "error: only x86_64 is supported (got $arch)" >&2
  exit 1
fi

download_release() {
  local url api tag
  if [[ -n "${DISK_CLEANER_TARBALL:-}" ]]; then
    echo "Using local package: $DISK_CLEANER_TARBALL"
    cp "$DISK_CLEANER_TARBALL" "$TMP/$ASSET_NAME"
    return
  fi

  need_cmd grep
  need_cmd sed

  if [[ "$VERSION" == "latest" ]]; then
    api="https://api.github.com/repos/${REPO}/releases/latest"
  else
    api="https://api.github.com/repos/${REPO}/releases/tags/${VERSION}"
  fi

  echo "Fetching release metadata ($VERSION)…"
  tag="$(curl -fsSL "$api" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1)"
  if [[ -z "$tag" ]]; then
    echo "error: could not resolve release for $REPO ($VERSION)" >&2
    echo "hint: set DISK_CLEANER_VERSION=vX.Y.Z or DISK_CLEANER_TARBALL=/path/to/$ASSET_NAME" >&2
    exit 1
  fi

  url="https://github.com/${REPO}/releases/download/${tag}/${ASSET_NAME}"
  echo "Downloading $url"
  curl -fsSL "$url" -o "$TMP/$ASSET_NAME"
}

install_runtime_hint() {
  if ! ldconfig -p 2>/dev/null | grep -q 'libwebkit2gtk-4.1.so.0'; then
    if [[ ! -e /lib/x86_64-linux-gnu/libwebkit2gtk-4.1.so.0 && ! -e /usr/lib/x86_64-linux-gnu/libwebkit2gtk-4.1.so.0 ]]; then
      echo
      echo "GUI needs WebKitGTK 4.1 at runtime. On Ubuntu/Debian:"
      echo "  sudo apt install libwebkit2gtk-4.1-0"
      echo
    fi
  fi
}

echo "Disk Cleaner installer"
echo "  prefix: $PREFIX"
download_release

echo "Extracting…"
tar -xzf "$TMP/$ASSET_NAME" -C "$TMP"
PKG="$(find "$TMP" -maxdepth 1 -type d -name 'disk-cleaner-linux-*' | head -n1)"
if [[ -z "$PKG" ]]; then
  echo "error: unexpected package layout" >&2
  exit 1
fi

mkdir -p "$BIN_DIR" "$APP_DIR"
install -m 755 "$PKG/bin/disk-cleaner" "$BIN_DIR/disk-cleaner"
install -m 755 "$PKG/bin/disk-cleaner-app" "$BIN_DIR/disk-cleaner-app"

if [[ -f "$PKG/share/applications/com.diskcleaner.app.desktop" ]]; then
  install -m 644 "$PKG/share/applications/com.diskcleaner.app.desktop" \
    "$APP_DIR/com.diskcleaner.app.desktop"
  # Absolute Exec path so the Apps menu works even if PATH is incomplete
  if grep -q '^Exec=' "$APP_DIR/com.diskcleaner.app.desktop"; then
    sed -i "s|^Exec=.*|Exec=$BIN_DIR/disk-cleaner-app|" "$APP_DIR/com.diskcleaner.app.desktop"
  else
    echo "Exec=$BIN_DIR/disk-cleaner-app" >>"$APP_DIR/com.diskcleaner.app.desktop"
  fi
fi

if [[ -d "$PKG/share/icons" ]]; then
  mkdir -p "$ICON_BASE"
  cp -a "$PKG/share/icons/hicolor/." "$ICON_BASE/"
fi

if [[ -f "$PKG/uninstall.sh" ]]; then
  install -m 755 "$PKG/uninstall.sh" "$BIN_DIR/disk-cleaner-uninstall"
fi

if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$APP_DIR" 2>/dev/null || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -f -t "$ICON_BASE" 2>/dev/null || true
fi

install_runtime_hint

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *)
    echo
    echo "Add $BIN_DIR to your PATH, e.g. in ~/.bashrc:"
    echo "  export PATH=\"\$HOME/.local/bin:\$PATH\""
    ;;
esac

echo
echo "Installed:"
echo "  disk-cleaner       → $BIN_DIR/disk-cleaner"
echo "  disk-cleaner-app   → $BIN_DIR/disk-cleaner-app"
echo "  Apps menu entry    → Disk Cleaner"
echo "  uninstall          → $BIN_DIR/disk-cleaner-uninstall"
echo
echo "Try:  disk-cleaner --help"
echo "      disk-cleaner-app"
