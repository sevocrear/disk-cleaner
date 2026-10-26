use crate::action::{Action, PhaseResult};
use crate::config::Config;
use crate::safety::{default_media_roots, DeleteGuard};
use crate::util::{days_to_seconds, file_age_ok_for_delete, iter_files_filtered, now_ts};

/// Analyze Pictures, Videos, Music, Downloads for large/old files.
pub fn phase_media(cfg: &Config) -> PhaseResult {
    let mut result = PhaseResult::new("media");
    let guard = match DeleteGuard::new(cfg) {
        Ok(g) => g,
        Err(e) => {
            result.notes.push(format!("scan refused: {e}"));
            return result;
        }
    };
    let cutoff = now_ts() - days_to_seconds(cfg.file_unused_days);
    let roots = default_media_roots(cfg);

    if roots.is_empty() {
        result.notes.push("no media directories found".into());
        return result;
    }

    for (label, root) in roots {
        if let Some(reason) = guard.root_block_reason(&root) {
            result.notes.push(format!("{label}: skipped ({reason})"));
            continue;
        }
        let mut count = 0u64;
        let mut bytes = 0u64;
        for f in iter_files_filtered(&root, cfg.cross_fs, |d| guard.skip_dir(d)) {
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
            count += 1;
            bytes += meta.len();
            result.add(Action::new(
                "media",
                "delete_file",
                f.to_string_lossy(),
                meta.len(),
                format!("{label}: large+unused >{}d", cfg.file_unused_days),
            ));
        }
        result.notes.push(format!(
            "{label}: {count} candidates (~{})",
            crate::util::format_bytes(bytes)
        ));
    }
    result
}
