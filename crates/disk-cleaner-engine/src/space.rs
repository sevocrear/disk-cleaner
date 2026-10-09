//! Explain "invisible" disk usage: what `df` counts but a user-level scan can't see.
//!
//! Typical culprits: Docker data under root-only `/var/lib/docker`, files that were
//! deleted while a process still holds them open, and other root-only directories.

use crate::cmd::{run_command, which_bin};
use crate::config::Config;
use crate::safety::is_mount_point;
use crate::util::parse_size;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FsUsage {
    pub mount_point: String,
    pub total_bytes: u64,
    /// Like `df` "Used": blocks in use, excluding the root reserve.
    pub used_bytes: u64,
    /// Free for unprivileged users.
    pub available_bytes: u64,
    /// Blocks reserved for root (ext4 default 5%).
    pub reserved_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HiddenSource {
    /// `docker`, `deleted_open`, or `unreadable`.
    pub kind: String,
    pub label: String,
    pub bytes: u64,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaceAccounting {
    pub fs: FsUsage,
    pub root: String,
    /// Scan root is the mount point, so `used - scanned` is meaningful.
    pub is_mount_root: bool,
    pub scanned_bytes: u64,
    /// `used - scanned` when `is_mount_root`, else 0.
    pub unaccounted_bytes: u64,
    pub sources: Vec<HiddenSource>,
    pub unreadable_dirs: u64,
}

/// Nearest mount point at or above `path`.
pub fn mount_point_of(path: &Path) -> PathBuf {
    let mut cur = path.to_path_buf();
    loop {
        if is_mount_point(&cur) {
            return cur;
        }
        match cur.parent() {
            Some(p) => cur = p.to_path_buf(),
            None => return cur,
        }
    }
}

#[cfg(unix)]
pub fn fs_usage(path: &Path) -> Option<FsUsage> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid NUL-terminated path and `st` is a properly sized buffer.
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let frsize = st.f_frsize as u64;
    let blocks = st.f_blocks as u64;
    let bfree = st.f_bfree as u64;
    let bavail = st.f_bavail as u64;
    Some(FsUsage {
        mount_point: mount_point_of(path).to_string_lossy().into_owned(),
        total_bytes: blocks * frsize,
        used_bytes: blocks.saturating_sub(bfree) * frsize,
        available_bytes: bavail * frsize,
        reserved_bytes: bfree.saturating_sub(bavail) * frsize,
    })
}

#[cfg(not(unix))]
pub fn fs_usage(_path: &Path) -> Option<FsUsage> {
    None
}

#[cfg(unix)]
fn dev_of(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| m.dev())
}

#[cfg(not(unix))]
fn dev_of(_path: &Path) -> Option<u64> {
    None
}

