use crate::action::{Action, PhaseResult};
use crate::cmd::{run_command, which_bin};
use crate::config::Config;
use crate::docker_inventory::{
    builder_prune_command, list_build_cache, list_images, stale_images, summarize_build_cache,
};
use crate::docker_track::{store_path, UsageStore};
use crate::util::{docker_until_filter, parse_docker_system_df};

pub fn phase_docker(cfg: &Config) -> PhaseResult {
    let mut result = PhaseResult::new("docker");
    if which_bin("docker").is_none() {
        result.notes.push("docker not found; skipped".into());
        return result;
    }

    let until = docker_until_filter(cfg.docker_unused_days);

    let mut df = std::collections::HashMap::new();
    let cp = run_command(
        &[
            "docker".into(),
            "system".into(),
            "df".into(),
            "--format".into(),
            "{{.Type}}\t{{.Reclaimable}}".into(),
        ],
        cfg,
    );
    if cp.status == 0 {
        df = parse_docker_system_df(&cp.stdout);
        for line in cp.stdout.lines() {
            result.notes.push(format!("df: {}", line.trim()));
        }
    }

    let containers_est = df.get("Containers").copied().unwrap_or(0);
    if containers_est > 0 {
        result.add(
            Action::new(
                "docker",
                "docker_cmd",
                "container_prune",
                containers_est,
                &until,
            )
            .with_command(vec![
                "docker".into(),
                "container".into(),
                "prune".into(),
                "-f".into(),
                "--filter".into(),
                until.clone(),
            ]),
        );
    } else {
        result
            .notes
            .push("no reclaimable containers (skipped container_prune)".into());
    }

    let now = chrono::Utc::now().timestamp();
    let store = UsageStore::load(&store_path());
    match list_images(cfg, &store, now) {
        Ok(images) => {
            let stale = stale_images(&images, cfg.docker_unused_days, now);
            if stale.is_empty() {
                result
                    .notes
                    .push("no unused images older than threshold".into());
            } else {
                let est = stale.iter().map(|i| i.bytes).sum();
                let mut cmd = vec!["docker".into(), "rmi".into(), "-f".into()];
                cmd.extend(stale.iter().map(|i| i.id.clone()));
                result.add(
                    Action::new(
                        "docker",
                        "docker_cmd",
                        "image_rmi",
                        est,
                        format!(
                            "no containers, last used >{}d ago ({} images)",
                            cfg.docker_unused_days,
                            stale.len()
                        ),
                    )
                    .with_command(cmd),
                );
            }
        }
        Err(e) => result.notes.push(format!("image listing failed: {e}")),
    }

    // BuildKit tracks last use per cache record; `until=` prunes by it.
    let (cache_est, cache_detail) = match list_build_cache(cfg, now) {
        Some(records) => {
            let sum = summarize_build_cache(&records, cfg.docker_unused_days, now);
            (
                sum.stale_bytes,
                format!(
                    "build cache unused >{}d ({} records)",
                    cfg.docker_unused_days, sum.stale_records
                ),
            )
        }
        // No buildx JSON: the df figure is an upper bound for the filtered prune.
        None => (
            df.get("Build Cache").copied().unwrap_or(0),
            format!(
                "build cache unused >{}d (size is an upper bound)",
                cfg.docker_unused_days
            ),
        ),
    };
    if cache_est > 0 {
        result.add(
            Action::new("docker", "docker_cmd", "builder_prune", cache_est, cache_detail)
                .with_command(builder_prune_command(cfg.docker_unused_days)),
        );
    } else {
        result
            .notes
            .push("no build cache older than threshold (skipped builder_prune)".into());
    }

    let ncp = run_command(
        &[
            "docker".into(),
            "network".into(),
            "ls".into(),
            "-q".into(),
            "--filter".into(),
            "dangling=true".into(),
        ],
        cfg,
    );
    let dangling_networks = ncp.status == 0
        && ncp
            .stdout
            .split_whitespace()
            .any(|id| !id.is_empty());
    if dangling_networks {
        result.add(
            Action::new("docker", "docker_cmd", "network_prune", 0, "unused networks")
                .with_command(vec![
                    "docker".into(),
                    "network".into(),
                    "prune".into(),
                    "-f".into(),
                ]),
        );
    } else {
        result
            .notes
            .push("no dangling networks (skipped network_prune)".into());
    }

    if cfg.include_docker_volumes {
        // Docker 23+: `volume prune` without -a only removes anonymous volumes.
        // Named unused volumes (most of Local Volumes "reclaimable") need --all.
        let vcp = run_command(
            &["docker".into(), "system".into(), "df".into(), "-v".into()],
            cfg,
        );
        let (vol_bytes, vol_count) = if vcp.status == 0 {
            crate::util::unused_volume_bytes_from_df_v(&vcp.stdout)
        } else {
            (0, 0)
        };
        if vol_count > 0 && vol_bytes > 0 {
            result.add(
                Action::new(
                    "docker",
                    "docker_cmd",
                    "volume_prune",
                    vol_bytes,
                    format!("unused volumes · {vol_count} volumes (named+anonymous)"),
                )
                .with_command(vec![
                    "docker".into(),
                    "volume".into(),
                    "prune".into(),
                    "-af".into(),
                ]),
            );
        } else if vol_count > 0 {
            result.notes.push(format!(
                "{vol_count} unused volumes but size 0 (skipped volume_prune)"
            ));
        } else {
            result
                .notes
                .push("no unused volumes (skipped volume_prune)".into());
        }
    }

    result
}
