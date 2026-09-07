#!/usr/bin/env python3
"""Host disk cleaner — safe, parameterized reclaim of Docker, caches, old files, apps, dupes.

Dry-run by default. Real deletion requires --apply. Stdlib only.
"""

from __future__ import annotations

import argparse
import dataclasses
import fnmatch
import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import time
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path
from typing import Callable, Iterable, Iterator, Optional, Sequence

__version__ = "1.1.0"

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

DENY_PREFIXES = (
    "/usr",
    "/bin",
    "/sbin",
    "/lib",
    "/lib32",
    "/lib64",
    "/libx32",
    "/etc",
    "/boot",
    "/sys",
    "/proc",
    "/dev",
    "/run",
    "/snap",  # system snap mount; user data is under ~/snap
    "/var/lib/docker",
    "/var/lib/containerd",
    "/var/lib/snapd",
)

DEFAULT_PROTECT_GLOBS = (
    "**/huggingface/**",
    "**/torch/**",
    "**/transformers/**",
    "**/.ssh/**",
    "**/google-chrome/**",
    "**/chromium/**",
    "**/yandex-browser/**",
    "**/mozilla/**",
    "**/firefox/**",
)

SNAP_PROTECT_NAMES = frozenset(
    {
        "bare",
        "snapd",
        "cups",
        "firmware-updater",
        "gtk-common-themes",
    }
)
SNAP_PROTECT_PREFIXES = (
    "core",
    "gnome-",
    "mesa",
    "gtk-",
    "kde-",
    "kf6-",
    "plasma-",
    "gaming-graphics",
    "lxqt-",
)

PARTIAL_HASH_BYTES = 1 * 1024 * 1024


# ---------------------------------------------------------------------------
# Data types
# ---------------------------------------------------------------------------


@dataclasses.dataclass
class Action:
    phase: str
    kind: str  # delete_file | rmdir | snap_remove | flatpak_uninstall | docker_cmd | cache_cmd | report
    path: str
    bytes: int = 0
    detail: str = ""
    command: Optional[list[str]] = None

    def to_dict(self) -> dict:
        d = dataclasses.asdict(self)
        return d


@dataclasses.dataclass
class PhaseResult:
    name: str
    reclaimable_bytes: int = 0
    actions: list[Action] = dataclasses.field(default_factory=list)
    notes: list[str] = dataclasses.field(default_factory=list)

    def add(self, action: Action) -> None:
        self.actions.append(action)
        self.reclaimable_bytes += max(0, action.bytes)


@dataclasses.dataclass
class Config:
    apply: bool = False
    yes: bool = False
    docker_unused_days: int = 14
    file_unused_days: int = 60
    app_unused_days: int = 60
    min_file_size: int = 1 * 1024 * 1024
    min_dupe_size: int = 1 * 1024 * 1024
    dedupe_keep: str = "newest"  # newest|oldest|first
    extra_roots: list[Path] = dataclasses.field(default_factory=list)
    dedupe_roots: list[Path] = dataclasses.field(default_factory=list)
    protect_globs: list[str] = dataclasses.field(default_factory=list)
    protect_apps: list[str] = dataclasses.field(default_factory=list)
    skip_docker: bool = False
    skip_caches: bool = False
    skip_files: bool = False
    skip_apps: bool = False
    skip_dupes: bool = False
    only: Optional[str] = None  # docker|caches|files|apps|dupes
    journal_vacuum: str = "7d"
    include_docker_volumes: bool = False
    include_hf_cache: bool = False
    include_opt_apps: bool = False
    confirm_above: int = 5 * 1024 * 1024 * 1024
    cross_fs: bool = False
    home: Path = dataclasses.field(default_factory=lambda: Path.home())
    report_dir: Optional[Path] = None
    # When set, use exactly these roots (no /tmp/.cache defaults). Useful for tests.
    file_roots: Optional[list[Path]] = None
    # Subprocess timeout (seconds). pkg managers / docker can hang without this.
    cmd_timeout_sec: int = 120
    docker_timeout_sec: int = 600
    # uv/pip/npm cache prune — off by default (uv cache prune can hang for ages)
    include_pkg_managers: bool = False
    # Test hooks
    run_cmd: Optional[Callable[..., subprocess.CompletedProcess]] = None
    which: Optional[Callable[[str], Optional[str]]] = None


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def parse_size(s: str) -> int:
    """Parse sizes like 1M, 2G, 512K, 100."""
    s = s.strip().replace(" ", "")
    if not s:
        raise ValueError("empty size")
    m = re.fullmatch(r"(?i)(\d+(?:\.\d+)?)([kmgt]?b?)?", s)
    if not m:
        raise ValueError(f"invalid size: {s}")
    num = float(m.group(1))
    unit = (m.group(2) or "").upper().rstrip("B")
    mult = {"": 1, "K": 1024, "M": 1024**2, "G": 1024**3, "T": 1024**4}[unit]
    return int(num * mult)


def format_bytes(n: int) -> str:
    n = abs(int(n))
    for unit, div in (("T", 1024**4), ("G", 1024**3), ("M", 1024**2), ("K", 1024)):
        if n >= div:
            return f"{n / div:.1f}{unit}"
    return f"{n}B"


def now_ts() -> float:
    return time.time()


def days_to_seconds(days: int) -> float:
    return days * 86400.0


def docker_until_filter(days: int) -> str:
    """Docker filter until=<duration> — use hours for granularity."""
    hours = max(1, int(days) * 24)
    return f"until={hours}h"


def _which(name: str, cfg: Config) -> Optional[str]:
    if cfg.which is not None:
        return cfg.which(name)
    return shutil.which(name)


