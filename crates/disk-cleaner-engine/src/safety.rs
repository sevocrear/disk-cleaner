use crate::config::Config;
use std::path::{Component, Path, PathBuf};

pub const DENY_PREFIXES: &[&str] = &[
    "/usr", "/bin", "/sbin", "/lib", "/lib32", "/lib64", "/libx32", "/etc", "/boot", "/sys",
    "/proc", "/dev", "/run", "/snap", "/var/lib/docker", "/var/lib/containerd", "/var/lib/snapd",
];

pub const DEFAULT_PROTECT_GLOBS: &[&str] = &[
    "**/huggingface/**",
    "**/torch/**",
    "**/transformers/**",
    "**/.ssh/**",
    "**/google-chrome/**",
    "**/chromium/**",
    "**/yandex-browser/**",
    "**/mozilla/**",
    "**/firefox/**",
];

pub const SNAP_PROTECT_NAMES: &[&str] = &[
    "bare",
    "snapd",
    "cups",
    "firmware-updater",
    "gtk-common-themes",
];

pub const SNAP_PROTECT_PREFIXES: &[&str] = &[
    "core", "gnome-", "mesa", "gtk-", "kde-", "kf6-", "plasma-",
];

pub const PARTIAL_HASH_BYTES: u64 = 1024 * 1024;

pub fn path_is_denied(path: &Path) -> bool {
    let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let s = resolved.to_string_lossy();
    for prefix in DENY_PREFIXES {
        if s == *prefix || s.starts_with(&format!("{prefix}/")) {
            return true;
        }
    }
    false
}

/// Why a user-picked path (Overview / browse) must not be deleted, or None if allowed.
///
/// Blocks relative paths, `/`, system prefixes, the home dir and its ancestors,
/// and mount points. Protect globs are *not* a block here — the UI warns instead.
pub fn manual_delete_block_reason(path: &Path, home: &Path) -> Option<String> {
    if !path.is_absolute() {
        return Some("not an absolute path".into());
    }
    if path
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Some("path contains . or ..".into());
    }
    if path.parent().is_none() {
        return Some("filesystem root".into());
    }
    // Check the path itself (not a symlink target) against system prefixes.
    let s = path.to_string_lossy();
    if DENY_PREFIXES
        .iter()
        .any(|p| s == *p || s.starts_with(&format!("{p}/")))
    {
        return Some("system path".into());
    }
    if path_is_denied(path) && !is_symlink(path) {
        return Some("system path".into());
    }
    if home.starts_with(path) {
        return Some("home directory or its parent".into());
    }
    if is_mount_point(path) {
        return Some("mount point".into());
    }
    None
}

fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

#[cfg(unix)]
pub fn is_mount_point(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !meta.is_dir() {
        return false;
    }
    let Some(parent) = path.parent() else {
        return true;
    };
    match std::fs::metadata(parent) {
        Ok(pm) => pm.dev() != meta.dev() || pm.ino() == meta.ino(),
        Err(_) => false,
    }
}

#[cfg(not(unix))]
pub fn is_mount_point(_path: &Path) -> bool {
    false
}

pub fn matches_protect(path: &Path, globs: &[String]) -> bool {
    let s = path.to_string_lossy();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let parts: Vec<String> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(os) => Some(os.to_string_lossy().to_string()),
            _ => None,
        })
        .collect();

    for g in globs {
        if g.is_empty() {
            continue;
        }
        if name == *g || s == *g {
            return true;
        }
        let seg = g.trim_matches('*').trim_matches('/');
        if !seg.is_empty() && !seg.contains('/') && (parts.iter().any(|p| p == seg) || name == seg)
        {
            return true;
        }
        if glob_match(g, &s) || glob_match(g, &name) {
            return true;
        }
    }
    false
}

