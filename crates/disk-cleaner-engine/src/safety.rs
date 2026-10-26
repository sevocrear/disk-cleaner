use crate::config::Config;
use regex::Regex;
use std::io::Read;
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

/// Directory names whose contents the file / dupe / media scans never touch, at any depth:
/// VCS metadata, language environments and package stores, OS trees on other disks.
pub const SYSTEM_DIR_NAMES: &[&str] = &[
    ".git", ".hg", ".svn", ".bzr", "node_modules", "site-packages", "dist-packages",
    "__pycache__", ".venv", "venv", "virtualenv", ".tox", ".conda", "conda-meta", "anaconda3",
    "miniconda3", "miniforge3", "mambaforge", "micromamba", ".cargo", ".rustup", ".nvm",
    ".pyenv", ".rbenv", ".gradle", ".m2", ".ssh", ".gnupg", ".config", "lost+found",
    "System Volume Information", "$RECYCLE.BIN", "Windows", "Program Files",
    "Program Files (x86)", "ProgramData",
];

/// A directory containing one of these is a project / environment root: left alone entirely.
pub const PROJECT_MARKERS: &[&str] = &[".git", ".hg", ".svn", "pyvenv.cfg", "conda-meta"];

/// Extensions of libraries, binaries, package archives, configs, keys, databases and VM disks.
pub const SYSTEM_FILE_EXTS: &[&str] = &[
    "so", "ko", "o", "a", "la", "dll", "sys", "dylib", "pyc", "pyo", "pyd", "conda", "efi",
    "deb", "rpm", "snap", "conf", "cfg", "ini", "config", "service", "socket", "timer",
    "desktop", "rules", "policy", "pem", "key", "crt", "cer", "p12", "pfx", "kdbx", "gpg", "db",
    "sqlite", "sqlite3", "lock", "qcow2", "vdi", "vmdk", "vhd", "vhdx",
];

/// Why a directory must not be entered (or its files deleted) by automatic scans.
///
/// `/`, the home dir and its parents are exempt from marker checks: a whole-system or
/// dotfiles-in-git home would otherwise hide everything.
pub fn system_dir_reason(dir: &Path, home: &Path) -> Option<String> {
    if let Some(name) = dir.file_name().map(|n| n.to_string_lossy()) {
        if SYSTEM_DIR_NAMES.iter().any(|n| name.eq_ignore_ascii_case(n)) {
            return Some(format!("system/project dir {name}"));
        }
    }
    if dir.parent().is_none() || home.starts_with(dir) {
        return None;
    }
    if let Some(m) = PROJECT_MARKERS.iter().find(|m| dir.join(m).exists()) {
        return Some(format!("project/env root (has {m})"));
    }
    if dir.join("etc").is_dir() && dir.join("usr").is_dir() {
        return Some("looks like an OS root".into());
    }
    None
}

/// Library, executable, config, key or database file: never an automatic cleanup target.
pub fn system_file_reason(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_string_lossy().to_lowercase();
    if name.ends_with(".appimage") {
        return None;
    }
    let parts: Vec<&str> = name.split('.').collect();
    if parts.len() > 1 {
        let ext = parts[parts.len() - 1];
        if SYSTEM_FILE_EXTS.contains(&ext) {
            return Some(format!("system file type .{ext}"));
        }
        // Versioned shared libraries: libfoo.so.1.2
        if let Some(i) = parts.iter().position(|p| *p == "so") {
            if i > 0 && parts[i + 1..].iter().all(|p| p.chars().all(|c| c.is_ascii_digit())) {
                return Some("shared library".into());
            }
        }
    }
    let mut head = [0u8; 4];
    if let Ok(mut f) = std::fs::File::open(path) {
        if f.read_exact(&mut head).is_ok() {
            if &head == b"\x7fELF" {
                return Some("executable / library (ELF)".into());
            }
            if &head[..2] == b"#!" {
                return Some("script".into());
            }
        }
    }
    None
}

/// First protected ancestor of `path` (see [`system_dir_reason`]).
pub fn system_ancestor_reason(path: &Path, home: &Path) -> Option<String> {
    path.ancestors()
        .skip(1)
        .find_map(|d| system_dir_reason(d, home).map(|r| format!("{r}: {}", d.display())))
}