def run_command(
    cmd: Sequence[str],
    cfg: Config,
    *,
    check: bool = False,
    capture: bool = True,
    timeout: Optional[float] = None,
) -> subprocess.CompletedProcess:
    """Run a command with timeout. Never hangs forever."""
    if timeout is None:
        timeout = float(cfg.cmd_timeout_sec)
    # Docker prune/build can legitimately take longer
    if cmd and cmd[0] == "docker" and timeout < cfg.docker_timeout_sec:
        if any(x in cmd for x in ("prune", "rmi", "builder")):
            timeout = float(cfg.docker_timeout_sec)

    runner = cfg.run_cmd
    if runner is not None:
        try:
            return runner(
                list(cmd),
                check=check,
                capture_output=capture,
                text=True,
                timeout=timeout,
            )
        except TypeError:
            # Test doubles may not accept timeout=
            return runner(list(cmd), check=check, capture_output=capture, text=True)

    try:
        return subprocess.run(
            list(cmd),
            check=check,
            capture_output=capture,
            text=True,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired as e:
        return subprocess.CompletedProcess(
            args=list(cmd),
            returncode=124,
            stdout=(e.stdout.decode() if isinstance(e.stdout, bytes) else (e.stdout or "")),
            stderr=(
                (e.stderr.decode() if isinstance(e.stderr, bytes) else (e.stderr or ""))
                + f"\n[host_cleaner] timed out after {timeout}s"
            ),
        )


def path_is_denied(path: Path) -> bool:
    try:
        resolved = path.resolve()
    except OSError:
        resolved = path
    s = str(resolved)
    for prefix in DENY_PREFIXES:
        if s == prefix or s.startswith(prefix + "/"):
            # Allow ~/snap user data — DENY is /snap (system), not home snap
            return True
    return False


def matches_protect(path: Path, globs: Sequence[str]) -> bool:
    """Return True if path should be protected.

    Supports simple names, shell globs, and `**/seg/**` style patterns
    (fnmatch alone does not treat `**` as recursive).
    """
    s = str(path)
    parts = path.parts
    name = path.name
    for g in globs:
        if not g:
            continue
        if name == g or s == g:
            return True
        # Segment protect: "huggingface" or "**/huggingface/**"
        seg = g.strip("*").strip("/")
        if seg and "/" not in seg and (seg in parts or name == seg):
            return True
        if fnmatch.fnmatch(s, g) or fnmatch.fnmatch(name, g):
            return True
        try:
            if path.match(g) or Path(name).match(g):
                return True
        except ValueError:
            pass
    return False


def default_file_roots(cfg: Config) -> list[Path]:
    if cfg.file_roots is not None:
        return list(cfg.file_roots)
    roots = [
        Path("/tmp"),
        Path("/var/tmp"),
        cfg.home / ".cache",
        cfg.home / ".local" / "share" / "Trash",
    ]
    roots.extend(cfg.extra_roots)
    # Include non-existing extra_roots so callers can create them; skip missing defaults.
    out: list[Path] = []
    for r in roots:
        if r in cfg.extra_roots or r.exists():
            out.append(r)
    return out


def default_dedupe_roots(cfg: Config) -> list[Path]:
    if cfg.dedupe_roots:
        return list(cfg.dedupe_roots)
    # Intentionally exclude Trash — hashing deleted trees (e.g. Blender) can take
    # minutes and look like a freeze. Trash is cleaned by age in the caches phase.
    roots: list[Path] = []
    for r in (
        cfg.home / "Downloads",
        cfg.home / "Applications",
        cfg.home / "AppImages",
        Path("/tmp"),
        Path("/var/tmp"),
    ):
        if r.exists():
            roots.append(r)
    roots.extend(cfg.extra_roots)
    return roots


def iter_files(
    root: Path,
    *,
    cross_fs: bool = False,
    follow_symlinks: bool = False,
) -> Iterator[Path]:
    """Walk files under root; do not escape via symlinks."""
    if not root.exists():
        return
    try:
        root_dev = root.stat().st_dev
    except OSError:
        return

    stack = [root]
    while stack:
        current = stack.pop()
        try:
            with os.scandir(current) as it:
                for entry in it:
                    try:
                        if entry.is_symlink() and not follow_symlinks:
                            continue
                        if entry.is_dir(follow_symlinks=False):
                            p = Path(entry.path)
                            if path_is_denied(p):
                                continue
                            if not cross_fs:
                                try:
                                    if entry.stat(follow_symlinks=False).st_dev != root_dev:
                                        continue
                                except OSError:
                                    continue
                            stack.append(p)
                        elif entry.is_file(follow_symlinks=False):
                            yield Path(entry.path)
                    except OSError:
                        continue
        except OSError:
            continue


def file_age_ok_for_delete(path: Path, cutoff: float) -> bool:
    """True if both mtime and atime are older than cutoff timestamp."""
    try:
        st = path.stat()
    except OSError:
        return False
    return st.st_mtime < cutoff and st.st_atime < cutoff


def dir_size(path: Path) -> int:
    total = 0
    if path.is_file():
        try:
            return path.stat().st_size
        except OSError:
            return 0
    for f in iter_files(path):
        try:
            total += f.stat().st_size
        except OSError:
            pass
    return total


def newest_activity(path: Path) -> Optional[float]:
    """Newest atime/mtime under path (or the file itself)."""
    if not path.exists():
        return None
    newest: Optional[float] = None
    if path.is_file():
        try:
            st = path.stat()
            return max(st.st_atime, st.st_mtime)
        except OSError:
            return None
    for f in iter_files(path):
        try:
            st = f.stat()
            t = max(st.st_atime, st.st_mtime)
            if newest is None or t > newest:
                newest = t
        except OSError:
            continue
    if newest is None:
        try:
            st = path.stat()
            newest = max(st.st_atime, st.st_mtime)
        except OSError:
            return None
    return newest


def hash_file(path: Path, *, limit: Optional[int] = None) -> str:
    h = hashlib.sha256()
    remaining = limit
    with path.open("rb") as f:
        while True:
            chunk_size = 1024 * 1024
            if remaining is not None:
                if remaining <= 0:
                    break
                chunk_size = min(chunk_size, remaining)
            chunk = f.read(chunk_size)
            if not chunk:
                break
            h.update(chunk)
            if remaining is not None:
                remaining -= len(chunk)
    return h.hexdigest()


def is_snap_protected(name: str, protect_apps: Sequence[str]) -> bool:
    if name in protect_apps or name in SNAP_PROTECT_NAMES:
        return True
    for p in SNAP_PROTECT_PREFIXES:
        if name.startswith(p):
            return True
    return False


def resolve_protect_globs(cfg: Config) -> list[str]:
    globs = list(DEFAULT_PROTECT_GLOBS) + list(cfg.protect_globs)
    if cfg.include_hf_cache:
        globs = [
            g
            for g in globs
            if "huggingface" not in g and "torch" not in g and "transformers" not in g
        ]
    return globs


def should_run_phase(name: str, cfg: Config) -> bool:
    if cfg.only is not None:
        return cfg.only == name
    skips = {
        "docker": cfg.skip_docker,
        "caches": cfg.skip_caches,
        "files": cfg.skip_files,
        "apps": cfg.skip_apps,
        "dupes": cfg.skip_dupes,
    }
    return not skips.get(name, False)


# ---------------------------------------------------------------------------
# Phase A — Docker
# ---------------------------------------------------------------------------


def parse_reclaimed_bytes(text: str) -> Optional[int]:
    """Parse 'Total reclaimed space: 1.021GB' or 'Total:\\t188.1GB' from docker output."""
    if not text:
        return None
    for line in text.splitlines():
        m = re.search(
            r"(?i)(?:total reclaimed space|total):\s*([0-9.]+)\s*([KMGT])B?\b",
            line.strip(),
        )
        if not m:
            m = re.search(r"(?i)(?:total reclaimed space|total):\s*([0-9.]+)\s*B\b", line.strip())
            if m:
                return int(float(m.group(1)))
            continue
        try:
            return parse_size(m.group(1) + m.group(2))
        except ValueError:
            continue
    return None


def docker_image_created_ts(created_at: str) -> Optional[float]:
    """Parse docker CreatedAt strings to unix ts."""
    created_at = created_at.strip()
    for fmt in (
        "%Y-%m-%d %H:%M:%S %z %Z",
        "%Y-%m-%d %H:%M:%S %z",
        "%Y-%m-%dT%H:%M:%S%z",
    ):
        try:
            return datetime.strptime(created_at, fmt).timestamp()
        except ValueError:
            continue
    # Fallback: dateutil-less — try dropping TZ name
    m = re.match(r"(\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2} [+-]\d{4})", created_at)
    if m:
        try:
            return datetime.strptime(m.group(1), "%Y-%m-%d %H:%M:%S %z").timestamp()
        except ValueError:
            pass
    return None


def collect_unused_old_image_ids(cfg: Config, days: int) -> tuple[list[str], int, list[str]]:
    """Return (image_ids_or_refs to remove, estimated_bytes, notes).

    Docker's --filter until= is unreliable on Engine 29/buildx; age-filter manually.
    Never removes images used by any container.
    """
    notes: list[str] = []
    used: set[str] = set()
    try:
        cp = run_command(
            ["docker", "ps", "-a", "--format", "{{.Image}}\t{{.ID}}"],
            cfg,
        )
        if cp.returncode == 0 and cp.stdout:
            for line in cp.stdout.splitlines():
                parts = line.split("\t")
                for p in parts:
                    if p.strip():
                        used.add(p.strip())
    except Exception as e:  # noqa: BLE001
        notes.append(f"docker ps failed: {e}")
        return [], 0, notes

    # Also resolve used image IDs via inspect when possible
    try:
        cp = run_command(
            ["docker", "ps", "-aq"],
            cfg,
        )
        if cp.returncode == 0 and cp.stdout.strip():
            cids = [x for x in cp.stdout.split() if x]
            if cids:
                icp = run_command(
                    ["docker", "inspect", "--format", "{{.Image}}", *cids],
                    cfg,
                )
                if icp.returncode == 0 and icp.stdout:
                    for line in icp.stdout.splitlines():
                        used.add(line.strip())
                        # short id without sha256:
                        if line.startswith("sha256:"):
                            used.add(line.split(":", 1)[1][:12])
    except Exception as e:  # noqa: BLE001
        notes.append(f"inspect containers failed: {e}")

    cutoff = now_ts() - days_to_seconds(days)
    to_remove: list[str] = []
    est = 0
    try:
        cp = run_command(
            [
                "docker",
                "images",
                "--format",
                "{{.ID}}\t{{.Repository}}:{{.Tag}}\t{{.CreatedAt}}\t{{.Size}}",
            ],
            cfg,
        )
        if cp.returncode != 0 or not cp.stdout:
            notes.append("docker images listing failed")
            return [], 0, notes
        seen_ids: set[str] = set()
        for line in cp.stdout.splitlines():
            parts = line.split("\t")
            if len(parts) < 4:
                continue
            img_id, ref, created_at, size_s = parts[0], parts[1], parts[2], parts[3]
            if img_id in seen_ids:
                continue
            # In use?
            if (
                img_id in used
                or ref in used
                or any(u.startswith(img_id) or img_id.startswith(u.replace("sha256:", "")[:12]) for u in used)
            ):
                continue
            # Match repository name without tag against running image refs
            repo = ref.rsplit(":", 1)[0] if ref != "<none>:<none>" else ""
            if ref in used or (repo and repo in used):
                continue
            # Check short-id / digest usage more carefully via docker image inspect is expensive;
            # use: if any container Image field contains this id
            skip = False
            for u in used:
                u_short = u.replace("sha256:", "")[:12]
                if img_id.startswith(u_short) or u_short.startswith(img_id[:12]):
                    skip = True
                    break
                if u == ref or (repo and u == repo):
                    skip = True
                    break
            if skip:
                continue
            ts = docker_image_created_ts(created_at)
            if ts is None:
                notes.append(f"unparsed CreatedAt for {ref}: {created_at}")
                continue
            if ts >= cutoff:
                continue
            seen_ids.add(img_id)
            to_remove.append(img_id)
            try:
                # Size like "15.3GB" or "971MB"
                est += parse_size(size_s.replace("B", ""))
            except ValueError:
                pass
    except Exception as e:  # noqa: BLE001
        notes.append(f"docker images failed: {e}")
        return [], 0, notes
    return to_remove, est, notes


def phase_docker(cfg: Config) -> PhaseResult:
    result = PhaseResult(name="docker")
    if not _which("docker", cfg):
        result.notes.append("docker not found; skipped")
        return result

    until = docker_until_filter(cfg.docker_unused_days)

    # Estimate reclaimable from docker system df (best-effort)
    try:
        cp = run_command(["docker", "system", "df", "--format", "{{.Type}}\t{{.Reclaimable}}"], cfg)
        if cp.returncode == 0 and cp.stdout:
            for line in cp.stdout.splitlines():
                result.notes.append(f"df: {line.strip()}")
    except Exception as e:  # noqa: BLE001
        result.notes.append(f"docker system df failed: {e}")

    # Containers: until= filter works on classic prune
    result.add(
        Action(
            phase="docker",
            kind="docker_cmd",
            path="container_prune",
            bytes=0,
            detail=until,
            command=["docker", "container", "prune", "-f", "--filter", until],
        )
    )

    # Images: do NOT use --filter until= (broken/no-op on Engine 29). Age-filter manually.
    img_ids, img_est, img_notes = collect_unused_old_image_ids(cfg, cfg.docker_unused_days)
    result.notes.extend(img_notes)
    if img_ids:
        result.add(
            Action(
                phase="docker",
                kind="docker_cmd",
                path="image_rmi",
                bytes=img_est,
                detail=f"unused + older than {cfg.docker_unused_days}d ({len(img_ids)} images)",
                command=["docker", "rmi", "-f", *img_ids],
            )
        )
    else:
        result.notes.append("no unused images older than threshold")

    # Build cache: buildx ignores until= (reclaims 0B). Prune all unused cache with -a.
    bytes_est = 0
    try:
        bcp = run_command(["docker", "builder", "du"], cfg)
        if bcp.returncode == 0 and bcp.stdout:
            for line in bcp.stdout.splitlines():
                if "Reclaimable:" in line:
                    part = line.split(":", 1)[1].strip().split()[0]
                    try:
                        bytes_est = parse_size(part.replace("B", ""))
                    except ValueError:
                        pass
    except Exception:  # noqa: BLE001
        pass
    result.add(
        Action(
            phase="docker",
            kind="docker_cmd",
            path="builder_prune",
            bytes=bytes_est,
            detail="all unused build cache (buildx until= is a no-op)",
            command=["docker", "builder", "prune", "-af"],
        )
    )

    result.add(
        Action(
            phase="docker",
            kind="docker_cmd",
            path="network_prune",
            bytes=0,
            detail="unused networks",
            command=["docker", "network", "prune", "-f"],
        )
    )

    if cfg.include_docker_volumes:
        # until= often a no-op for volumes too — unused volumes only
        result.add(
            Action(
                phase="docker",
                kind="docker_cmd",
                path="volume_prune",
                bytes=0,
                detail="unused volumes (opt-in)",
                command=["docker", "volume", "prune", "-f"],
            )
        )
    return result


# ---------------------------------------------------------------------------
# Phase B — Caches
# ---------------------------------------------------------------------------


def phase_caches(cfg: Config) -> PhaseResult:
    result = PhaseResult(name="caches")
    protect = resolve_protect_globs(cfg)
    cutoff = now_ts() - days_to_seconds(cfg.file_unused_days)
    cache_root = cfg.home / ".cache"

    if cache_root.is_dir():
        try:
            for entry in os.scandir(cache_root):
                p = Path(entry.path)
                if matches_protect(p, protect):
                    continue
                try:
                    st = entry.stat(follow_symlinks=False)
                except OSError:
                    continue
                # Directory entry: use newest activity; file: own times
                if entry.is_dir(follow_symlinks=False):
                    act = newest_activity(p)
                    if act is None or act >= cutoff:
                        continue
                    size = dir_size(p)
                else:
                    if not (st.st_mtime < cutoff and st.st_atime < cutoff):
                        continue
                    size = st.st_size
                result.add(
                    Action(
                        phase="caches",
                        kind="delete_tree" if entry.is_dir(follow_symlinks=False) else "delete_file",
                        path=str(p),
                        bytes=size,
                        detail=f"unused >{cfg.file_unused_days}d",
                    )
                )
        except OSError as e:
            result.notes.append(f"scan .cache failed: {e}")

    # Package manager caches — opt-in only (uv/npm prune frequently hang for minutes)
    if cfg.include_pkg_managers:
        pkg_cmds: list[tuple[str, list[str]]] = []
        if _which("uv", cfg):
            pkg_cmds.append(("uv_cache_prune", ["uv", "cache", "prune"]))
        if _which("pip", cfg):
            pkg_cmds.append(("pip_cache_purge", ["pip", "cache", "purge"]))
        if _which("npm", cfg):
            pkg_cmds.append(("npm_cache_clean", ["npm", "cache", "clean", "--force"]))
        if _which("cargo-cache", cfg):
            pkg_cmds.append(("cargo_cache", ["cargo-cache", "-a"]))
        for name, cmd in pkg_cmds:
            result.add(
                Action(
                    phase="caches",
                    kind="cache_cmd",
                    path=name,
                    bytes=0,
                    command=cmd,
                    detail="package manager cache (opt-in)",
                )
            )

    if os.geteuid() == 0 and _which("apt-get", cfg):
        result.add(
            Action(
                phase="caches",
                kind="cache_cmd",
                path="apt_clean",
                bytes=0,
                command=["apt-get", "clean"],
                detail="apt clean",
            )
        )

    # journalctl vacuum needs privileges; skip for normal users (it hangs/fails otherwise)
    if os.geteuid() == 0 and _which("journalctl", cfg):
        result.add(
            Action(
                phase="caches",
                kind="cache_cmd",
                path="journal_vacuum",
                bytes=0,
                command=["journalctl", f"--vacuum-time={cfg.journal_vacuum}"],
                detail=f"vacuum {cfg.journal_vacuum}",
            )
        )

    trash = cfg.home / ".local" / "share" / "Trash"
    for sub in ("files", "info"):
        d = trash / sub
        if not d.is_dir():
            continue
        for f in iter_files(d):
            if file_age_ok_for_delete(f, cutoff):
                try:
                    sz = f.stat().st_size
                except OSError:
                    sz = 0
                result.add(
                    Action(
                        phase="caches",
                        kind="delete_file",
                        path=str(f),
                        bytes=sz,
                        detail="trash",
                    )
                )
    return result


# ---------------------------------------------------------------------------
# Phase C — Age-based files
# ---------------------------------------------------------------------------


def phase_files(cfg: Config) -> PhaseResult:
    result = PhaseResult(name="files")
    protect = resolve_protect_globs(cfg)
    cutoff = now_ts() - days_to_seconds(cfg.file_unused_days)
    candidates: list[Path] = []

    for root in default_file_roots(cfg):
        if path_is_denied(root):
            result.notes.append(f"denied root skipped: {root}")
            continue
        if not root.exists():
            continue
        for f in iter_files(root, cross_fs=cfg.cross_fs):
            if path_is_denied(f):
                continue
            if matches_protect(f, protect):
                continue
            try:
                st = f.stat()
            except OSError:
                continue
            if st.st_size < cfg.min_file_size:
                continue
            if not file_age_ok_for_delete(f, cutoff):
                continue
            candidates.append(f)
            result.add(
                Action(
                    phase="files",
                    kind="delete_file",
                    path=str(f),
                    bytes=st.st_size,
                    detail=f"mtime+atime >{cfg.file_unused_days}d",
                )
            )

    # Plan empty-dir cleanup for parents that become empty (applied later)
    dirs_to_check: set[Path] = set()
    for f in candidates:
        parent = f.parent
        while True:
            # Don't remove the allowlisted root itself
            roots = {r.resolve() for r in default_file_roots(cfg) if r.exists()}
            try:
                if parent.resolve() in roots:
                    break
            except OSError:
                break
            dirs_to_check.add(parent)
            if parent == parent.parent:
                break
            parent = parent.parent

    for d in sorted(dirs_to_check, key=lambda p: len(str(p)), reverse=True):
        result.add(
            Action(
                phase="files",
                kind="rmdir_if_empty",
                path=str(d),
                bytes=0,
                detail="empty after age purge",
            )
        )
    return result


# ---------------------------------------------------------------------------
# Phase D — Unused apps
# ---------------------------------------------------------------------------


def phase_apps(cfg: Config) -> PhaseResult:
    result = PhaseResult(name="apps")
    cutoff = now_ts() - days_to_seconds(cfg.app_unused_days)
    protect_apps = list(cfg.protect_apps)

    # --- Snap ---
    if _which("snap", cfg):
        try:
            cp = run_command(["snap", "list"], cfg)
            if cp.returncode == 0 and cp.stdout:
                lines = cp.stdout.strip().splitlines()
                # header + rows
                for line in lines[1:]:
                    parts = line.split()
                    if not parts:
                        continue
                    name = parts[0]
                    notes = parts[-1] if len(parts) > 1 else ""
                    if "base" in notes.lower():
                        continue
                    if is_snap_protected(name, protect_apps):
                        continue
                    user_data = cfg.home / "snap" / name
                    act = newest_activity(user_data) if user_data.exists() else None
                    var_common = Path("/var/snap") / name / "common"
                    if var_common.exists():
                        act2 = newest_activity(var_common)
                        if act2 is not None and (act is None or act2 > act):
                            act = act2
                    if act is None:
                        # No user data — treat as unused candidate only if install is old:
                        # use snap info install date if available; else skip (safer).
                        result.notes.append(f"snap {name}: no user data; skipped (unknown last-used)")
                        continue
                    if act >= cutoff:
                        continue
                    size = dir_size(user_data) if user_data.exists() else 0
                    result.add(
                        Action(
                            phase="apps",
                            kind="snap_remove",
                            path=name,
                            bytes=size,
                            detail=f"last_used={datetime.fromtimestamp(act, tz=timezone.utc).date().isoformat()}",
                            command=["snap", "remove", name],
                        )
                    )
        except Exception as e:  # noqa: BLE001
            result.notes.append(f"snap list failed: {e}")

    # --- AppImage ---
    appimage_dirs = [
        cfg.home / "Applications",
        cfg.home / "AppImages",
        cfg.home,
    ]
    seen: set[Path] = set()
    for d in appimage_dirs:
        if not d.is_dir():
            continue
        try:
            for entry in os.scandir(d):
                if not entry.name.lower().endswith(".appimage"):
                    continue
                p = Path(entry.path)
                if p in seen:
                    continue
                seen.add(p)
                # Protect by stem name
                stem = p.stem.split("_")[0].lower()
                if any(stem == a.lower() or p.name == a for a in protect_apps):
                    continue
                try:
                    st = entry.stat(follow_symlinks=False)
                except OSError:
                    continue
                last = max(st.st_atime, st.st_mtime)
                if last >= cutoff:
                    continue
                result.add(
                    Action(
                        phase="apps",
                        kind="delete_file",
                        path=str(p),
                        bytes=st.st_size,
                        detail=f"AppImage last_used={datetime.fromtimestamp(last, tz=timezone.utc).date().isoformat()}",
                    )
                )
        except OSError as e:
            result.notes.append(f"AppImage scan {d}: {e}")

    # --- Flatpak ---
    if _which("flatpak", cfg):
        try:
            cp = run_command(["flatpak", "list", "--app", "--columns=application"], cfg)
            if cp.returncode == 0 and cp.stdout:
                for line in cp.stdout.splitlines():
                    app_id = line.strip()
                    if not app_id or app_id in protect_apps:
                        continue
                    data = cfg.home / ".var" / "app" / app_id
                    act = newest_activity(data) if data.exists() else None
                    if act is None or act >= cutoff:
                        continue
                    size = dir_size(data) if data.exists() else 0
                    result.add(
                        Action(
                            phase="apps",
                            kind="flatpak_uninstall",
                            path=app_id,
                            bytes=size,
                            detail=f"last_used={datetime.fromtimestamp(act, tz=timezone.utc).date().isoformat()}",
                            command=["flatpak", "uninstall", "-y", app_id],
                        )
                    )
        except Exception as e:  # noqa: BLE001
            result.notes.append(f"flatpak list failed: {e}")

    # --- /opt report-only ---
    if cfg.include_opt_apps:
        opt = Path("/opt")
        if opt.is_dir():
            try:
                for entry in os.scandir(opt):
                    if not entry.is_dir(follow_symlinks=False):
                        continue
                    p = Path(entry.path)
                    act = newest_activity(p)
                    size = dir_size(p)
                    detail = "report-only"
                    if act is not None:
                        detail += f" last_used={datetime.fromtimestamp(act, tz=timezone.utc).date().isoformat()}"
                        if act < cutoff:
                            detail += " UNUSED_CANDIDATE"
                    result.notes.append(f"/opt {entry.name}: {format_bytes(size)} {detail}")
            except OSError as e:
                result.notes.append(f"/opt scan failed: {e}")

    return result


# ---------------------------------------------------------------------------
# Phase E — Duplicates
# ---------------------------------------------------------------------------


def _pick_keeper(paths: list[Path], policy: str) -> Path:
    if policy == "first":
        return sorted(paths, key=lambda p: str(p))[0]
    if policy == "oldest":
        return min(paths, key=lambda p: (p.stat().st_mtime, str(p)))
    # newest
    return max(paths, key=lambda p: (p.stat().st_mtime, str(p)))


def phase_dupes(cfg: Config) -> PhaseResult:
    result = PhaseResult(name="dupes")
    protect = resolve_protect_globs(cfg)
    by_size: dict[int, list[Path]] = defaultdict(list)
    scanned = 0

    roots = default_dedupe_roots(cfg)
    print(f"… scanning dupes in {len(roots)} root(s): {', '.join(str(r) for r in roots)}", flush=True)
    for root in roots:
        if path_is_denied(root):
            continue
        if not root.exists():
            continue
        for f in iter_files(root, cross_fs=cfg.cross_fs):
            if path_is_denied(f) or matches_protect(f, protect):
                continue
            try:
                st = f.stat()
            except OSError:
                continue
            if st.st_size < cfg.min_dupe_size:
                continue
            by_size[st.st_size].append(f)
            scanned += 1
            if scanned % 500 == 0:
                print(f"… hashed-size index: {scanned} candidate files", flush=True)

    print(f"… dupe size-index done ({scanned} files ≥ {format_bytes(cfg.min_dupe_size)})", flush=True)

    for size, paths in by_size.items():
        if len(paths) < 2:
            continue
        # Partial hash
        partial_groups: dict[str, list[Path]] = defaultdict(list)
        for p in paths:
            try:
                partial_groups[hash_file(p, limit=PARTIAL_HASH_BYTES)].append(p)
            except OSError:
                continue
        for _ph, group in partial_groups.items():
            if len(group) < 2:
                continue
            full_groups: dict[str, list[Path]] = defaultdict(list)
            for p in group:
                try:
                    full_groups[hash_file(p)].append(p)
                except OSError:
                    continue
            for digest, identical in full_groups.items():
                if len(identical) < 2:
                    continue
                try:
                    keeper = _pick_keeper(identical, cfg.dedupe_keep)
                except OSError:
                    continue
                for p in identical:
                    if p == keeper:
                        continue
                    if matches_protect(p, protect):
                        continue
                    result.add(
                        Action(
                            phase="dupes",
                            kind="delete_file",
                            path=str(p),
                            bytes=size,
                            detail=f"dupe of {keeper} sha256={digest[:12]}… keep={cfg.dedupe_keep}",
                        )
                    )
    return result


# ---------------------------------------------------------------------------
# Apply / execute
# ---------------------------------------------------------------------------


def execute_actions(actions: Iterable[Action], cfg: Config) -> list[dict]:
    """Execute actions when cfg.apply; always return log records."""
    log: list[dict] = []
    actionable = [a for a in actions if a.kind != "rmdir_if_empty" or cfg.apply]
    total_n = len([a for a in actionable if a.kind != "rmdir_if_empty"])
    done_n = 0
    for action in actions:
        rec = action.to_dict()
        rec["applied"] = False
        rec["error"] = None
        rec["reclaimed_bytes"] = None
        if not cfg.apply:
            log.append(rec)
            continue
        if action.kind != "rmdir_if_empty":
            done_n += 1
            label = action.command or action.path
            if isinstance(label, list):
                label = " ".join(label)
            print(f"→ [{done_n}/{total_n}] {action.kind}: {label}", flush=True)
        try:
            if action.kind in ("delete_file",) and action.path:
                p = Path(action.path)
                if p.is_file() or p.is_symlink():
                    try:
                        p.unlink()
                    except PermissionError:
                        # Trash / copied trees often lack u+w
                        os.chmod(p, stat.S_IWUSR | stat.S_IRUSR)
                        p.unlink()
                    rec["applied"] = True
                    rec["reclaimed_bytes"] = action.bytes
            elif action.kind == "delete_tree":
                p = Path(action.path)
                if p.is_dir():
                    def _onerror(func, path, _exc_info):
                        try:
                            os.chmod(path, stat.S_IWUSR | stat.S_IRUSR | stat.S_IXUSR)
                            func(path)
                        except OSError:
                            raise

                    shutil.rmtree(p, onerror=_onerror)
                    rec["applied"] = True
                    rec["reclaimed_bytes"] = action.bytes
            elif action.kind == "rmdir_if_empty":
                p = Path(action.path)
                if p.is_dir() and not any(p.iterdir()):
                    p.rmdir()
                    rec["applied"] = True
            elif action.kind in ("docker_cmd", "cache_cmd", "snap_remove", "flatpak_uninstall"):
                if action.command:
                    cp = run_command(action.command, cfg)
                    rec["applied"] = cp.returncode == 0
                    rec["stdout"] = (cp.stdout or "")[-2000:]
                    rec["stderr"] = (cp.stderr or "")[-2000:]
                    rec["reclaimed_bytes"] = parse_reclaimed_bytes(
                        (cp.stdout or "") + "\n" + (cp.stderr or "")
                    )
                    if rec["reclaimed_bytes"] is None and rec["applied"] and action.bytes:
                        if action.path in ("image_rmi", "builder_prune"):
                            if action.path == "image_rmi" or "Total:" in (cp.stdout or ""):
                                rec["reclaimed_bytes"] = action.bytes
                    if cp.returncode != 0:
                        err = f"exit {cp.returncode}"
                        if cp.returncode == 124:
                            err = f"timeout after {cfg.cmd_timeout_sec}s"
                        rec["error"] = err
                        print(f"  ! {rec['error']}", flush=True)
            elif action.kind == "report":
                rec["applied"] = True
            else:
                rec["error"] = f"unknown kind {action.kind}"
        except Exception as e:  # noqa: BLE001
            rec["error"] = str(e)
            print(f"  ! {e}", flush=True)
        log.append(rec)
    return log


def print_apply_results(log: list[dict]) -> int:
    """Print per-action apply outcome. Returns sum of reported reclaimed bytes."""
    print("--- apply results ---")
    total = 0
    failures = 0
    for rec in log:
        if rec.get("kind") == "rmdir_if_empty":
            continue
        status = "ok" if rec.get("applied") else "FAIL"
        if rec.get("error"):
            status = f"FAIL ({rec['error']})"
            failures += 1
        reclaimed = rec.get("reclaimed_bytes")
        if isinstance(reclaimed, int):
            total += reclaimed
            reb = format_bytes(reclaimed)
        else:
            reb = "?"
        line = f"  [{status}] {rec.get('path')} reclaimed={reb}"
        out = (rec.get("stdout") or "").strip().splitlines()
        err = (rec.get("stderr") or "").strip().splitlines()
        # Show last meaningful docker line
        tail = ""
        for src in (out, err):
            for candidate in reversed(src):
                if candidate.strip():
                    tail = candidate.strip()
                    break
            if tail:
                break
        if tail and len(tail) < 120:
            line += f" | {tail}"
        elif rec.get("error") and rec.get("stderr"):
            err_tail = (rec.get("stderr") or "").strip().splitlines()
            if err_tail:
                line += f" | {err_tail[-1][:120]}"
        print(line)
    print(f"Reported reclaimed: ~{format_bytes(total)}" + (f"  ({failures} failures)" if failures else ""))
    return total


def write_report(cfg: Config, results: list[PhaseResult], log: list[dict]) -> Path:
    report_dir = cfg.report_dir or (cfg.home / ".local" / "share" / "host_cleaner" / "reports")
    report_dir.mkdir(parents=True, exist_ok=True)
    stamp = datetime.now(tz=timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    path = report_dir / f"report_{stamp}.json"
    payload = {
        "version": __version__,
        "apply": cfg.apply,
        "timestamp": stamp,
        "phases": [
            {
                "name": r.name,
                "reclaimable_bytes": r.reclaimable_bytes,
                "notes": r.notes,
                "action_count": len(r.actions),
            }
            for r in results
        ],
        "actions": log,
    }
    path.write_text(json.dumps(payload, indent=2), encoding="utf-8")
    return path


def confirm_or_abort(total: int, cfg: Config) -> bool:
    if not cfg.apply:
        return True
    if total <= cfg.confirm_above:
        return True
    if cfg.yes:
        return True
    if not sys.stdin.isatty():
        print(
            f"Refusing to apply reclaim of {format_bytes(total)} without --yes "
            f"(> {format_bytes(cfg.confirm_above)})",
            file=sys.stderr,
        )
        return False
    ans = input(f"About to free ~{format_bytes(total)}. Type 'y' to continue: ").strip().lower()
    return ans in ("y", "yes")


def print_summary(results: list[PhaseResult], cfg: Config) -> int:
    mode = "APPLY" if cfg.apply else "dry-run"
    print(f"=== host_cleaner {mode} ===")
    total = 0
    for r in results:
        total += r.reclaimable_bytes
        summary_parts = [f"{len(r.actions)} actions", format_bytes(r.reclaimable_bytes)]
        if r.notes:
            summary_parts.append("; ".join(r.notes[:3]))
        print(f"{r.name.capitalize():8} {', '.join(summary_parts)}")
        # Show top actions briefly
        shown = 0
        for a in sorted(r.actions, key=lambda x: x.bytes, reverse=True):
            if shown >= 8:
                break
            if a.kind == "rmdir_if_empty":
                continue
            print(f"  - [{a.kind}] {a.path} ({format_bytes(a.bytes)}) {a.detail}")
            shown += 1
        if len(r.actions) > shown:
            print(f"  … {len(r.actions) - shown} more")
    print(f"TOTAL:   ~{format_bytes(total)} would free" if not cfg.apply else f"TOTAL:   ~{format_bytes(total)} targeted")
    if not cfg.apply:
        print("\nRe-run with --apply [--yes] to execute.")
    return total


def df_snapshot(cfg: Config) -> str:
    if not _which("df", cfg):
        return ""
    try:
        cp = run_command(["df", "-h", "/"], cfg)
        return (cp.stdout or "").strip()
    except Exception:  # noqa: BLE001
        return ""


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="host_cleaner",
        description="Safe host disk cleaner (Docker, caches, old files, unused apps, duplicates).",
    )
    p.add_argument("--version", action="version", version=f"%(prog)s {__version__}")
    p.add_argument("--apply", action="store_true", help="Actually delete / run prune commands")
    p.add_argument("--yes", action="store_true", help="Skip confirmation for large reclaim")
    p.add_argument("--docker-unused-days", type=int, default=14)
    p.add_argument("--file-unused-days", type=int, default=60)
    p.add_argument("--app-unused-days", type=int, default=60)
    p.add_argument("--min-file-size", type=parse_size, default="1M")
    p.add_argument("--min-dupe-size", type=parse_size, default="1M")
    p.add_argument("--dedupe-keep", choices=("newest", "oldest", "first"), default="newest")
    p.add_argument("--extra-root", action="append", default=[], dest="extra_roots")
    p.add_argument("--dedupe-root", action="append", default=[], dest="dedupe_roots")
    p.add_argument("--protect", action="append", default=[], dest="protect_globs")
    p.add_argument("--protect-app", action="append", default=[], dest="protect_apps")
    p.add_argument("--journal-vacuum", default="7d")
    p.add_argument("--include-docker-volumes", action="store_true")
    p.add_argument("--include-hf-cache", action="store_true")
    p.add_argument("--include-opt-apps", action="store_true")
    p.add_argument(
        "--include-pkg-managers",
        action="store_true",
        help="Also run uv/pip/npm cache prune (can hang; off by default)",
    )
    p.add_argument("--cmd-timeout", type=int, default=120, help="Subprocess timeout seconds")
    p.add_argument("--docker-timeout", type=int, default=600, help="Docker prune timeout seconds")
    p.add_argument("--confirm-above", type=parse_size, default="5G")
    p.add_argument("--cross-fs", action="store_true")
    p.add_argument("--home", type=Path, default=None, help=argparse.SUPPRESS)
    p.add_argument("--report-dir", type=Path, default=None)

    g = p.add_mutually_exclusive_group()
    g.add_argument("--skip-docker", action="store_true")
    g.add_argument("--only-docker", action="store_true")

    g2 = p.add_mutually_exclusive_group()
    g2.add_argument("--skip-caches", action="store_true")
    g2.add_argument("--only-caches", action="store_true")

    g3 = p.add_mutually_exclusive_group()
    g3.add_argument("--skip-files", action="store_true")
    g3.add_argument("--only-files", action="store_true")

    g4 = p.add_mutually_exclusive_group()
    g4.add_argument("--skip-apps", action="store_true")
    g4.add_argument("--only-apps", action="store_true")

    g5 = p.add_mutually_exclusive_group()
    g5.add_argument("--skip-dupes", action="store_true")
    g5.add_argument("--only-dupes", action="store_true")

    return p


def config_from_args(args: argparse.Namespace) -> Config:
    only = None
    for name, flag in (
        ("docker", args.only_docker),
        ("caches", args.only_caches),
        ("files", args.only_files),
        ("apps", args.only_apps),
        ("dupes", args.only_dupes),
    ):
        if flag:
            only = name
            break

    return Config(
        apply=args.apply,
        yes=args.yes,
        docker_unused_days=args.docker_unused_days,
        file_unused_days=args.file_unused_days,
        app_unused_days=args.app_unused_days,
        min_file_size=args.min_file_size,
        min_dupe_size=args.min_dupe_size,
        dedupe_keep=args.dedupe_keep,
        extra_roots=[Path(x) for x in args.extra_roots],
        dedupe_roots=[Path(x) for x in args.dedupe_roots],
        protect_globs=list(args.protect_globs),
        protect_apps=list(args.protect_apps),
        skip_docker=args.skip_docker,
        skip_caches=args.skip_caches,
        skip_files=args.skip_files,
        skip_apps=args.skip_apps,
        skip_dupes=args.skip_dupes,
        only=only,
        journal_vacuum=args.journal_vacuum,
        include_docker_volumes=args.include_docker_volumes,
        include_hf_cache=args.include_hf_cache,
        include_opt_apps=args.include_opt_apps,
        include_pkg_managers=args.include_pkg_managers,
        cmd_timeout_sec=args.cmd_timeout,
        docker_timeout_sec=args.docker_timeout,
        confirm_above=args.confirm_above,
        cross_fs=args.cross_fs,
        home=args.home or Path.home(),
        report_dir=args.report_dir,
    )


def run(cfg: Config) -> int:
    before = df_snapshot(cfg) if cfg.apply else ""

    phase_fns: list[tuple[str, Callable[[Config], PhaseResult]]] = [
        ("docker", phase_docker),
        ("caches", phase_caches),
        ("files", phase_files),
        ("apps", phase_apps),
        ("dupes", phase_dupes),
    ]

    results: list[PhaseResult] = []
    all_actions: list[Action] = []
    for name, fn in phase_fns:
        if not should_run_phase(name, cfg):
            continue
        print(f"… phase: {name}", flush=True)
        r = fn(cfg)
        results.append(r)
        all_actions.extend(r.actions)

    total = print_summary(results, cfg)

    if cfg.apply and not confirm_or_abort(total, cfg):
        return 1

    log = execute_actions(all_actions, cfg)
    if cfg.apply:
        print_apply_results(log)
    report_path = write_report(cfg, results, log)
    print(f"Report: {report_path}")

    if cfg.apply:
        after = df_snapshot(cfg)
        if before:
            print("--- df before ---\n" + before)
        if after:
            print("--- df after ---\n" + after)

    return 0


def main(argv: Optional[Sequence[str]] = None) -> int:
    parser = build_parser()
    args = parser.parse_args(list(argv) if argv is not None else None)
    cfg = config_from_args(args)
    return run(cfg)


if __name__ == "__main__":
    sys.exit(main())
