use crate::action::{Action, PhaseResult};
use crate::cmd::{run_command, which_bin};
use crate::config::Config;
use crate::safety::{matches_protect, resolve_protect_globs};
use crate::util::{
    days_to_seconds, dir_size, file_age_ok_for_delete, iter_files, newest_activity, now_ts,
};
use std::path::{Path, PathBuf};

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
        maybe_add_pkg_cache(
            &mut result,
            cfg,
            "uv",
            "uv_cache_prune",
            &["uv".into(), "cache".into(), "prune".into()],
            || {
                cmd_path_line(cfg, &["uv".into(), "cache".into(), "dir".into()])
                    .unwrap_or_else(|| cfg.home.join(".cache/uv"))
            },
        );
        maybe_add_pkg_cache(
            &mut result,
            cfg,
            "pip",
            "pip_cache_purge",
            &["pip".into(), "cache".into(), "purge".into()],
            || {
                cmd_path_line(cfg, &["pip".into(), "cache".into(), "dir".into()])
                    .unwrap_or_else(|| cfg.home.join(".cache/pip"))
            },
        );
        maybe_add_pkg_cache(
            &mut result,
            cfg,
            "npm",
            "npm_cache_clean",
            &[
                "npm".into(),
                "cache".into(),
                "clean".into(),
                "--force".into(),
            ],
            || {
                cmd_path_line(cfg, &["npm".into(), "config".into(), "get".into(), "cache".into()])
                    .unwrap_or_else(|| cfg.home.join(".npm"))
            },
        );
        if which_bin("cargo-cache").is_some() {
            let registry = cfg.home.join(".cargo/registry");
            let git = cfg.home.join(".cargo/git");
            let size = dir_size_if_exists(&registry) + dir_size_if_exists(&git);
            if size > 0 {
                result.add(
                    Action::new(
                        "caches",
                        "cache_cmd",
                        "cargo_cache",
                        size,
                        "package manager cache (opt-in)",
                    )
                    .with_command(vec!["cargo-cache".into(), "-a".into()]),
                );
            }
        }
    }

    #[cfg(unix)]
    {
        if is_root() {
            if which_bin("apt-get").is_some() {
                let archives = PathBuf::from("/var/cache/apt/archives");
                let size = dir_size_if_exists(&archives);
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

fn maybe_add_pkg_cache(
    result: &mut PhaseResult,
    cfg: &Config,
    bin: &str,
    name: &str,
    cmd: &[String],
    resolve_dir: impl FnOnce() -> PathBuf,
) {
    if which_bin(bin).is_none() {
        return;
    }
    let dir = resolve_dir();
    let size = dir_size_if_exists(&dir);
    if size == 0 {
        result.notes.push(format!("{name}: cache empty or missing; skipped"));
        return;
    }
    let _ = cfg;
    result.add(
        Action::new(
            "caches",
            "cache_cmd",
            name,
            size,
            "package manager cache (opt-in)",
        )
        .with_command(cmd.to_vec()),
    );
}

fn cmd_path_line(cfg: &Config, cmd: &[String]) -> Option<PathBuf> {
    let cp = run_command(cmd, cfg);
    if cp.status != 0 {
        return None;
    }
    let line = cp.stdout.lines().next()?.trim();
    if line.is_empty() {
        return None;
    }
    Some(PathBuf::from(line))
}

fn dir_size_if_exists(path: &Path) -> u64 {
    if path.is_dir() {
        dir_size(path)
    } else {
        0
    }
}

fn journal_disk_usage(cfg: &Config) -> Option<u64> {
    let cp = run_command(&["journalctl".into(), "--disk-usage".into()], cfg);
    if cp.status != 0 {
        return None;
    }
    // e.g. "Archived and active journals take up 128.0M in the file system."
    let re = regex::Regex::new(r"(?i)take up\s+([0-9.]+)\s*([KMGT]?i?B?)").ok()?;
    let caps = re.captures(&cp.stdout)?;
    let num = &caps[1];
    let unit = caps.get(2).map(|m| m.as_str()).unwrap_or("B");
    let unit = unit.replace('i', "").replace('I', "");
    parse_size_loose(num, &unit)
}

fn parse_size_loose(num: &str, unit: &str) -> Option<u64> {
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
