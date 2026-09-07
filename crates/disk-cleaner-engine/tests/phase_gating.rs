//! Integration tests with PATH stubs for docker gating.

use disk_cleaner_engine::config::Config;
use disk_cleaner_engine::phases::phase_docker;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Mutex;

static PATH_LOCK: Mutex<()> = Mutex::new(());

fn write_exec(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

fn with_path_prefix<T>(bindir: &Path, f: impl FnOnce() -> T) -> T {
    let _guard = PATH_LOCK.lock().unwrap();
    let old = std::env::var_os("PATH");
    let mut new_path = bindir.display().to_string();
    if let Some(ref o) = old {
        new_path.push(':');
        new_path.push_str(&o.to_string_lossy());
    }
    std::env::set_var("PATH", &new_path);
    let out = f();
    match old {
        Some(o) => std::env::set_var("PATH", o),
        None => std::env::remove_var("PATH"),
    }
    out
}

fn base_cfg(home: &Path) -> Config {
    let mut cfg = Config::default();
    cfg.home = home.to_path_buf();
    cfg.cmd_timeout_sec = 5;
    cfg.docker_timeout_sec = 5;
    cfg.skip_docker = false;
    cfg.include_docker_volumes = true;
    cfg
}

#[test]
fn docker_omits_zero_reclaimable_prunes() {
    let tmp = tempfile::tempdir().unwrap();
    let bindir = tmp.path().join("bin");
    fs::create_dir_all(&bindir).unwrap();

    write_exec(
        &bindir.join("docker"),
        r#"#!/usr/bin/env bash
set -euo pipefail
case "$*" in
  *"system df -v"*|*"system df"*" -v"*)
    echo "Local Volumes space usage:"
    echo ""
    echo "VOLUME NAME   LINKS     SIZE"
    ;;
  *"system df"*)
    echo -e "Containers\t0B (0%)"
    echo -e "Images\t0B (0%)"
    echo -e "Local Volumes\t0B (0%)"
    echo -e "Build Cache\t0B (0%)"
    ;;
  *"builder du"*)
    echo -e "Reclaimable:\t0B"
    ;;
  *"network ls"*)
    ;;
  *"ps -a --format"*)
    ;;
  *"ps -aq"*)
    ;;
  *"images --format"*)
    ;;
  *)
    echo "unexpected: $*" >&2
    exit 1
    ;;
esac
"#,
    );

    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let cfg = base_cfg(&home);

    let result = with_path_prefix(&bindir, || phase_docker(&cfg));
    assert!(
        result.actions.is_empty(),
        "expected no actions, got {:?}",
        result.actions.iter().map(|a| &a.path).collect::<Vec<_>>()
    );
}

#[test]
fn docker_volume_prune_uses_all_and_real_sizes() {
    let tmp = tempfile::tempdir().unwrap();
    let bindir = tmp.path().join("bin");
    fs::create_dir_all(&bindir).unwrap();

    write_exec(
        &bindir.join("docker"),
        r#"#!/usr/bin/env bash
set -euo pipefail
case "$*" in
  *"system df -v"*|*"df -v"*)
    echo "Local Volumes space usage:"
    echo ""
    echo "VOLUME NAME   LINKS     SIZE"
    echo "keep-linked   1         5.0GB"
    echo "unused-big    0         9.678GB"
    echo "unused-small  0         512MB"
    ;;
  *"system df"*)
    echo -e "Containers\t0B (0%)"
    echo -e "Images\t0B (0%)"
    echo -e "Local Volumes\t14.94GB (97%)"
    echo -e "Build Cache\t0B (0%)"
    ;;
  *"network ls"*)
    ;;
  *"ps -a --format"*)
    ;;
  *"ps -aq"*)
    ;;
  *"images --format"*)
    ;;
  *)
    echo "unexpected: $*" >&2
    exit 1
    ;;
esac
"#,
    );

    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let cfg = base_cfg(&home);

    let result = with_path_prefix(&bindir, || phase_docker(&cfg));
    let vol = result
        .actions
        .iter()
        .find(|a| a.path == "volume_prune")
        .expect("volume_prune");
    assert!(
        vol.command
            .as_ref()
            .map(|c| c.iter().any(|x| x == "-af"))
            .unwrap_or(false),
        "expected prune -af, got {:?}",
        vol.command
    );
    assert!(vol.bytes > 9_000_000_000);
    // Must not use the misleading aggregate Local Volumes reclaimable alone.
    assert!(vol.bytes < 14 * 1024 * 1024 * 1024);
}

#[test]
fn docker_includes_measured_reclaimable() {
    let tmp = tempfile::tempdir().unwrap();
    let bindir = tmp.path().join("bin");
    fs::create_dir_all(&bindir).unwrap();

    write_exec(
        &bindir.join("docker"),
        r#"#!/usr/bin/env bash
set -euo pipefail
case "$*" in
  *"system df -v"*|*"df -v"*)
    echo "Local Volumes space usage:"
    echo ""
    echo "VOLUME NAME   LINKS     SIZE"
    echo "v1            0         2.0GB"
    ;;
  *"system df"*)
    echo -e "Containers\t100MB (10%)"
    echo -e "Images\t0B (0%)"
    echo -e "Local Volumes\t2.0GB (100%)"
    echo -e "Build Cache\t50MB (5%)"
    ;;
  *"network ls"*)
    echo deadbeef
    ;;
  *"ps -a --format"*)
    ;;
  *"ps -aq"*)
    ;;
  *"images --format"*)
    ;;
  *)
    exit 1
    ;;
esac
"#,
    );

    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let cfg = base_cfg(&home);

    let result = with_path_prefix(&bindir, || phase_docker(&cfg));
    let paths: Vec<_> = result.actions.iter().map(|a| a.path.as_str()).collect();
    assert!(paths.contains(&"container_prune"));
    assert!(paths.contains(&"builder_prune"));
    assert!(paths.contains(&"network_prune"));
    assert!(paths.contains(&"volume_prune"));
    assert!(!paths.contains(&"image_rmi"));
}
