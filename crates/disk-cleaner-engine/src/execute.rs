use crate::action::{Action, ApplyProgress, ApplyRecord, ApplySummary};
use crate::cmd::run_command;
use crate::config::Config;
use crate::trash::move_to_trash;
use crate::util::parse_reclaimed_bytes;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Default worker count for filesystem deletes (bounded).
pub fn default_apply_jobs() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(2, 8)
}

fn is_parallel_kind(kind: &str) -> bool {
    matches!(kind, "delete_file" | "delete_tree")
}

pub fn execute_actions(
    actions: &[Action],
    cfg: &Config,
    use_trash: bool,
) -> (Vec<ApplyRecord>, ApplySummary) {
    execute_actions_parallel(actions, cfg, use_trash, default_apply_jobs(), |_| {})
}

/// Apply actions with a bounded worker pool for filesystem deletes.
///
/// `delete_file` / `delete_tree` run concurrently (up to `jobs` workers).
/// Command actions and `rmdir_if_empty` stay sequential and drain in-flight
/// deletes first so empty-dir cleanup and prune commands stay correct.
pub fn execute_actions_parallel<F>(
    actions: &[Action],
    cfg: &Config,
    use_trash: bool,
    jobs: usize,
    on_progress: F,
) -> (Vec<ApplyRecord>, ApplySummary)
where
    F: Fn(ApplyProgress) + Sync,
{
    let total = actions.len() as u64;
    if total == 0 {
        on_progress(ApplyProgress {
            done: 0,
            total: 0,
            bytes_reclaimed: 0,
            failures: 0,
            path: String::new(),
        });
        return (Vec::new(), ApplySummary::default());
    }

    if !cfg.apply {
        return execute_dry_run(actions, &on_progress);
    }

    let jobs = jobs.max(1);
    let records: Vec<Mutex<Option<ApplyRecord>>> =
        (0..actions.len()).map(|_| Mutex::new(None)).collect();
    let done = AtomicU64::new(0);
    let bytes_reclaimed = AtomicU64::new(0);
    let failures = AtomicU64::new(0);

    let notify = |path: &str| {
        on_progress(ApplyProgress {
            done: done.load(Ordering::Relaxed),
            total,
            bytes_reclaimed: bytes_reclaimed.load(Ordering::Relaxed),
            failures: failures.load(Ordering::Relaxed),
            path: path.to_string(),
        });
    };

    let mut i = 0;
    while i < actions.len() {
        if is_parallel_kind(&actions[i].kind) {
            let start = i;
            while i < actions.len() && is_parallel_kind(&actions[i].kind) {
                i += 1;
            }
            run_parallel_segment(
                &actions[start..i],
                start,
                cfg,
                use_trash,
                jobs,
                &records,
                &done,
                &bytes_reclaimed,
                &failures,
                &notify,
            );
        } else {
            let action = &actions[i];
            let rec = apply_one_record(action, cfg, use_trash);
            account_record(&rec, &done, &bytes_reclaimed, &failures);
            let path = action.path.clone();
            if let Ok(mut slot) = records[i].lock() {
                *slot = Some(rec);
            }
            notify(&path);
            i += 1;
        }
    }

    let log: Vec<ApplyRecord> = records
        .into_iter()
        .enumerate()
        .map(|(idx, slot)| {
            slot.into_inner().ok().flatten().unwrap_or_else(|| ApplyRecord {
                action: actions[idx].clone(),
                applied: false,
                error: Some("missing apply record".into()),
                reclaimed_bytes: None,
                stdout: None,
                stderr: None,
            })
        })
        .collect();

    let summary = summarize(&log);
    (log, summary)
}

fn execute_dry_run<F>(actions: &[Action], on_progress: &F) -> (Vec<ApplyRecord>, ApplySummary)
where
    F: Fn(ApplyProgress) + Sync,
{
    let total = actions.len() as u64;
    let mut log = Vec::with_capacity(actions.len());
    for (i, action) in actions.iter().enumerate() {
        log.push(ApplyRecord {
            action: action.clone(),
            applied: false,
            error: None,
            reclaimed_bytes: None,
            stdout: None,
            stderr: None,
        });
        on_progress(ApplyProgress {
            done: (i + 1) as u64,
            total,
            bytes_reclaimed: 0,
            failures: 0,
            path: action.path.clone(),
        });
    }
    (log, ApplySummary::default())
}

fn run_parallel_segment<N>(
    segment: &[Action],
    base_index: usize,
    cfg: &Config,
    use_trash: bool,
    jobs: usize,
    records: &[Mutex<Option<ApplyRecord>>],
    done: &AtomicU64,
    bytes_reclaimed: &AtomicU64,
    failures: &AtomicU64,
    notify: &N,
) where
    N: Fn(&str) + Sync,
{
    if segment.is_empty() {
        return;
    }
    let next = AtomicUsize::new(0);
    let worker_count = jobs.min(segment.len()).max(1);
    let cfg = Arc::new(cfg.clone());

    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            let cfg = Arc::clone(&cfg);
            let next = &next;
            let records = records;
            let done = done;
            let bytes_reclaimed = bytes_reclaimed;
            let failures = failures;
            let notify = notify;
            scope.spawn(move || {
                loop {
                    let local = next.fetch_add(1, Ordering::Relaxed);
                    if local >= segment.len() {
                        break;
                    }
                    let action = &segment[local];
                    let rec = apply_one_record(action, cfg.as_ref(), use_trash);
                    account_record(&rec, done, bytes_reclaimed, failures);
                    let path = action.path.clone();
                    if let Ok(mut slot) = records[base_index + local].lock() {
                        *slot = Some(rec);
                    }
                    notify(&path);
                }
            });
        }
    });
}

