use crate::action::{Action, PhaseResult};
use crate::cmd::{run_command, which_bin};
use crate::config::Config;
use crate::safety::is_snap_protected;
use crate::util::{days_to_seconds, dir_size, format_bytes, newest_activity, now_ts};
use chrono::{TimeZone, Utc};
use std::collections::HashSet;
use std::path::PathBuf;

pub fn phase_apps(cfg: &Config) -> PhaseResult {
    let mut result = PhaseResult::new("apps");
    let cutoff = now_ts() - days_to_seconds(cfg.app_unused_days);
    let protect_apps = &cfg.protect_apps;

    if which_bin("snap").is_some() {
        let cp = run_command(&["snap".into(), "list".into()], cfg);
        if cp.status == 0 {
            for line in cp.stdout.lines().skip(1) {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.is_empty() {
                    continue;
                }
                let name = parts[0];
                let notes = parts.last().copied().unwrap_or("");
                if notes.to_lowercase().contains("base") {
                    continue;
                }
                if is_snap_protected(name, protect_apps) {
                    continue;
                }
                let user_data = cfg.home.join("snap").join(name);
                let mut act = if user_data.exists() {
                    newest_activity(&user_data)
                } else {
                    None
                };
                let var_common = PathBuf::from(format!("/var/snap/{name}/common"));
                if var_common.exists() {
                    if let Some(act2) = newest_activity(&var_common) {
                        act = Some(act.map_or(act2, |a| a.max(act2)));
                    }
                }
                let Some(act) = act else {
                    result.notes.push(format!(
                        "snap {name}: no user data; skipped (unknown last-used)"
                    ));
                    continue;
                };
                if act >= cutoff {
                    continue;
                }
                let size = if user_data.exists() {
                    dir_size(&user_data)
                } else {
                    0
                };
                let date = Utc
                    .timestamp_opt(act as i64, 0)
                    .single()
                    .map(|d| d.date_naive().to_string())
                    .unwrap_or_default();
                result.add(
                    Action::new(
                        "apps",
                        "snap_remove",
                        name,
                        size,
                        format!("last_used={date}"),
                    )
                    .with_command(vec!["snap".into(), "remove".into(), name.into()]),
                );
            }
        } else {
            result.notes.push("snap list failed".into());
        }
    }

    let appimage_dirs = [
        cfg.home.join("Applications"),
        cfg.home.join("AppImages"),
        cfg.home.clone(),
    ];
    let mut seen = HashSet::new();
    for d in &appimage_dirs {
        if !d.is_dir() {
            continue;
        }
        if let Ok(entries) = std::fs::read_dir(d) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !name.to_lowercase().ends_with(".appimage") {
                    continue;
                }
                let p = entry.path();
                if !seen.insert(p.clone()) {
                    continue;
                }
                let stem = p
                    .file_stem()
                    .map(|s| s.to_string_lossy().split('_').next().unwrap_or("").to_lowercase())
                    .unwrap_or_default();
                if protect_apps
                    .iter()
                    .any(|a| stem == a.to_lowercase() || name == *a)
                {
                    continue;
                }
                let Ok(meta) = entry.metadata() else { continue };
                #[cfg(unix)]
                let last = {
                    use std::os::unix::fs::MetadataExt;
                    (meta.atime() as f64).max(meta.mtime() as f64)
                };
                #[cfg(not(unix))]
                let last = 0.0;
                if last >= cutoff {
                    continue;
                }
                let date = Utc
                    .timestamp_opt(last as i64, 0)
                    .single()
                    .map(|d| d.date_naive().to_string())
                    .unwrap_or_default();
                result.add(Action::new(
                    "apps",
                    "delete_file",
                    p.to_string_lossy(),
                    meta.len(),
                    format!("AppImage last_used={date}"),
                ));
            }
        }
    }

    if which_bin("flatpak").is_some() {
        let cp = run_command(
            &[
                "flatpak".into(),
                "list".into(),
                "--app".into(),
                "--columns=application".into(),
            ],
            cfg,
        );
        if cp.status == 0 {
            for line in cp.stdout.lines() {
                let app_id = line.trim();
                if app_id.is_empty() || protect_apps.iter().any(|a| a == app_id) {
                    continue;
                }
                let data = cfg.home.join(".var/app").join(app_id);
                let Some(act) = (if data.exists() {
                    newest_activity(&data)
                } else {
                    None
                }) else {
                    continue;
                };
                if act >= cutoff {
                    continue;
                }
                let size = if data.exists() { dir_size(&data) } else { 0 };
                let date = Utc
                    .timestamp_opt(act as i64, 0)
                    .single()
                    .map(|d| d.date_naive().to_string())
                    .unwrap_or_default();
                result.add(
                    Action::new(
                        "apps",
                        "flatpak_uninstall",
                        app_id,
                        size,
                        format!("last_used={date}"),
                    )
                    .with_command(vec![
                        "flatpak".into(),
                        "uninstall".into(),
                        "-y".into(),
                        app_id.into(),
                    ]),
                );
            }
        }
    }

    if cfg.include_opt_apps {
        let opt = PathBuf::from("/opt");
        if opt.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&opt) {
                for entry in entries.flatten() {
                    if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        continue;
                    }
                    let p = entry.path();
                    let act = newest_activity(&p);
                    let size = dir_size(&p);
                    let mut detail = "report-only".to_string();
                    if let Some(act) = act {
                        let date = Utc
                            .timestamp_opt(act as i64, 0)
                            .single()
                            .map(|d| d.date_naive().to_string())
                            .unwrap_or_default();
                        detail.push_str(&format!(" last_used={date}"));
                        if act < cutoff {
                            detail.push_str(" UNUSED_CANDIDATE");
                        }
                    }
                    result.notes.push(format!(
                        "/opt {}: {} {detail}",
                        entry.file_name().to_string_lossy(),
                        format_bytes(size)
                    ));
                }
            }
        }
    }

    result
}
