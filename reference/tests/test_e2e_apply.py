"""End-to-end: apply must actually free files on an isolated home."""

from __future__ import annotations

import json
import os
import time
from pathlib import Path

import host_cleaner as hc
from conftest import base_cfg, write_aged


def test_apply_deletes_old_cache_files_dupes_and_appimage(isolated_home, tmp_path):
    home = isolated_home

    # Old cache entry (should go)
    old_cache = write_aged(home / ".cache" / "stale_tool" / "blob.bin", b"C" * 500, days=120)
    # Fresh cache (must stay)
    fresh_cache = home / ".cache" / "active" / "blob.bin"
    fresh_cache.parent.mkdir(parents=True)
    fresh_cache.write_bytes(b"F" * 500)

    # Age-based file root
    purge_root = tmp_path / "purge_root"
    old_file = write_aged(purge_root / "old.bin", b"O" * 500, days=90)
    new_file = purge_root / "new.bin"
    new_file.write_bytes(b"N" * 500)

    # Duplicates — keep newest
    data = b"DUPLICATE-PAYLOAD" * 20
    older = time.time() - 100
    newer = time.time() - 5
    a = home / "Downloads" / "a.bin"
    b = home / "Downloads" / "b.bin"
    a.write_bytes(data)
    b.write_bytes(data)
    os.utime(a, (older, older))
    os.utime(b, (newer, newer))

    # Unused AppImage
    app = write_aged(home / "Applications" / "DeadApp.AppImage", b"A" * 2048, days=100)

    cfg = base_cfg(
        home,
        tmp_path,
        skip_docker=True,
        skip_caches=False,
        skip_files=False,
        skip_apps=False,
        skip_dupes=False,
        file_roots=[purge_root],
        dedupe_roots=[home / "Downloads"],
        apply=True,
        yes=True,
    )

    rc = hc.run(cfg)
    assert rc == 0

    assert not old_cache.exists()
    assert not (home / ".cache" / "stale_tool").exists()
    assert fresh_cache.exists()
    assert not old_file.exists()
    assert new_file.exists()
    assert not a.exists()
    assert b.exists()
    assert not app.exists()

    reports = list((tmp_path / "reports").glob("report_*.json"))
    assert reports
    payload = json.loads(reports[0].read_text())
    assert payload["apply"] is True
    applied = [x for x in payload["actions"] if x.get("applied")]
    assert applied, "expected at least one applied action in report"


def test_dry_run_does_not_delete(isolated_home, tmp_path):
    home = isolated_home
    target = write_aged(home / ".cache" / "x" / "y.bin", b"Z" * 400, days=200)
    cfg = base_cfg(
        home,
        tmp_path,
        skip_caches=False,
        skip_files=True,
        skip_dupes=True,
        skip_apps=True,
        apply=False,
    )
    assert hc.run(cfg) == 0
    assert target.exists()


def test_protect_glob_keeps_huggingface_cache(isolated_home, tmp_path):
    home = isolated_home
    hf = write_aged(home / ".cache" / "huggingface" / "model.bin", b"H" * 400, days=200)
    other = write_aged(home / ".cache" / "misc" / "x.bin", b"M" * 400, days=200)
    cfg = base_cfg(home, tmp_path, skip_caches=False, skip_files=True, skip_dupes=True, apply=True)
    assert hc.run(cfg) == 0
    assert hf.exists()
    assert not other.exists()