/// Compile `never_delete` patterns. Glob by default, `re:` prefix for a raw regex.
///
/// Globs: `*` = within one path segment, `**` = any depth, `?` = one char, `~/` = home.
/// A pattern with no `/` matches any file or directory name (`*.py`, `secrets`);
/// one starting with `/` or `~/` is anchored at the root; otherwise it matches any
/// trailing part of the path (`path/*.py`). A match on a directory protects all inside it.
pub fn compile_never_delete(patterns: &[String], home: &Path) -> Result<Vec<Regex>, String> {
    patterns
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .map(|p| {
            let src = match p.strip_prefix("re:") {
                Some(re) => re.trim().to_string(),
                None => glob_to_regex(p, home),
            };
            Regex::new(&src).map_err(|e| format!("invalid pattern `{p}`: {e}"))
        })
        .collect()
}

fn glob_to_regex(glob: &str, home: &Path) -> String {
    let expanded = match glob.strip_prefix("~/") {
        Some(rest) => format!("{}/{rest}", home.to_string_lossy().trim_end_matches('/')),
        None if glob == "~" => home.to_string_lossy().into_owned(),
        None => glob.to_string(),
    };
    let anchored = expanded.starts_with('/');
    let body = expanded.trim_start_matches('/').trim_end_matches('/');
    let mut re = String::from(if anchored { "^/" } else { "(?:^|/)" });
    let chars: Vec<char> = body.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' if chars.get(i + 1) == Some(&'*') => {
                if chars.get(i + 2) == Some(&'/') {
                    re.push_str("(?:.*/)?");
                    i += 3;
                } else {
                    re.push_str(".*");
                    i += 2;
                }
                continue;
            }
            '*' => re.push_str("[^/]*"),
            '?' => re.push_str("[^/]"),
            c => re.push_str(&regex::escape(&c.to_string())),
        }
        i += 1;
    }
    re.push_str("(?:/|$)");
    re
}

/// Delete guard for automatic cleanups: protect globs + user `never_delete` + system checks.
pub struct DeleteGuard {
    home: PathBuf,
    protect: Vec<String>,
    never: Vec<Regex>,
}

impl DeleteGuard {
    /// Fails closed: an invalid `never_delete` pattern is an error, not ignored.
    pub fn new(cfg: &Config) -> Result<Self, String> {
        Ok(Self {
            home: cfg.home.clone(),
            protect: resolve_protect_globs(cfg),
            never: compile_never_delete(&cfg.never_delete, &cfg.home)?,
        })
    }

    pub fn is_never_delete(&self, path: &Path) -> bool {
        let s = path.to_string_lossy();
        self.never.iter().any(|re| re.is_match(&s))
    }

