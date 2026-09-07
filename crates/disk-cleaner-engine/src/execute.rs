use crate::action::{Action, ApplyRecord, ApplySummary};
use crate::cmd::run_command;
use crate::config::Config;
use crate::trash::move_to_trash;
use crate::util::parse_reclaimed_bytes;
use std::collections::HashMap;
use std::path::PathBuf;

pub fn execute_actions(actions: &[Action], cfg: &Config, use_trash: bool) -> (Vec<ApplyRecord>, ApplySummary) {
    let mut log = Vec::new();
    let mut summary = ApplySummary::default();
    let mut by_phase: HashMap<String, u64> = HashMap::new();

    for action in actions {
        let mut rec = ApplyRecord {
            action: action.clone(),
            applied: false,
            error: None,
            reclaimed_bytes: None,
            stdout: None,
            stderr: None,
        };
        if !cfg.apply {
            log.push(rec);
            continue;
        }
        match apply_one(action, cfg, use_trash) {
            Ok(mut r) => {
                if r.applied {
                    summary.files_deleted += 1;
                    let bytes = r.reclaimed_bytes.unwrap_or(action.bytes);
                    summary.bytes_reclaimed = summary.bytes_reclaimed.saturating_add(bytes);
                    *by_phase.entry(action.phase.clone()).or_default() += bytes;
                } else if r.error.is_some() {
                    summary.failures += 1;
                }
                std::mem::swap(&mut rec, &mut r);
            }
            Err(e) => {
                rec.error = Some(e);
                summary.failures += 1;
            }
        }
        log.push(rec);
    }
    summary.by_phase = by_phase.into_iter().collect();
    summary.by_phase.sort_by(|a, b| b.1.cmp(&a.1));
    (log, summary)
}

fn apply_one(action: &Action, cfg: &Config, use_trash: bool) -> Result<ApplyRecord, String> {
    let mut rec = ApplyRecord {
        action: action.clone(),
        applied: false,
        error: None,
        reclaimed_bytes: None,
        stdout: None,
        stderr: None,
    };
    match action.kind.as_str() {
        "delete_file" => {
            let p = PathBuf::from(&action.path);
            if p.is_file() || p.is_symlink() {
                if use_trash {
                    move_to_trash(&cfg.home, &p).map_err(|e| e.to_string())?;
                } else {
                    std::fs::remove_file(&p).or_else(|_| {
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            let mut perms = std::fs::metadata(&p)?.permissions();
                            perms.set_mode(0o600);
                            std::fs::set_permissions(&p, perms)?;
                        }
                        std::fs::remove_file(&p)
                    }).map_err(|e| e.to_string())?;
                }
                rec.applied = true;
                rec.reclaimed_bytes = Some(action.bytes);
            }
        }
        "delete_tree" => {
            let p = PathBuf::from(&action.path);
            if p.is_dir() {
                if use_trash {
                    move_to_trash(&cfg.home, &p).map_err(|e| e.to_string())?;
                } else {
                    std::fs::remove_dir_all(&p).map_err(|e| e.to_string())?;
                }
                rec.applied = true;
                rec.reclaimed_bytes = Some(action.bytes);
            }
        }
        "rmdir_if_empty" => {
            let p = PathBuf::from(&action.path);
            if p.is_dir() && std::fs::read_dir(&p).map(|mut i| i.next().is_none()).unwrap_or(false) {
                std::fs::remove_dir(&p).map_err(|e| e.to_string())?;
                rec.applied = true;
            }
        }
        "docker_cmd" | "cache_cmd" | "snap_remove" | "flatpak_uninstall" => {
            if let Some(ref cmd) = action.command {
                let cp = run_command(cmd, cfg);
                rec.applied = cp.status == 0;
                rec.stdout = Some(cp.stdout.chars().rev().take(2000).collect::<String>().chars().rev().collect());
                rec.stderr = Some(cp.stderr.chars().rev().take(2000).collect::<String>().chars().rev().collect());
                let combined = format!("{}\n{}", cp.stdout, cp.stderr);
                rec.reclaimed_bytes = parse_reclaimed_bytes(&combined);
                if rec.reclaimed_bytes.is_none() && rec.applied && action.bytes > 0 {
                    if action.path == "image_rmi" || action.path == "builder_prune" {
                        if action.path == "image_rmi" || cp.stdout.contains("Total:") {
                            rec.reclaimed_bytes = Some(action.bytes);
                        }
                    }
                }
                if cp.status != 0 {
                    rec.error = Some(if cp.status == 124 {
                        format!("timeout after {}s", cfg.cmd_timeout_sec)
                    } else {
                        format!("exit {}", cp.status)
                    });
                }
            }
        }
        "report" => {
            rec.applied = true;
        }
        other => {
            rec.error = Some(format!("unknown kind {other}"));
        }
    }
    Ok(rec)
}
