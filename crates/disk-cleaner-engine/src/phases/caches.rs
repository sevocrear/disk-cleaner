use crate::action::{Action, PhaseResult};
use crate::cmd::{which_bin};
use crate::config::Config;
use crate::safety::{matches_protect, resolve_protect_globs};
use crate::util::{
    days_to_seconds, dir_size, file_age_ok_for_delete, iter_files, newest_activity, now_ts,
};

pub fn phase_caches(cfg: &Config) -> PhaseResult {
    let mut result = PhaseResult::new("caches");
    let protect = resolve_protect_globs(cfg);
    let cutoff = now_ts() - days_to_seconds(cfg.file_unused_days);
    let cache_root = cfg.home.join(".cache");

    if cache_root.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&cache_root) {
            for entry in entries.flatten() {
                let p = entry.path();
                if matches_protect(&p, &protect) {
                    continue;
                }
                let Ok(meta) = entry.metadata() else { continue };
                let is_dir = meta.is_dir();
                if is_dir {
                    let Some(act) = newest_activity(&p) else { continue };
                    if act >= cutoff {
                        continue;
                    }
                    let size = dir_size(&p);
                    result.add(Action::new(
                        "caches",
                        "delete_tree",
                        p.to_string_lossy(),
                        size,
                        format!("unused >{}d", cfg.file_unused_days),
                    ));
                } else {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;
                        if !((meta.mtime() as f64) < cutoff && (meta.atime() as f64) < cutoff) {
                            continue;
                        }
                    }
                    #[cfg(not(unix))]
                    {
                        continue;
                    }
                    result.add(Action::new(
                        "caches",
                        "delete_file",
                        p.to_string_lossy(),
                        meta.len(),
                        format!("unused >{}d", cfg.file_unused_days),
                    ));
                }
            }
        }
    }

    if cfg.include_pkg_managers {
        let mut pkg_cmds: Vec<(&str, Vec<String>)> = Vec::new();
        if which_bin("uv").is_some() {
            pkg_cmds.push(("uv_cache_prune", vec!["uv".into(), "cache".into(), "prune".into()]));
        }
        if which_bin("pip").is_some() {
            pkg_cmds.push(("pip_cache_purge", vec!["pip".into(), "cache".into(), "purge".into()]));
        }
        if which_bin("npm").is_some() {
            pkg_cmds.push((
                "npm_cache_clean",
                vec!["npm".into(), "cache".into(), "clean".into(), "--force".into()],
            ));
        }
        if which_bin("cargo-cache").is_some() {
            pkg_cmds.push(("cargo_cache", vec!["cargo-cache".into(), "-a".into()]));
        }
        for (name, cmd) in pkg_cmds {
            result.add(
                Action::new("caches", "cache_cmd", name, 0, "package manager cache (opt-in)")
                    .with_command(cmd),
            );
        }
    }

    #[cfg(unix)]
    {
        if is_root() {
            if which_bin("apt-get").is_some() {
                result.add(
                    Action::new("caches", "cache_cmd", "apt_clean", 0, "apt clean")
                        .with_command(vec!["apt-get".into(), "clean".into()]),
                );
            }
            if which_bin("journalctl").is_some() {
                result.add(
                    Action::new(
                        "caches",
                        "cache_cmd",
                        "journal_vacuum",
                        0,
                        format!("vacuum {}", cfg.journal_vacuum),
                    )
                    .with_command(vec![
                        "journalctl".into(),
                        format!("--vacuum-time={}", cfg.journal_vacuum),
                    ]),
                );
            }
        }
    }

    let trash = cfg.home.join(".local/share/Trash");
    for sub in ["files", "info"] {
        let d = trash.join(sub);
        if !d.is_dir() {
            continue;
        }
        for f in iter_files(&d, false) {
            if file_age_ok_for_delete(&f, cutoff) {
                let sz = std::fs::metadata(&f).map(|m| m.len()).unwrap_or(0);
                result.add(Action::new(
                    "caches",
                    "delete_file",
                    f.to_string_lossy(),
                    sz,
                    "trash",
                ));
            }
        }
    }

    result
}

#[cfg(unix)]
fn is_root() -> bool {
    extern "C" {
        fn geteuid() -> u32;
    }
    unsafe { geteuid() == 0 }
}