    /// First path under `dir` (or `dir` itself) matching `never_delete`.
    pub fn never_delete_inside(&self, dir: &Path) -> Option<PathBuf> {
        if self.is_never_delete(dir) {
            return Some(dir.to_path_buf());
        }
        if self.never.is_empty() || !dir.is_dir() {
            return None;
        }
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&d) else { continue };
            for e in entries.flatten() {
                let p = e.path();
                if self.is_never_delete(&p) {
                    return Some(p);
                }
                if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    stack.push(p);
                }
            }
        }
        None
    }

    /// Whether a scan should not descend into `dir` (below the scan root).
    pub fn skip_dir(&self, dir: &Path) -> bool {
        let hidden = dir
            .file_name()
            .map(|n| n.to_string_lossy().starts_with('.'))
            .unwrap_or(false);
        hidden
            || path_is_denied(dir)
            || self.is_never_delete(dir)
            || matches_protect(dir, &self.protect)
            || system_dir_reason(dir, &self.home).is_some()
    }

    /// Why a scan root must be skipped entirely, or None.
    pub fn root_block_reason(&self, root: &Path) -> Option<String> {
        if path_is_denied(root) {
            return Some("system path".into());
        }
        if self.is_never_delete(root) {
            return Some("matches a never-delete pattern".into());
        }
        system_dir_reason(root, &self.home).or_else(|| system_ancestor_reason(root, &self.home))
    }

    /// Why a file found by an automatic scan (old files / dupes / media) must not be deleted.
    /// Re-checked right before deletion.
    pub fn file_block_reason(&self, path: &Path) -> Option<String> {
        if path_is_denied(path) {
            return Some("system path".into());
        }
        if self.is_never_delete(path) {
            return Some("matches a never-delete pattern".into());
        }
        if matches_protect(path, &self.protect) {
            return Some("matches a protect glob".into());
        }
        let hidden = path
            .file_name()
            .map(|n| n.to_string_lossy().starts_with('.'))
            .unwrap_or(false);
        if hidden {
            return Some("hidden file".into());
        }
        system_file_reason(path).or_else(|| system_ancestor_reason(path, &self.home))
    }
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

    fn never(patterns: &[&str]) -> Vec<Regex> {
        let pats: Vec<String> = patterns.iter().map(|s| s.to_string()).collect();
        compile_never_delete(&pats, Path::new("/home/u")).unwrap()
    }

    fn hit(res: &[Regex], path: &str) -> bool {
        res.iter().any(|r| r.is_match(path))
    }

    #[test]
    fn never_delete_globs() {
        let r = never(&["path/*.py"]);
        assert!(hit(&r, "/home/u/path/x.py"));
        assert!(hit(&r, "/media/d/proj/path/x.py"));
        assert!(!hit(&r, "/home/u/path/sub/x.py"));
        assert!(!hit(&r, "/home/u/mypath/x.py"));

        let r = never(&["path/**/*.py"]);
        assert!(hit(&r, "/a/path/x.py"));
        assert!(hit(&r, "/a/path/s/t/x.py"));

        let r = never(&["*.co*"]);
        assert!(hit(&r, "/x/env.conda"));
        assert!(hit(&r, "/x/a.config"));
        assert!(!hit(&r, "/x/conda/a.txt"));

        // Directory match protects everything inside it.
        let r = never(&["~/projects"]);
        assert!(hit(&r, "/home/u/projects"));
        assert!(hit(&r, "/home/u/projects/a/b.bin"));
        assert!(!hit(&r, "/home/u/projects2/b.bin"));
        assert!(!hit(&r, "/media/home/u/projects/b.bin"));

        let r = never(&["/media/data/keep"]);
        assert!(hit(&r, "/media/data/keep/x"));
        assert!(!hit(&r, "/other/media/data/keep/x"));

        let r = never(&["re:\\.(iso|img)$"]);
        assert!(hit(&r, "/d/ubuntu.iso"));
        assert!(!hit(&r, "/d/ubuntu.iso.txt"));

        assert!(compile_never_delete(&["re:(".into()], Path::new("/h")).is_err());
        assert!(never(&["", "  "]).is_empty());
    }

    #[test]
    fn system_files_detected() {
        let tmp = tempfile::tempdir().unwrap();
        let w = |name: &str, data: &[u8]| {
            let p = tmp.path().join(name);
            std::fs::write(&p, data).unwrap();
            p
        };
        assert!(system_file_reason(&w("libfoo.so", b"x")).is_some());
        assert!(system_file_reason(&w("libfoo.so.1.2", b"x")).is_some());
        assert!(system_file_reason(&w("pkg.conda", b"x")).is_some());
        assert!(system_file_reason(&w("app.conf", b"x")).is_some());
        assert!(system_file_reason(&w("noext", b"\x7fELF....")).is_some());
        assert!(system_file_reason(&w("run", b"#!/bin/sh\n")).is_some());
        assert!(system_file_reason(&w("Tool.AppImage", b"\x7fELF....")).is_none());
        assert!(system_file_reason(&w("movie.mp4", b"data")).is_none());
        assert!(system_file_reason(&w("notes.so.txt", b"data")).is_none());
    }

    #[test]
    fn system_dirs_detected() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        let os = tmp.path().join("oldroot");
        std::fs::create_dir_all(os.join("etc")).unwrap();
        std::fs::create_dir_all(os.join("usr")).unwrap();
        let plain = tmp.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();

        assert!(system_dir_reason(&repo, &home).is_some());
        assert!(system_dir_reason(&os, &home).is_some());
        assert!(system_dir_reason(&plain, &home).is_none());
        assert!(system_dir_reason(Path::new("/x/site-packages"), &home).is_some());
        assert!(system_dir_reason(Path::new("/"), &home).is_none());
        assert!(system_ancestor_reason(&repo.join("src/a.bin"), &home).is_some());
        assert!(system_ancestor_reason(&plain.join("a.bin"), &home).is_none());
    }

    #[test]
    fn guard_fails_closed_on_bad_pattern() {
        let mut cfg = Config::default();
        cfg.never_delete = vec!["re:[".into()];
        assert!(DeleteGuard::new(&cfg).is_err());
    }
}
