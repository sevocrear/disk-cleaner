use crate::action::{Action, PhaseResult};
use crate::cmd::{run_command, which_bin};
use crate::config::Config;
use crate::util::{days_to_seconds, docker_until_filter, now_ts, parse_size};
use chrono::{DateTime, FixedOffset, NaiveDateTime};

pub fn phase_docker(cfg: &Config) -> PhaseResult {
    let mut result = PhaseResult::new("docker");
    if which_bin("docker").is_none() {
        result.notes.push("docker not found; skipped".into());
        return result;
    }

    let until = docker_until_filter(cfg.docker_unused_days);

    let cp = run_command(
        &["docker".into(), "system".into(), "df".into(), "--format".into(), "{{.Type}}\t{{.Reclaimable}}".into()],
        cfg,
    );
    if cp.status == 0 {
        for line in cp.stdout.lines() {
            result.notes.push(format!("df: {}", line.trim()));
        }
    }

    result.add(
        Action::new("docker", "docker_cmd", "container_prune", 0, &until).with_command(vec![
            "docker".into(),
            "container".into(),
            "prune".into(),
            "-f".into(),
            "--filter".into(),
            until.clone(),
        ]),
    );

    let (img_ids, img_est, img_notes) = collect_unused_old_image_ids(cfg, cfg.docker_unused_days);
    result.notes.extend(img_notes);
    if !img_ids.is_empty() {
        let mut cmd = vec!["docker".into(), "rmi".into(), "-f".into()];
        cmd.extend(img_ids.iter().cloned());
        result.add(
            Action::new(
                "docker",
                "docker_cmd",
                "image_rmi",
                img_est,
                format!(
                    "unused + older than {}d ({} images)",
                    cfg.docker_unused_days,
                    img_ids.len()
                ),
            )
            .with_command(cmd),
        );
    } else {
        result
            .notes
            .push("no unused images older than threshold".into());
    }

    let mut bytes_est = 0u64;
    let bcp = run_command(&["docker".into(), "builder".into(), "du".into()], cfg);
    if bcp.status == 0 {
        for line in bcp.stdout.lines() {
            if let Some(part) = line.split("Reclaimable:").nth(1) {
                let token = part.trim().split_whitespace().next().unwrap_or("");
                if let Ok(v) = parse_size(&token.replace('B', "")) {
                    bytes_est = v;
                }
            }
        }
    }
    result.add(
        Action::new(
            "docker",
            "docker_cmd",
            "builder_prune",
            bytes_est,
            "all unused build cache (buildx until= is a no-op)",
        )
        .with_command(vec![
            "docker".into(),
            "builder".into(),
            "prune".into(),
            "-af".into(),
        ]),
    );

    result.add(
        Action::new("docker", "docker_cmd", "network_prune", 0, "unused networks").with_command(
            vec![
                "docker".into(),
                "network".into(),
                "prune".into(),
                "-f".into(),
            ],
        ),
    );

    if cfg.include_docker_volumes {
        result.add(
            Action::new(
                "docker",
                "docker_cmd",
                "volume_prune",
                0,
                "unused volumes (opt-in)",
            )
            .with_command(vec![
                "docker".into(),
                "volume".into(),
                "prune".into(),
                "-f".into(),
            ]),
        );
    }

    result
}

