"""CLI smoke against the real script entrypoint."""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import host_cleaner as hc

SCRIPT = Path(__file__).resolve().parents[1] / "host_cleaner.py"


def test_help_and_version():
    help_cp = subprocess.run([sys.executable, str(SCRIPT), "--help"], capture_output=True, text=True)
    assert help_cp.returncode == 0
    assert "--include-pkg-managers" in help_cp.stdout
    assert "--cmd-timeout" in help_cp.stdout
    ver = subprocess.run([sys.executable, str(SCRIPT), "--version"], capture_output=True, text=True)
    assert ver.returncode == 0
    assert "1.1.0" in ver.stdout


def test_parse_helpers():
    assert hc.parse_size("1M") == 1024**2
    assert hc.parse_reclaimed_bytes("Total:\t12.5GB") == hc.parse_size("12.5G")
    assert hc.docker_until_filter(14) == "until=336h"
