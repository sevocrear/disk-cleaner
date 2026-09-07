"""Docker phase via a real stub binary on PATH (not mocked run_cmd)."""

from __future__ import annotations

import os
import stat
import textwrap
from pathlib import Path

import host_cleaner as hc
from conftest import base_cfg


def _install_docker_stub(bindir: Path) -> Path:
    bindir.mkdir(parents=True, exist_ok=True)
    docker = bindir / "docker"
    docker.write_text(
        textwrap.dedent(
            """\
            #!/usr/bin/env bash
            set -euo pipefail
            echo "ARGS:$*" >> "${DOCKER_STUB_LOG}"
            case "$*" in
              *"system df"*)
                echo -e "Images\\t1GB"
                ;;
              *"builder du"*)
                echo -e "Reclaimable:\\t2.5GB"
                echo -e "Total:\\t2.5GB"
                ;;
              *"ps -a --format"*)
                echo -e "redis:7-alpine\\tc1"
                ;;
              *"ps -aq"*)
                echo c1
                ;;
              *"inspect"*)
                echo sha256:aaaaaaaaaaaa
                ;;
              *"images --format"*)
                # unused old image + in-use redis
                echo -e "bbbbbbbbbbbb\\told:latest\\t2024-01-01 12:00:00 +0000 UTC\\t1.0GB"
                echo -e "aaaaaaaaaaaa\\tredis:7-alpine\\t2024-01-01 12:00:00 +0000 UTC\\t50MB"
                ;;
              *"builder prune"*)
                echo -e "Total:\\t2.5GB"
                ;;
              *"container prune"*|*"network prune"*|*"volume prune"*|*"rmi"*)
                echo "Total reclaimed space: 0B"
                ;;
              *)
                echo "unexpected: $*" >&2
                exit 1
                ;;
            esac
            """
        ),
        encoding="utf-8",
    )
    docker.chmod(docker.stat().st_mode | stat.S_IEXEC)
    return docker


def test_docker_phase_calls_builder_prune_af_and_rmi_old(isolated_home, tmp_path, monkeypatch):
    bindir = tmp_path / "bin"
    log = tmp_path / "docker.log"
    log.write_text("")
    _install_docker_stub(bindir)
    monkeypatch.setenv("PATH", f"{bindir}:{os.environ.get('PATH', '')}")
    monkeypatch.setenv("DOCKER_STUB_LOG", str(log))

    cfg = base_cfg(
        isolated_home,
        tmp_path,
        skip_docker=False,
        only="docker",
        apply=True,
        docker_unused_days=14,
        which=None,  # use real PATH
        run_cmd=None,
    )
    # Clear test hooks so real subprocess + PATH stub is used
    cfg.which = None
    cfg.run_cmd = None

    result = hc.phase_docker(cfg)
    cmds = [" ".join(a.command) for a in result.actions if a.command]
    assert any("builder prune -af" in c or c.endswith("prune -af") for c in cmds)
    assert any(c.startswith("docker rmi") and "bbbbbbbbbbbb" in c for c in cmds)
    assert all("aaaaaaaaaaaa" not in c or not c.startswith("docker rmi") for c in cmds)

    # Execute for real against the stub
    log_recs = hc.execute_actions(result.actions, cfg)
    assert all(r.get("applied") or r.get("kind") == "rmdir_if_empty" for r in log_recs)
    stub_log = log.read_text()
    assert "builder prune -af" in stub_log or "builder prune -a -f" in stub_log.replace(" -af", " -a -f")
    # bash joins as "builder prune -af"
    assert "prune -af" in stub_log or "prune -a" in stub_log
    assert "rmi -f bbbbbbbbbbbb" in stub_log


def test_pkg_manager_cmds_off_by_default(isolated_home, tmp_path, monkeypatch):
    bindir = tmp_path / "bin"
    bindir.mkdir()
    for name in ("uv", "pip", "npm", "journalctl"):
        p = bindir / name
        p.write_text("#!/bin/sh\necho ran-$0 >> \"$STUB_LOG\"\n")
        p.chmod(p.stat().st_mode | stat.S_IEXEC)
    monkeypatch.setenv("PATH", f"{bindir}:{os.environ.get('PATH', '')}")
    log = tmp_path / "stub.log"
    log.write_text("")
    monkeypatch.setenv("STUB_LOG", str(log))

    cfg = base_cfg(isolated_home, tmp_path, skip_caches=False, only="caches", include_pkg_managers=False)
    cfg.which = None
    result = hc.phase_caches(cfg)
    assert not any(a.path.startswith("uv_") or a.path.startswith("pip_") for a in result.actions)
