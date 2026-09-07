use crate::action::{Action, PhaseResult};
use crate::config::Config;
use crate::safety::{
    default_dedupe_roots, matches_protect, path_is_denied, resolve_protect_globs, PARTIAL_HASH_BYTES,
};
use crate::util::{hash_file, iter_files};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub fn phase_dupes(cfg: &Config) -> PhaseResult {
    let mut result = PhaseResult::new("dupes");
    let protect = resolve_protect_globs(cfg);
    let mut by_size: HashMap<u64, Vec<PathBuf>> = HashMap::new();

    let roots = default_dedupe_roots(cfg);
    for root in &roots {
        if path_is_denied(root) || !root.exists() {
            continue;
        }
        for f in iter_files(root, cfg.cross_fs) {
            if path_is_denied(&f) || matches_protect(&f, &protect) {
                continue;
            }
            let Ok(meta) = std::fs::metadata(&f) else { continue };
            if meta.len() < cfg.min_dupe_size {
                continue;
            }
            by_size.entry(meta.len()).or_default().push(f);
        }
    }

    for (size, paths) in by_size {
        if paths.len() < 2 {
            continue;
        }
        let mut partial_groups: HashMap<String, Vec<PathBuf>> = HashMap::new();
        for p in &paths {
            if let Ok(h) = hash_file(p, Some(PARTIAL_HASH_BYTES)) {
                partial_groups.entry(h).or_default().push(p.clone());
            }
        }
        for group in partial_groups.values() {
            if group.len() < 2 {
                continue;
            }
            let mut full_groups: HashMap<String, Vec<PathBuf>> = HashMap::new();
            for p in group {
                if let Ok(h) = hash_file(p, None) {
                    full_groups.entry(h).or_default().push(p.clone());
                }
            }
            for (digest, identical) in full_groups {
                if identical.len() < 2 {
                    continue;
                }
                let Ok(keeper) = pick_keeper(&identical, &cfg.dedupe_keep) else {
                    continue;
                };
                for p in &identical {
                    if p == &keeper {
                        continue;
                    }
                    if matches_protect(p, &protect) {
                        continue;
                    }
                    result.add(Action::new(
                        "dupes",
                        "delete_file",
                        p.to_string_lossy(),
                        size,
                        format!(
                            "dupe of {} sha256={}… keep={}",
                            keeper.display(),
                            &digest[..12.min(digest.len())],
                            cfg.dedupe_keep
                        ),
                    ));
                }
            }
        }
    }
    result
}

fn pick_keeper(paths: &[PathBuf], policy: &str) -> std::io::Result<PathBuf> {
    match policy {
        "first" => {
            let mut sorted = paths.to_vec();
            sorted.sort();
            Ok(sorted[0].clone())
        }
        "oldest" => paths
            .iter()
            .min_by_key(|p| {
                (
                    std::fs::metadata(p)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(0),
                    p.to_string_lossy().to_string(),
                )
            })
            .cloned()
            .ok_or_else(|| std::io::Error::other("empty")),
        _ => paths
            .iter()
            .max_by_key(|p| {
                (
                    std::fs::metadata(p)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(0),
                    p.to_string_lossy().to_string(),
                )
            })
            .cloned()
            .ok_or_else(|| std::io::Error::other("empty")),
    }
}

#[allow(dead_code)]
fn _path_eq(a: &Path, b: &Path) -> bool {
    a == b
}
