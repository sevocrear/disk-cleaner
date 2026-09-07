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

pub fn default_file_roots(cfg: &Config) -> Vec<PathBuf> {
    if let Some(ref roots) = cfg.file_roots {
        return roots.clone();
    }
    let mut roots = vec![
        PathBuf::from("/tmp"),
        PathBuf::from("/var/tmp"),
        cfg.home.join(".cache"),
        cfg.home.join(".local/share/Trash"),
    ];
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
    roots.extend(cfg.extra_roots.clone());
    roots
}

pub fn default_media_roots(cfg: &Config) -> Vec<(String, PathBuf)> {
    let pairs = [
        ("Pictures", dirs::picture_dir().unwrap_or_else(|| cfg.home.join("Pictures"))),
        ("Videos", dirs::video_dir().unwrap_or_else(|| cfg.home.join("Videos"))),
        ("Music", dirs::audio_dir().unwrap_or_else(|| cfg.home.join("Music"))),
        ("Downloads", dirs::download_dir().unwrap_or_else(|| cfg.home.join("Downloads"))),
    ];
    pairs
        .into_iter()
        .filter(|(_, p)| p.exists())
        .map(|(n, p)| (n.to_string(), p))
        .collect()
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
    fn protect_segment() {
        let globs = vec!["**/huggingface/**".into()];
        assert!(matches_protect(Path::new("/home/u/.cache/huggingface/x"), &globs));
    }
}