fn collect_unused_old_image_ids(cfg: &Config, days: u32) -> (Vec<String>, u64, Vec<String>) {
    let mut notes = Vec::new();
    let mut used = std::collections::HashSet::new();

    let cp = run_command(
        &[
            "docker".into(),
            "ps".into(),
            "-a".into(),
            "--format".into(),
            "{{.Image}}\t{{.ID}}".into(),
        ],
        cfg,
    );
    if cp.status == 0 {
        for line in cp.stdout.lines() {
            for p in line.split('\t') {
                if !p.trim().is_empty() {
                    used.insert(p.trim().to_string());
                }
            }
        }
    }

    let cp = run_command(&["docker".into(), "ps".into(), "-aq".into()], cfg);
    if cp.status == 0 && !cp.stdout.trim().is_empty() {
        let cids: Vec<String> = cp.stdout.split_whitespace().map(String::from).collect();
        if !cids.is_empty() {
            let mut cmd = vec![
                "docker".into(),
                "inspect".into(),
                "--format".into(),
                "{{.Image}}".into(),
            ];
            cmd.extend(cids);
            let icp = run_command(&cmd, cfg);
            if icp.status == 0 {
                for line in icp.stdout.lines() {
                    let line = line.trim();
                    used.insert(line.to_string());
                    if let Some(rest) = line.strip_prefix("sha256:") {
                        used.insert(rest.chars().take(12).collect());
                    }
                }
            }
        }
    }

    let cutoff = now_ts() - days_to_seconds(days);
    let mut to_remove = Vec::new();
    let mut est = 0u64;
    let mut seen_ids = std::collections::HashSet::new();

    let cp = run_command(
        &[
            "docker".into(),
            "images".into(),
            "--format".into(),
            "{{.ID}}\t{{.Repository}}:{{.Tag}}\t{{.CreatedAt}}\t{{.Size}}".into(),
        ],
        cfg,
    );
    if cp.status != 0 || cp.stdout.is_empty() {
        notes.push("docker images listing failed".into());
        return (vec![], 0, notes);
    }

    for line in cp.stdout.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 4 {
            continue;
        }
        let (img_id, ref_, created_at, size_s) = (parts[0], parts[1], parts[2], parts[3]);
        if seen_ids.contains(img_id) {
            continue;
        }
        let repo = if ref_ != "<none>:<none>" {
            ref_.rsplit_once(':').map(|(r, _)| r).unwrap_or(ref_)
        } else {
            ""
        };
        let mut skip = used.contains(img_id) || used.contains(ref_) || (!repo.is_empty() && used.contains(repo));
        if !skip {
            for u in &used {
                let u_short: String = u.replace("sha256:", "").chars().take(12).collect();
                if img_id.starts_with(&u_short)
                    || u_short.starts_with(&img_id.chars().take(12).collect::<String>())
                    || u == ref_
                    || (!repo.is_empty() && u == repo)
                {
                    skip = true;
                    break;
                }
            }
        }
        if skip {
            continue;
        }
        let Some(ts) = docker_image_created_ts(created_at) else {
            notes.push(format!("unparsed CreatedAt for {ref_}: {created_at}"));
            continue;
        };
        if ts >= cutoff {
            continue;
        }
        seen_ids.insert(img_id.to_string());
        to_remove.push(img_id.to_string());
        if let Ok(v) = parse_size(&size_s.replace('B', "")) {
            est += v;
        }
    }
    (to_remove, est, notes)
}

fn docker_image_created_ts(created_at: &str) -> Option<f64> {
    let created_at = created_at.trim();
    let formats = [
        "%Y-%m-%d %H:%M:%S %z %Z",
        "%Y-%m-%d %H:%M:%S %z",
        "%Y-%m-%dT%H:%M:%S%z",
    ];
    for fmt in formats {
        if let Ok(dt) = DateTime::parse_from_str(created_at, fmt) {
            return Some(dt.timestamp() as f64);
        }
    }
    if let Some(m) = regex::Regex::new(r"(\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2} [+-]\d{4})")
        .ok()
        .and_then(|re| re.captures(created_at))
    {
        if let Ok(dt) = DateTime::parse_from_str(&m[1], "%Y-%m-%d %H:%M:%S %z") {
            return Some(dt.timestamp() as f64);
        }
    }
    if let Ok(ndt) = NaiveDateTime::parse_from_str(created_at, "%Y-%m-%d %H:%M:%S") {
        return Some(ndt.and_utc().timestamp() as f64);
    }
    let _ = FixedOffset::east_opt(0);
    None
}