fn account_record(
    rec: &ApplyRecord,
    done: &AtomicU64,
    bytes_reclaimed: &AtomicU64,
    failures: &AtomicU64,
) {
    done.fetch_add(1, Ordering::Relaxed);
    if rec.applied {
        let bytes = rec.reclaimed_bytes.unwrap_or(rec.action.bytes);
        bytes_reclaimed.fetch_add(bytes, Ordering::Relaxed);
    } else if rec.error.is_some() {
        failures.fetch_add(1, Ordering::Relaxed);
    }
}

fn summarize(log: &[ApplyRecord]) -> ApplySummary {
    let mut summary = ApplySummary::default();
    let mut by_phase: HashMap<String, u64> = HashMap::new();
    for rec in log {
        if rec.applied {
            summary.files_deleted += 1;
            let bytes = rec.reclaimed_bytes.unwrap_or(rec.action.bytes);
            summary.bytes_reclaimed = summary.bytes_reclaimed.saturating_add(bytes);
            *by_phase.entry(rec.action.phase.clone()).or_default() += bytes;
        } else if rec.error.is_some() {
            summary.failures += 1;
        }
    }
    summary.by_phase = by_phase.into_iter().collect();
    summary.by_phase.sort_by(|a, b| b.1.cmp(&a.1));
    summary
}

fn apply_one_record(action: &Action, cfg: &Config, use_trash: bool) -> ApplyRecord {
    match apply_one(action, cfg, use_trash) {
        Ok(rec) => rec,
        Err(e) => ApplyRecord {
            action: action.clone(),
            applied: false,
            error: Some(e),
            reclaimed_bytes: None,
            stdout: None,
            stderr: None,
        },
    }
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
                    std::fs::remove_file(&p)
                        .or_else(|_| {
                            #[cfg(unix)]
                            {
                                use std::os::unix::fs::PermissionsExt;
                                let mut perms = std::fs::metadata(&p)?.permissions();
                                perms.set_mode(0o600);
                                std::fs::set_permissions(&p, perms)?;
                            }
                            std::fs::remove_file(&p)
                        })
                        .map_err(|e| e.to_string())?;
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
            if p.is_dir()
                && std::fs::read_dir(&p)
                    .map(|mut i| i.next().is_none())
                    .unwrap_or(false)
            {
                std::fs::remove_dir(&p).map_err(|e| e.to_string())?;
                rec.applied = true;
            }
        }
        "docker_cmd" | "cache_cmd" | "snap_remove" | "flatpak_uninstall" => {
            if let Some(ref cmd) = action.command {
                let cp = run_command(cmd, cfg);
                rec.applied = cp.status == 0;
                rec.stdout = Some(
                    cp.stdout
                        .chars()
                        .rev()
                        .take(2000)
                        .collect::<String>()
                        .chars()
                        .rev()
                        .collect(),
                );
                rec.stderr = Some(
                    cp.stderr
                        .chars()
                        .rev()
                        .take(2000)
                        .collect::<String>()
                        .chars()
                        .rev()
                        .collect(),
                );
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;
    use tempfile::tempdir;

    fn apply_cfg(home: PathBuf) -> Config {
        let mut cfg = Config::default();
        cfg.apply = true;
        cfg.home = home;
        cfg.gui_use_trash = false;
        cfg
    }

    #[test]
    fn parallel_deletes_many_files() {
        let dir = tempdir().unwrap();
        let home = dir.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let files_dir = dir.path().join("files");
        fs::create_dir_all(&files_dir).unwrap();

        let mut actions = Vec::new();
        for i in 0..64 {
            let p = files_dir.join(format!("f{i}.txt"));
            fs::write(&p, vec![b'x'; i]).unwrap();
            actions.push(Action::new(
                "files",
                "delete_file",
                p.to_string_lossy(),
                i as u64,
                "test",
            ));
        }

        let progress = Mutex::new(Vec::new());
        let (log, summary) = execute_actions_parallel(
            &actions,
            &apply_cfg(home),
            false,
            4,
            |p| progress.lock().unwrap().push(p.done),
        );

        assert_eq!(summary.files_deleted, 64);
        assert_eq!(summary.failures, 0);
        assert_eq!(log.len(), 64);
        assert!(log.iter().all(|r| r.applied));
        assert_eq!(fs::read_dir(&files_dir).unwrap().count(), 0);
        let last = *progress.lock().unwrap().last().unwrap();
        assert_eq!(last, 64);
    }

    #[test]
    fn rmdir_runs_after_parallel_deletes() {
        let dir = tempdir().unwrap();
        let home = dir.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let nested = dir.path().join("a").join("b");
        fs::create_dir_all(&nested).unwrap();
        let file = nested.join("x.txt");
        fs::write(&file, b"hi").unwrap();

        let actions = vec![
            Action::new(
                "files",
                "delete_file",
                file.to_string_lossy(),
                2,
                "file",
            ),
            Action::new(
                "files",
                "rmdir_if_empty",
                nested.to_string_lossy(),
                0,
                "empty",
            ),
            Action::new(
                "files",
                "rmdir_if_empty",
                dir.path().join("a").to_string_lossy(),
                0,
                "empty",
            ),
        ];

        let (log, summary) = execute_actions_parallel(&actions, &apply_cfg(home), false, 4, |_| {});
        assert_eq!(summary.files_deleted, 3);
        assert!(log[0].applied);
        assert!(log[1].applied);
        assert!(log[2].applied);
        assert!(!nested.exists());
        assert!(!dir.path().join("a").exists());
    }
}