/// Files deleted from the namespace but still open, on filesystem `dev`.
/// Returns (bytes, process name → bytes).
pub fn deleted_open_files(proc_root: &Path, dev: u64) -> (u64, BTreeMap<String, u64>) {
    use std::os::unix::fs::MetadataExt;
    let mut total = 0u64;
    let mut by_proc: BTreeMap<String, u64> = BTreeMap::new();
    let mut seen: HashSet<(u64, u64)> = HashSet::new();
    let Ok(procs) = std::fs::read_dir(proc_root) else {
        return (0, by_proc);
    };
    for p in procs.flatten() {
        if p.file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
            .is_none()
        {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(p.path().join("fd")) else {
            continue;
        };
        let mut name: Option<String> = None;
        for fd in fds.flatten() {
            let Ok(target) = std::fs::read_link(fd.path()) else {
                continue;
            };
            if !target.to_string_lossy().ends_with(" (deleted)") {
                continue;
            }
            // metadata() on the fd link stats the still-open inode.
            let Ok(meta) = std::fs::metadata(fd.path()) else {
                continue;
            };
            if meta.dev() != dev || !meta.is_file() || !seen.insert((meta.dev(), meta.ino())) {
                continue;
            }
            let bytes = meta.blocks() * 512;
            if bytes == 0 {
                continue;
            }
            total += bytes;
            let n = name.get_or_insert_with(|| {
                std::fs::read_to_string(p.path().join("comm"))
                    .map(|s| s.trim().to_string())
                    .unwrap_or_else(|_| "?".into())
            });
            *by_proc.entry(n.clone()).or_default() += bytes;
        }
    }
    (total, by_proc)
}

/// Parse `docker system df --format '{{.Type}}\t{{.Size}}'` into (total, breakdown).
pub fn parse_docker_df_sizes(stdout: &str) -> (u64, Vec<(String, u64)>) {
    let mut total = 0u64;
    let mut parts = Vec::new();
    for line in stdout.lines() {
        let Some((ty, size)) = line.trim().split_once('\t') else {
            continue;
        };
        let Ok(bytes) = parse_size(size.trim()) else {
            continue;
        };
        total = total.saturating_add(bytes);
        parts.push((ty.trim().to_string(), bytes));
    }
    (total, parts)
}

fn docker_source(cfg: &Config, dev: u64) -> Option<HiddenSource> {
    which_bin("docker")?;
    let root = run_command(
        &[
            "docker".into(),
            "info".into(),
            "--format".into(),
            "{{.DockerRootDir}}".into(),
        ],
        cfg,
    );
    if root.status != 0 {
        return None;
    }
    let root_dir = PathBuf::from(root.stdout.trim());
    if dev_of(&root_dir)? != dev {
        return None;
    }
    let df = run_command(
        &[
            "docker".into(),
            "system".into(),
            "df".into(),
            "--format".into(),
            "{{.Type}}\t{{.Size}}".into(),
        ],
        cfg,
    );
    if df.status != 0 {
        return None;
    }
    let (total, parts) = parse_docker_df_sizes(&df.stdout);
    if total == 0 {
        return None;
    }
    let detail = parts
        .iter()
        .filter(|(_, b)| *b > 0)
        .map(|(t, b)| format!("{t} {}", crate::util::format_bytes(*b)))
        .collect::<Vec<_>>()
        .join(" · ");
    Some(HiddenSource {
        kind: "docker".into(),
        label: format!("Docker data ({})", root_dir.display()),
        bytes: total,
        detail,
    })
}

/// Build the accounting for a finished scan of `root` that found `scanned_bytes`.
pub fn space_accounting(
    root: &Path,
    scanned_bytes: u64,
    unreadable_dirs: u64,
    cfg: &Config,
) -> Option<SpaceAccounting> {
    let fs = fs_usage(root)?;
    let is_mount_root = Path::new(&fs.mount_point) == root;
    let dev = dev_of(root)?;
    let mut sources = Vec::new();

    if let Some(src) = docker_source(cfg, dev) {
        sources.push(src);
    }
    let (deleted, by_proc) = deleted_open_files(Path::new("/proc"), dev);
    if deleted > 0 {
        let mut procs: Vec<_> = by_proc.into_iter().collect();
        procs.sort_by_key(|p| std::cmp::Reverse(p.1));
        let detail = procs
            .iter()
            .take(5)
            .map(|(n, b)| format!("{n} {}", crate::util::format_bytes(*b)))
            .collect::<Vec<_>>()
            .join(" · ");
        sources.push(HiddenSource {
            kind: "deleted_open".into(),
            label: "Deleted files still held open".into(),
            bytes: deleted,
            detail: format!("{detail} — freed when these processes exit"),
        });
    }

    let unaccounted_bytes = if is_mount_root {
        fs.used_bytes.saturating_sub(scanned_bytes)
    } else {
        0
    };
    if is_mount_root {
        let explained: u64 = sources.iter().map(|s| s.bytes).sum();
        let rest = unaccounted_bytes.saturating_sub(explained);
        // Ignore filesystem metadata noise (journal, inode tables): ~1% of used.
        if rest > fs.used_bytes / 100 {
            sources.push(HiddenSource {
                kind: "unreadable".into(),
                label: "Other unreadable or root-only data".into(),
                bytes: rest,
                detail: format!(
                    "{unreadable_dirs} directories could not be read; run as root to see them"
                ),
            });
        }
    }

    Some(SpaceAccounting {
        fs,
        root: root.to_string_lossy().into_owned(),
        is_mount_root,
        scanned_bytes,
        unaccounted_bytes,
        sources,
        unreadable_dirs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_df_sizes() {
        let (total, parts) = parse_docker_df_sizes(
            "Images\t3.502GB\nContainers\t745.5kB\nLocal Volumes\t10.23GB\nBuild Cache\t0B\njunk line\n",
        );
        assert_eq!(parts.len(), 4);
        assert_eq!(
            total,
            parse_size("3.502G").unwrap()
                + parse_size("745.5k").unwrap()
                + parse_size("10.23G").unwrap()
        );
    }

    #[test]
    fn fs_usage_of_tmp_is_consistent() {
        let tmp = tempfile::tempdir().unwrap();
        let u = fs_usage(tmp.path()).unwrap();
        assert!(u.total_bytes > 0);
        assert!(u.used_bytes <= u.total_bytes);
        assert!(u.available_bytes + u.used_bytes <= u.total_bytes);
        assert!(Path::new(&u.mount_point).is_absolute());
        assert!(tmp.path().starts_with(&u.mount_point));
    }

    #[test]
    fn mount_point_of_walks_up() {
        assert_eq!(
            mount_point_of(Path::new("/proc/self")),
            PathBuf::from("/proc")
        );
        assert_eq!(mount_point_of(Path::new("/")), PathBuf::from("/"));
    }

    #[test]
    fn finds_deleted_file_held_open() {
        use std::io::Write;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ghost.bin");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(&vec![7u8; 256 * 1024]).unwrap();
        f.sync_all().unwrap();
        std::fs::remove_file(&path).unwrap();
        let dev = dev_of(tmp.path()).unwrap();
        let (bytes, by_proc) = deleted_open_files(Path::new("/proc"), dev);
        drop(f);
        assert!(bytes >= 256 * 1024, "bytes={bytes}");
        assert!(!by_proc.is_empty());
    }

    #[test]
    fn accounting_for_subdir_has_no_unaccounted() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = Config::default();
        cfg.cmd_timeout_sec = 5;
        let acc = space_accounting(tmp.path(), 0, 0, &cfg).unwrap();
        assert!(!acc.is_mount_root);
        assert_eq!(acc.unaccounted_bytes, 0);
        assert!(acc.sources.iter().all(|s| s.kind != "unreadable"));
    }
}
