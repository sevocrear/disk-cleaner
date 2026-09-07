#!/usr/bin/env bash
# Build from this checkout and install into ~/.local (dev / offline).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
chmod +x "$ROOT/scripts/package-linux.sh" "$ROOT/install.sh" "$ROOT/scripts/uninstall.sh"
"$ROOT/scripts/package-linux.sh"
DISK_CLEANER_TARBALL="$ROOT/dist/disk-cleaner-linux-x86_64.tar.gz" \
  PREFIX="${PREFIX:-$HOME/.local}" \
  "$ROOT/install.sh"
