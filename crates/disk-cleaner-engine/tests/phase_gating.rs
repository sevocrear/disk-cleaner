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
    // Keep the usage-tracker store hermetic (it lives under XDG_DATA_HOME).
    let old_data = std::env::var_os("XDG_DATA_HOME");
    std::env::set_var("XDG_DATA_HOME", bindir.join("data"));
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
    match old_data {
        Some(o) => std::env::set_var("XDG_DATA_HOME", o),
        None => std::env::remove_var("XDG_DATA_HOME"),
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
  *"network ls"*)
    ;;
  *"image ls -q"*)
    ;;
  *"ps -aq"*)
    ;;
  *"buildx du"*)
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
  *"image ls -q"*)
    ;;
  *"ps -aq"*)
    ;;
  *"buildx du"*)
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
  *"image ls -q"*)
    ;;
  *"ps -aq"*)
    ;;
  *"buildx du"*)
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
    // No buildx records reported → nothing older than the threshold.
    assert!(!paths.contains(&"builder_prune"));
    assert!(paths.contains(&"network_prune"));
    assert!(paths.contains(&"volume_prune"));
    assert!(!paths.contains(&"image_rmi"));
}

#[test]
fn docker_images_by_last_use_and_cache_by_age() {
    let tmp = tempfile::tempdir().unwrap();
    let bindir = tmp.path().join("bin");
    fs::create_dir_all(&bindir).unwrap();
    let now = chrono::Utc::now();
    let ago = |days: i64| (now - chrono::Duration::days(days)).to_rfc3339();
    let images = format!(
        r#"[{{"Id":"sha256:stale","RepoTags":["old:1"],"Created":"{old}","Size":3000,"Metadata":{{"LastTagTime":"{old}"}}}},
{{"Id":"sha256:pulled","RepoTags":["clickhouse:23"],"Created":"{ancient}","Size":5000,"Metadata":{{"LastTagTime":"{fresh}"}}}},
{{"Id":"sha256:busy","RepoTags":["svc:1"],"Created":"{ancient}","Size":7000,"Metadata":{{"LastTagTime":"0001-01-01T00:00:00Z"}}}}]"#,
        old = ago(90),
        ancient = ago(700),
        fresh = ago(1),
    );
    let containers = format!(
        r#"[{{"Id":"c1","Name":"/svc","Image":"sha256:busy","Created":"{t}","Config":{{"Image":"svc:1"}},"State":{{"Running":false,"StartedAt":"{t}","FinishedAt":"{t}"}}}}]"#,
        t = ago(200)
    );
    fs::write(bindir.join("images.json"), images).unwrap();
    fs::write(bindir.join("containers.json"), containers).unwrap();
    fs::write(
        bindir.join("du.jsonl"),
        "{\"ID\":\"a\",\"Size\":\"2GB\",\"CreatedAt\":\"2026-01-01 00:00:00.0 +0000 UTC\",\"LastUsedAt\":\"2 months ago\",\"Reclaimable\":true}\n\
         {\"ID\":\"b\",\"Size\":\"1GB\",\"CreatedAt\":\"2026-01-01 00:00:00.0 +0000 UTC\",\"LastUsedAt\":\"2 hours ago\",\"Reclaimable\":true}\n",
    )
    .unwrap();
    write_exec(
        &bindir.join("docker"),
        &format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
D="{dir}"
case "$*" in
  *"system df"*)
    echo -e "Containers\t0B (0%)"
    echo -e "Images\t15GB (100%)"
    echo -e "Local Volumes\t0B (0%)"
    echo -e "Build Cache\t3GB (100%)"
    ;;
  "image ls -q --no-trunc") printf 'sha256:stale\nsha256:pulled\nsha256:busy\nsha256:stale\n' ;;
  "image inspect "*) cat "$D/images.json" ;;
  "ps -aq --no-trunc") echo c1 ;;
  "inspect c1") cat "$D/containers.json" ;;
  "buildx du --format json") cat "$D/du.jsonl" ;;
  *"network ls"*) ;;
  *) echo "unexpected: $*" >&2; exit 1 ;;
esac
"#,
            dir = bindir.display()
        ),
    );

    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let mut cfg = base_cfg(&home);
    cfg.include_docker_volumes = false;
    cfg.docker_unused_days = 30;

    let result = with_path_prefix(&bindir, || phase_docker(&cfg));
    let rmi = result
        .actions
        .iter()
        .find(|a| a.path == "image_rmi")
        .unwrap_or_else(|| panic!("no image_rmi; notes={:?}", result.notes));
    // Only the image unused for 90 days: the pulled one is fresh, the busy one has a container.
    assert_eq!(
        rmi.command.as_ref().unwrap(),
        &vec!["docker", "rmi", "-f", "sha256:stale"]
    );
    assert_eq!(rmi.bytes, 3000);

    let prune = result
        .actions
        .iter()
        .find(|a| a.path == "builder_prune")
        .expect("builder_prune");
    assert_eq!(
        prune.command.as_ref().unwrap(),
        &vec!["docker", "builder", "prune", "-af", "--filter", "until=720h"]
    );
    assert_eq!(prune.bytes, 2 * 1024 * 1024 * 1024, "only the 2-month-old record");
}
