use crate::action::{Action, PhaseResult};
use crate::cmd::{run_command, which_bin};
use crate::config::Config;
use crate::safety::{matches_protect, resolve_protect_globs};
use crate::util::{
    days_to_seconds, dir_size, file_age_ok_for_delete, iter_files, newest_activity, now_ts,
};
use std::path::PathBuf;

pub fn phase_caches(cfg: &Config) -> PhaseResult {
    let mut result = PhaseResult::new("caches");
    let protect = resolve_protect_globs(cfg);
    let cutoff = now_ts() - days_to_seconds(cfg.file_unused_days);
    let cache_root = cfg.home.join(".cache");

    // Age-based cleanup of ~/.cache (covers pip/uv/npm dirs without requiring those CLIs).
    let _ = cfg.include_pkg_managers;

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

    #[cfg(unix)]
    {
        if is_root() {
            if which_bin("apt-get").is_some() {
                let archives = PathBuf::from("/var/cache/apt/archives");
                let size = if archives.is_dir() {
                    dir_size(&archives)
                } else {
                    0
                };
                if size > 0 {
                    result.add(
                        Action::new("caches", "cache_cmd", "apt_clean", size, "apt clean")
                            .with_command(vec!["apt-get".into(), "clean".into()]),
                    );
                }
            }
            if which_bin("journalctl").is_some() {
                if let Some(size) = journal_disk_usage(cfg) {
                    if size > 0 {
                        result.add(
                            Action::new(
                                "caches",
                                "cache_cmd",
                                "journal_vacuum",
                                size,
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

fn journal_disk_usage(cfg: &Config) -> Option<u64> {
    let cp = run_command(&["journalctl".into(), "--disk-usage".into()], cfg);
    if cp.status != 0 {
        return None;
    }
    let re = regex::Regex::new(r"(?i)take up\s+([0-9.]+)\s*([KMGT]?i?B?)").ok()?;
    let caps = re.captures(&cp.stdout)?;
    let num = &caps[1];
    let unit = caps.get(2).map(|m| m.as_str()).unwrap_or("B");
    let unit = unit.replace('i', "").replace('I', "");
    let u = unit.trim().to_uppercase().replace('B', "");
    let token = if u.is_empty() {
        num.to_string()
    } else {
        format!("{num}{u}")
    };
    crate::util::parse_size(&token).ok()
}

#[cfg(unix)]
fn is_root() -> bool {
    extern "C" {
        fn geteuid() -> u32;
    }
    unsafe { geteuid() == 0 }
}
