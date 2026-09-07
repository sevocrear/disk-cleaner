# Disk Cleaner

Ubuntu / Linux disk cleaner with a pastel desktop GUI and a matching CLI.

Finds reclaimable space from Docker leftovers, package caches, old downloads, unused apps, duplicate files, and media libraries. Review before delete. Manage mounted disks and the XDG trash.

**Keywords:** Ubuntu disk cleaner, free disk space Linux, Docker prune GUI, duplicate file finder, trash restore, Flatpak Snap cleanup.

## Install (one command)

Linux x86_64:

```bash
curl -fsSL https://raw.githubusercontent.com/sevocrear/disk-cleaner/main/install.sh | bash
```

This installs both tools into `~/.local`:

| Command | What it is |
| --- | --- |
| `disk-cleaner` | CLI (dry-run by default) |
| `disk-cleaner-app` | Desktop GUI (also in the Apps menu as **Disk Cleaner**) |

Uninstall:

```bash
disk-cleaner-uninstall
# or:
curl -fsSL https://raw.githubusercontent.com/sevocrear/disk-cleaner/main/scripts/uninstall.sh | bash
```

### Requirements

- **CLI:** no extra packages
- **GUI runtime (Ubuntu/Debian):** `libwebkit2gtk-4.1-0` (usually already present on desktop installs)

```bash
sudo apt install libwebkit2gtk-4.1-0
```

Ensure `~/.local/bin` is on your `PATH`.

## Quick start

```bash
# Scan only (safe)
disk-cleaner

# Apply permanently (CLI deletes for real; confirm large jobs with --yes)
disk-cleaner --apply --yes

# Open the GUI — select items, clean to Trash by default
disk-cleaner-app
```

## What it cleans

| Phase | Examples |
| --- | --- |
| Docker | unused images, containers, build cache |
| Caches | user caches, optional package-manager caches |
| Files | old large files under home / extras |
| Apps | idle Snap / Flatpak / AppImage / optional `/opt` |
| Dupes | content-identical files above a size floor |
| Media | large items under Pictures, Videos, Music, Downloads |

GUI panels: **Deep clean**, **Review**, **Disks**, **Trash**, **Settings**. After a clean you get a short report (files removed, space freed).

## Safety

- Does not walk all of `/`
- Hard deny list for system prefixes (`/usr`, `/etc`, …)
- Protect globs for SSH keys, browsers, Hugging Face caches (HF opt-in)
- GUI defaults to **move to Trash**; permanent delete is a setting
- CLI `--apply` deletes permanently (same model as classic host cleaners)

Config lives at `~/.config/disk-cleaner/config.toml`.

## Build from source

Need: Rust stable, Node.js 18+, and on Linux the WebKitGTK 4.1 **dev** packages for the GUI:

```bash
sudo apt install build-essential curl \
  libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev patchelf
```

```bash
git clone https://github.com/sevocrear/disk-cleaner.git
cd disk-cleaner
./scripts/package-linux.sh   # builds UI + binaries, writes dist/*.tar.gz
DISK_CLEANER_TARBALL=./dist/disk-cleaner-linux-x86_64.tar.gz ./install.sh
```

Dev loop:

```bash
cargo build --release -p disk-cleaner-cli
cd ui && npm install && npm run build && cd ..
cargo run --release -p disk-cleaner-app
```

## Project layout

```
crates/disk-cleaner-engine/   shared scan / trash / disks engine
crates/disk-cleaner-cli/      disk-cleaner binary
src-tauri/                    Tauri 2 shell (disk-cleaner-app)
ui/                           React UI
install.sh                    curl | bash installer (GitHub Release)
scripts/package-linux.sh      release tarball
scripts/uninstall.sh          remove user install
reference/host_cleaner.py     original Python reference
```

## License

MIT
