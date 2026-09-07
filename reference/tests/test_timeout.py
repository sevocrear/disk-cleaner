"""Subprocess timeout must abort hanging commands (the freeze bug)."""

from __future__ import annotations

import sys
import time
from pathlib import Path

import host_cleaner as hc
from conftest import base_cfg


def test_run_command_times_out_hanging_process(isolated_home, tmp_path):
    cfg = base_cfg(isolated_home, tmp_path, cmd_timeout_sec=1, docker_timeout_sec=1)
    t0 = time.monotonic()
    cp = hc.run_command([sys.executable, "-c", "import time; time.sleep(30)"], cfg, timeout=1)
    elapsed = time.monotonic() - t0
    assert cp.returncode == 124
    assert elapsed < 5
    assert "timed out" in (cp.stderr or "")


def test_execute_actions_continues_after_timeout(isolated_home, tmp_path):
    """A hanging cache_cmd must not block later file deletes."""
    victim = isolated_home / "kill_me.bin"
    victim.write_bytes(b"x" * 200)

    cfg = base_cfg(isolated_home, tmp_path, apply=True, cmd_timeout_sec=1)
    actions = [
        hc.Action(
            phase="caches",
            kind="cache_cmd",
            path="hang",
            command=[sys.executable, "-c", "import time; time.sleep(60)"],
        ),
        hc.Action(
            phase="files",
            kind="delete_file",
            path=str(victim),
            bytes=200,
        ),
    ]
    t0 = time.monotonic()
    log = hc.execute_actions(actions, cfg)
    elapsed = time.monotonic() - t0
    assert elapsed < 8
    assert log[0]["error"]
    assert "timeout" in log[0]["error"] or log[0].get("applied") is False
    assert log[1]["applied"] is True
    assert not victim.exists()
