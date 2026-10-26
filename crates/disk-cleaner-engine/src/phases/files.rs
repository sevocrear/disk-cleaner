use crate::action::{Action, PhaseResult};
use crate::config::Config;
use crate::safety::{default_file_roots, DeleteGuard};
use crate::util::{days_to_seconds, file_age_ok_for_delete, iter_files_filtered, now_ts};
use std::collections::HashSet;
use std::path::PathBuf;

pub fn phase_files(cfg: &Config) -> PhaseResult {
    let mut result = PhaseResult::new("files");
    let guard = match DeleteGuard::new(cfg) {
        Ok(g) => g,
        Err(e) => {
            result.notes.push(format!("scan refused: {e}"));
            return result;
        }
    };
    let cutoff = now_ts() - days_to_seconds(cfg.file_unused_days);
    let roots = default_file_roots(cfg);
    let mut candidates: Vec<PathBuf> = Vec::new();

    for root in &roots {
        if !root.exists() {
            continue;
        }
        if let Some(reason) = guard.root_block_reason(root) {
            result
                .notes
                .push(format!("protected root skipped: {} ({reason})", root.display()));
            continue;
        }
        for f in iter_files_filtered(root, cfg.cross_fs, |d| guard.skip_dir(d)) {
            let Ok(meta) = std::fs::metadata(&f) else { continue };
            if meta.len() < cfg.min_file_size {
                continue;
            }
            if !file_age_ok_for_delete(&f, cutoff) {
                continue;
            }
            if guard.file_block_reason(&f).is_some() {
                continue;
            }
            candidates.push(f.clone());
            result.add(Action::new(
                "files",
                "delete_file",
                f.to_string_lossy(),
                meta.len(),
                format!("mtime+atime >{}d", cfg.file_unused_days),
            ));
        }
    }

    let root_set: HashSet<PathBuf> = roots
        .iter()
        .filter_map(|r| r.canonicalize().ok())
        .collect();
    let mut dirs_to_check: HashSet<PathBuf> = HashSet::new();
    for f in &candidates {
        let mut parent = f.parent().map(|p| p.to_path_buf());
        while let Some(p) = parent {
            let resolved = p.canonicalize().unwrap_or_else(|_| p.clone());
            if root_set.contains(&resolved) {
                break;
            }
            dirs_to_check.insert(p.clone());
            if p.parent() == Some(p.as_path()) {
                break;
            }
            parent = p.parent().map(|x| x.to_path_buf());
            if parent.as_ref() == Some(&p) {
                break;
            }
        }
    }

    let mut dirs: Vec<_> = dirs_to_check.into_iter().collect();
    dirs.sort_by_key(|p| std::cmp::Reverse(p.to_string_lossy().len()));
    for d in dirs {
        result.add(Action::new(
            "files",
            "rmdir_if_empty",
            d.to_string_lossy(),
            0,
            "empty after age purge",
        ));
    }
    result
}
