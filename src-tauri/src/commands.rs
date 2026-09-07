use disk_cleaner_engine::action::{Action, ApplySummary, PhaseResult};
use disk_cleaner_engine::config::Config;
use disk_cleaner_engine::disks::{list_disks as eng_disks, DiskInfo};
use disk_cleaner_engine::execute::{default_apply_jobs, execute_actions_parallel};
use disk_cleaner_engine::report::write_report;
use disk_cleaner_engine::run::run_phases_parallel;
use disk_cleaner_engine::trash::{
    delete_trash_item, empty_trash as eng_empty, list_trash as eng_trash, restore_trash_item,
    TrashItem,
};
use disk_cleaner_engine::util::format_bytes;
use serde::Serialize;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, State};

pub struct AppState {
    pub last_results: Mutex<Vec<PhaseResult>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            last_results: Mutex::new(Vec::new()),
        }
    }
}

#[derive(Clone, Serialize)]
pub struct ScanProgress {
    pub phase: String,
}

#[derive(Clone, Serialize)]
pub struct ScanPhaseDone {
    pub name: String,
    pub reclaimable_bytes: u64,
    pub action_count: usize,
    pub notes: Vec<String>,
}

#[derive(Clone, Serialize)]
pub struct ScanDone {
    pub results: Vec<PhaseResult>,
    pub total_bytes: u64,
}

#[derive(Clone, Serialize)]
pub struct ApplyDone {
    pub summary: ApplySummary,
}

#[tauri::command]
pub fn get_config() -> Config {
    Config::load()
}

#[tauri::command]
pub fn save_config(config: Config) -> Result<(), String> {
    config.save().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_disks() -> Vec<DiskInfo> {
    eng_disks()
}

#[tauri::command]
pub fn list_trash() -> Vec<TrashItem> {
    let cfg = Config::load();
    eng_trash(&cfg.home)
}

#[tauri::command]
pub fn restore_trash(item: TrashItem) -> Result<(), String> {
    restore_trash_item(&item).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_trash(item: TrashItem) -> Result<(), String> {
    delete_trash_item(&item).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn empty_trash() -> Result<u64, String> {
    let cfg = Config::load();
    eng_empty(&cfg.home).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn start_scan(
    app: AppHandle,
    state: State<'_, AppState>,
    mut config: Config,
) -> Result<ScanDone, String> {
    config.apply = false;
    let app2 = app.clone();
    let results = tauri::async_runtime::spawn_blocking(move || {
        run_phases_parallel(
            &config,
            |phase| {
                let _ = app2.emit(
                    "scan-phase-start",
                    ScanProgress {
                        phase: phase.to_string(),
                    },
                );
                let _ = app2.emit(
                    "scan-progress",
                    ScanProgress {
                        phase: phase.to_string(),
                    },
                );
            },
            |result| {
                let _ = app2.emit(
                    "scan-phase-done",
                    ScanPhaseDone {
                        name: result.name.clone(),
                        reclaimable_bytes: result.reclaimable_bytes,
                        action_count: result
                            .actions
                            .iter()
                            .filter(|a| a.kind != "rmdir_if_empty")
                            .count(),
                        notes: result.notes.clone(),
                    },
                );
            },
        )
    })
    .await
    .map_err(|e| e.to_string())?;

    let total_bytes = results.iter().map(|r| r.reclaimable_bytes).sum();
    *state.last_results.lock().map_err(|e| e.to_string())? = results.clone();
    let done = ScanDone {
        results: results.clone(),
        total_bytes,
    };
    let _ = app.emit("scan-done", done.clone());
    let cfg = Config::load();
    let _ = write_report(&cfg, &results, &[]);
    Ok(done)
}

#[tauri::command]
pub async fn apply_selected(
    app: AppHandle,
    config: Config,
    actions: Vec<Action>,
) -> Result<ApplyDone, String> {
    let mut cfg = config;
    cfg.apply = true;
    let use_trash = cfg.gui_use_trash;
    let jobs = default_apply_jobs();
    let app2 = app.clone();
    let summary = tauri::async_runtime::spawn_blocking(move || {
        // Cap event rate so the webview stays smooth on multi-thousand deletes.
        let last_emit = Mutex::new(Instant::now() - Duration::from_secs(1));
        let min_interval = Duration::from_millis(100);
        let (log, summary) = execute_actions_parallel(&actions, &cfg, use_trash, jobs, |p| {
            let force = p.done == p.total || p.done == 1;
            let mut guard = last_emit.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            if force || now.duration_since(*guard) >= min_interval {
                *guard = now;
                let _ = app2.emit("apply-progress", p);
            }
        });
        let _ = write_report(&cfg, &[], &log);
        summary
    })
    .await
    .map_err(|e| e.to_string())?;
    let done = ApplyDone {
        summary: summary.clone(),
    };
    let _ = app.emit("apply-done", done.clone());
    Ok(done)
}

#[tauri::command]
pub fn format_bytes_cmd(n: u64) -> String {
    format_bytes(n)
}

#[tauri::command]
pub fn open_path(path: String) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(&path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(&path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
