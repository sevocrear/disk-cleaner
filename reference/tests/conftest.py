"""Shared helpers — isolated fake $HOME trees only."""

from __future__ import annotations

import os
import sys
import time
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

import host_cleaner as hc  # noqa: E402


@pytest.fixture
def isolated_home(tmp_path: Path) -> Path:
    home = tmp_path / "home"
    (home / ".cache").mkdir(parents=True)
    (home / ".local" / "share" / "Trash" / "files").mkdir(parents=True)
    (home / "Applications").mkdir()
    (home / "Downloads").mkdir()
    (home / "snap").mkdir()
    return home


def write_aged(path: Path, data: bytes, *, days: float) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    ts = time.time() - days * 86400
    os.utime(path, (ts, ts))
    return path


def base_cfg(home: Path, tmp_path: Path, **kwargs) -> hc.Config:
    defaults = dict(
        home=home,
        report_dir=tmp_path / "reports",
        skip_docker=True,
        skip_apps=True,
        yes=True,
        confirm_above=10**18,
        min_file_size=100,
        min_dupe_size=100,
        file_unused_days=60,
        app_unused_days=60,
        cmd_timeout_sec=5,
        docker_timeout_sec=5,
        include_pkg_managers=False,
    )
    defaults.update(kwargs)
    return hc.Config(**defaults)
