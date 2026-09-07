//! Integration tests with PATH stubs for docker / pkg-manager gating.

use disk_cleaner_engine::config::Config;
use disk_cleaner_engine::phases::{phase_caches, phase_docker};
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
    cfg.include_pkg_managers = true;
    cfg
}

#[test]
fn docker_omits_zero_reclaimable_prunes() {
    let tmp = tempfile::tempdir().unwrap();
    let bindir = tmp.path().join("bin");
    fs::create_dir_all(&bindir).unwrap();
    let log = tmp.path().join("docker.log");
    fs::write(&log, "").unwrap();

    write_exec(
        &bindir.join("docker"),
        &format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
echo "ARGS:$*" >> "{log}"
case "$*" in
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
            log = log.display()
        ),
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
fn docker_includes_measured_reclaimable() {
    let tmp = tempfile::tempdir().unwrap();
    let bindir = tmp.path().join("bin");
    fs::create_dir_all(&bindir).unwrap();

    write_exec(
        &bindir.join("docker"),
        r#"#!/usr/bin/env bash
set -euo pipefail
case "$*" in
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

    let container = result
        .actions
        .iter()
        .find(|a| a.path == "container_prune")
        .unwrap();
    assert!(container.bytes > 0);
    let network = result
        .actions
        .iter()
        .find(|a| a.path == "network_prune")
        .unwrap();
    assert_eq!(network.bytes, 0);
}

#[test]
fn pkg_cache_omits_empty_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let bindir = tmp.path().join("bin");
    let home = tmp.path().join("home");
    fs::create_dir_all(&bindir).unwrap();
    fs::create_dir_all(home.join(".cache")).unwrap();

    for name in ["uv", "pip", "npm"] {
        write_exec(
            &bindir.join(name),
            r#"#!/usr/bin/env bash
case "$*" in
  *"cache dir"*|*"config get cache"*)
    echo "/tmp/disk-cleaner-empty-cache-does-not-exist-$$"
    ;;
  *)
    exit 0
    ;;
esac
"#,
        );
    }

    let mut cfg = base_cfg(&home);
    cfg.include_pkg_managers = true;
    cfg.skip_caches = false;

    let result = with_path_prefix(&bindir, || phase_caches(&cfg));
    assert!(
        !result.actions.iter().any(|a| a.kind == "cache_cmd"),
        "unexpected cache cmds: {:?}",
        result.actions
    );
}

#[test]
fn pkg_cache_includes_nonempty_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let bindir = tmp.path().join("bin");
    let home = tmp.path().join("home");
    let pip_cache = tmp.path().join("pip-cache");
    fs::create_dir_all(&bindir).unwrap();
    fs::create_dir_all(home.join(".cache")).unwrap();
    fs::create_dir_all(&pip_cache).unwrap();
    fs::write(pip_cache.join("wheel.bin"), vec![0u8; 4096]).unwrap();

    write_exec(
        &bindir.join("pip"),
        &format!(
            r#"#!/usr/bin/env bash
case "$*" in
  *"cache dir"*)
    echo "{dir}"
    ;;
  *)
    exit 0
    ;;
esac
"#,
            dir = pip_cache.display()
        ),
    );

    let mut cfg = base_cfg(&home);
    cfg.include_pkg_managers = true;

    let result = with_path_prefix(&bindir, || phase_caches(&cfg));
    let pip = result
        .actions
        .iter()
        .find(|a| a.path == "pip_cache_purge")
        .expect("pip_cache_purge");
    assert!(pip.bytes >= 4096);
}