fn glob_match(pattern: &str, text: &str) -> bool {
    // Simple glob: * and ** support for common protect patterns
    let pat = pattern.replace("**/", "*").replace("**", "*");
    let mut pi = 0;
    let mut ti = 0;
    let pb = pat.as_bytes();
    let tb = text.as_bytes();
    let mut star = None::<(usize, usize)>;
    while ti < tb.len() {
        if pi < pb.len() && (pb[pi] == b'?' || pb[pi] == tb[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < pb.len() && pb[pi] == b'*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, ti));
        } else {
            return false;
        }
    }
    while pi < pb.len() && pb[pi] == b'*' {
        pi += 1;
    }
    pi == pb.len()
}

pub fn resolve_protect_globs(cfg: &Config) -> Vec<String> {
    let mut globs: Vec<String> = DEFAULT_PROTECT_GLOBS.iter().map(|s| s.to_string()).collect();
    globs.extend(cfg.protect_globs.clone());
    if cfg.include_hf_cache {
        globs.retain(|g| {
            !g.contains("huggingface") && !g.contains("torch") && !g.contains("transformers")
        });
    }
    globs
}

pub fn is_snap_protected(name: &str, protect_apps: &[String]) -> bool {
    if protect_apps.iter().any(|a| a == name) || SNAP_PROTECT_NAMES.contains(&name) {
        return true;
    }
    SNAP_PROTECT_PREFIXES.iter().any(|p| name.starts_with(p))
}

/// Whether the root filesystem (`/`) is included. Uses safe allowlisted dirs, never walks all of `/`.
pub fn scans_root_fs(cfg: &Config) -> bool {
    cfg.scan_mounts.iter().any(|m| m == Path::new("/"))
}

/// Non-root mounts selected for scanning (walked from the mount point).
pub fn extra_disk_scan_roots(cfg: &Config) -> Vec<PathBuf> {
    cfg.scan_mounts
        .iter()
        .filter(|m| m.as_path() != Path::new("/"))
        .filter(|m| m.exists())
        .cloned()
        .collect()
}

pub fn default_file_roots(cfg: &Config) -> Vec<PathBuf> {
    if let Some(ref roots) = cfg.file_roots {
        return roots.clone();
    }
    let mut roots = Vec::new();
    if scans_root_fs(cfg) {
        roots.extend([
            PathBuf::from("/tmp"),
            PathBuf::from("/var/tmp"),
            cfg.home.join(".cache"),
            cfg.home.join(".local/share/Trash"),
        ]);
    }
    roots.extend(extra_disk_scan_roots(cfg));
    roots.extend(cfg.extra_roots.clone());
    roots
        .into_iter()
        .filter(|r| cfg.extra_roots.contains(r) || r.exists())
        .collect()
}

pub fn default_dedupe_roots(cfg: &Config) -> Vec<PathBuf> {
    if !cfg.dedupe_roots.is_empty() {
        return cfg.dedupe_roots.clone();
    }
    let mut roots = Vec::new();
    if scans_root_fs(cfg) {
        for r in [
            cfg.home.join("Downloads"),
            cfg.home.join("Applications"),
            cfg.home.join("AppImages"),
            PathBuf::from("/tmp"),
            PathBuf::from("/var/tmp"),
        ] {
            if r.exists() {
                roots.push(r);
            }
        }
    }
    roots.extend(extra_disk_scan_roots(cfg));
    roots.extend(cfg.extra_roots.clone());
    roots
}

pub fn default_media_roots(cfg: &Config) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    if scans_root_fs(cfg) {
        let pairs = [
            ("Pictures", dirs::picture_dir().unwrap_or_else(|| cfg.home.join("Pictures"))),
            ("Videos", dirs::video_dir().unwrap_or_else(|| cfg.home.join("Videos"))),
            ("Music", dirs::audio_dir().unwrap_or_else(|| cfg.home.join("Music"))),
            ("Downloads", dirs::download_dir().unwrap_or_else(|| cfg.home.join("Downloads"))),
        ];
        out.extend(
            pairs
                .into_iter()
                .filter(|(_, p)| p.exists())
                .map(|(n, p)| (n.to_string(), p)),
        );
    }
    // On extra disks, only known library folders — not the whole mount (that is "Old files").
    for m in extra_disk_scan_roots(cfg) {
        for name in ["Pictures", "Videos", "Music", "Downloads", "photos", "movies"] {
            let p = m.join(name);
            if p.is_dir() {
                out.push((format!("{} ({})", name, m.display()), p));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denies_usr() {
        assert!(path_is_denied(Path::new("/usr/bin/ls")));
        assert!(!path_is_denied(Path::new("/tmp/foo")));
    }

    #[test]
    fn manual_delete_guards() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(home.join("junk")).unwrap();
        let block = |p: &Path| manual_delete_block_reason(p, &home);

        assert!(block(&home.join("junk")).is_none());
        assert!(block(&home.join("missing-file")).is_none());
        assert!(block(&home).unwrap().contains("home"));
        assert!(block(tmp.path()).unwrap().contains("home"));
        assert!(block(Path::new("/")).is_some());
        assert!(block(Path::new("/usr/share")).unwrap().contains("system"));
        assert!(block(Path::new("/var/lib/docker")).is_some());
        assert!(block(Path::new("relative/x")).is_some());
        assert!(block(&home.join("junk/../junk")).is_some());
        // /proc is a separate filesystem → mount point (and denied anyway).
        assert!(block(Path::new("/proc")).is_some());
    }

    #[test]
    fn mount_point_detection() {
        assert!(is_mount_point(Path::new("/proc")));
        let tmp = tempfile::tempdir().unwrap();
        assert!(!is_mount_point(tmp.path()));
        assert!(!is_mount_point(&tmp.path().join("missing")));
    }

    #[test]
    fn protect_segment() {
        let globs = vec!["**/huggingface/**".into()];
        assert!(matches_protect(Path::new("/home/u/.cache/huggingface/x"), &globs));
    }

    #[test]
    fn default_scan_mounts_is_root() {
        let cfg = Config::default();
        assert!(scans_root_fs(&cfg));
        assert_eq!(cfg.scan_mounts, vec![PathBuf::from("/")]);
    }

    #[test]
    fn file_roots_respect_scan_mounts() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&data).unwrap();

        let mut cfg = Config::default();
        cfg.home = tmp.path().join("home");
        std::fs::create_dir_all(cfg.home.join(".cache")).unwrap();
        cfg.scan_mounts = vec![PathBuf::from("/")];
        let roots = default_file_roots(&cfg);
        assert!(roots.iter().any(|r| r.ends_with(".cache")));
        assert!(!roots.contains(&data));

        cfg.scan_mounts = vec![data.clone()];
        let roots = default_file_roots(&cfg);
        assert!(roots.contains(&data));
        assert!(!roots.iter().any(|r| r.ends_with(".cache")));
    }
}
