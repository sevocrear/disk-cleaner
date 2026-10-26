use crate::action::{Action, PhaseResult};
use crate::config::Config;
use crate::safety::{default_dedupe_roots, DeleteGuard, PARTIAL_HASH_BYTES};
use crate::util::{hash_file, iter_files_filtered};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub fn phase_dupes(cfg: &Config) -> PhaseResult {
    let mut result = PhaseResult::new("dupes");
    let guard = match DeleteGuard::new(cfg) {
        Ok(g) => g,
        Err(e) => {
            result.notes.push(format!("scan refused: {e}"));
            return result;
        }
    };
    let mut by_size: HashMap<u64, Vec<PathBuf>> = HashMap::new();
    // Hardlinks / overlapping roots: one entry per inode, deleting a link frees nothing.
    let mut seen_inodes: HashSet<(u64, u64)> = HashSet::new();

    let roots = default_dedupe_roots(cfg);
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
            if meta.len() < cfg.min_dupe_size {
                continue;
            }
            if !seen_inodes.insert(inode_key(&meta)) {
                continue;
            }
            if guard.file_block_reason(&f).is_some() {
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
                    result.add(
                        Action::new(
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
                    )
                    .with_keep(keeper.to_string_lossy()),
                    );
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

#[cfg(unix)]
fn inode_key(meta: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (meta.dev(), meta.ino())
}

#[cfg(not(unix))]
fn inode_key(_meta: &std::fs::Metadata) -> (u64, u64) {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    (0, NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}

/// Whether `copy` may be deleted as a duplicate of `keeper`: the keeper still exists,
/// is a different file (not a hardlink of it) and has exactly the same content.
pub fn keeper_still_identical(copy: &Path, keeper: &Path) -> Result<(), String> {
    let cm = std::fs::symlink_metadata(copy).map_err(|e| e.to_string())?;
    let km = std::fs::symlink_metadata(keeper)
        .map_err(|_| format!("kept copy is gone: {}", keeper.display()))?;
    if !km.is_file() || !cm.is_file() {
        return Err("not a regular file".into());
    }
    if inode_key(&cm) == inode_key(&km) {
        return Err("kept copy is the same file (hardlink)".into());
    }
    if cm.len() != km.len() {
        return Err(format!("kept copy changed: {}", keeper.display()));
    }
    let a = hash_file(copy, None).map_err(|e| e.to_string())?;
    let b = hash_file(keeper, None).map_err(|e| e.to_string())?;
    if a != b {
        return Err(format!("kept copy differs now: {}", keeper.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dupes_skip_system_and_protected_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("data");
        let blob = vec![7u8; 4096];
        let put = |rel: &str| {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, &blob).unwrap();
        };
        // Plain duplicates: one of them is reported.
        put("a/movie.mkv");
        put("b/movie.mkv");
        // Same content inside a git repo, a conda env, as libraries, hidden, never-delete.
        std::fs::create_dir_all(root.join("repo/.git")).unwrap();
        put("repo/data/movie.mkv");
        std::fs::create_dir_all(root.join("env/conda-meta")).unwrap();
        put("env/lib/movie.mkv");
        put("libs/libx.so.1");
        put("libs/liby.so.1");
        put(".hidden/movie.mkv");
        put("keep/movie.mkv");

        let mut cfg = Config::default();
        cfg.home = tmp.path().join("home");
        cfg.scan_mounts = vec![];
        cfg.dedupe_roots = vec![root.clone()];
        cfg.min_dupe_size = 1;
        cfg.never_delete = vec!["keep".into()];
        let res = phase_dupes(&cfg);
        let paths: Vec<&str> = res.actions.iter().map(|a| a.path.as_str()).collect();
        assert_eq!(paths.len(), 1, "{paths:?}");
        assert!(paths[0].ends_with("movie.mkv") && !paths[0].contains("repo"));
        let keep = res.actions[0].keep.as_deref().unwrap();
        assert!(keep.ends_with("/a/movie.mkv") || keep.ends_with("/b/movie.mkv"));
        assert_ne!(keep, paths[0]);
    }

    #[test]
    fn keeper_must_survive_identical() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        std::fs::write(&a, b"same").unwrap();
        std::fs::write(&b, b"same").unwrap();
        assert!(keeper_still_identical(&a, &b).is_ok());
        std::fs::write(&b, b"diff").unwrap();
        assert!(keeper_still_identical(&a, &b).is_err());
        std::fs::remove_file(&b).unwrap();
        assert!(keeper_still_identical(&a, &b).is_err());
        std::fs::hard_link(&a, &b).unwrap();
        assert!(keeper_still_identical(&a, &b).is_err());
    }
}
