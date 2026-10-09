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
use disk_cleaner_engine::docker_inventory::{
    builder_prune_command, image_remove_actions, inventory as docker_inv, DockerInventory,
};
use disk_cleaner_engine::docker_track::{self, TrackerStatus};
use disk_cleaner_engine::inuse::{find_in_use, InUse};
use disk_cleaner_engine::space::{space_accounting, SpaceAccounting};
use disk_cleaner_engine::tree::{
    scan_tree, DeleteOutcome, DirListing, ScanOptions, SizeTree, TreeScanProgress,
};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, State};

pub struct AppState {
    pub last_results: Mutex<Vec<PhaseResult>>,
    /// Overview size tree (scanned once, browsed and edited in memory).
    pub overview: Arc<Mutex<Option<SizeTree>>>,
    pub overview_cancel: Arc<AtomicBool>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            last_results: Mutex::new(Vec::new()),
            overview: Arc::new(Mutex::new(None)),
            overview_cancel: Arc::new(AtomicBool::new(false)),
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

// ---------- Overview (ncdu-style browser) ----------

#[derive(Clone, Serialize)]
pub struct OverviewScanDone {
    pub root: String,
    pub total_bytes: u64,
    pub cancelled: bool,
    pub unreadable_dirs: u64,
    pub accounting: Option<SpaceAccounting>,
    pub listing: Option<DirListing>,
}

#[derive(Clone, Serialize)]
pub struct OverviewDeleteDone {
    pub outcome: DeleteOutcome,
}

#[derive(Clone, Serialize)]
pub struct OverviewDeleteProgress {
    pub done: usize,
    pub total: usize,
    pub path: String,
}

fn lock_err<T>(e: std::sync::PoisonError<T>) -> String {
    e.to_string()
}

#[tauri::command]
pub async fn overview_scan(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
    cross_fs: bool,
) -> Result<OverviewScanDone, String> {
    let root = PathBuf::from(&path)
        .canonicalize()
        .map_err(|e| format!("{path}: {e}"))?;
    if !root.is_dir() {
        return Err(format!("not a directory: {}", root.display()));
    }
    let cancel = state.overview_cancel.clone();
    cancel.store(false, Ordering::Relaxed);
    let slot = state.overview.clone();
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let opts = ScanOptions {
            cross_fs,
            ..Default::default()
        };
        let last_emit = Mutex::new(Instant::now() - Duration::from_secs(1));
        let tree = scan_tree(&root, &opts, &cancel, |p: TreeScanProgress| {
            let mut guard = last_emit.lock().unwrap_or_else(|e| e.into_inner());
            if guard.elapsed() >= Duration::from_millis(150) {
                *guard = Instant::now();
                let _ = app2.emit("overview-progress", p);
            }
        });
        let cfg = Config::load();
        let accounting = if tree.cancelled {
            None
        } else {
            space_accounting(&root, tree.total_bytes(), tree.unreadable_dirs, &cfg)
        };
        let done = OverviewScanDone {
            root: root.to_string_lossy().into_owned(),
            total_bytes: tree.total_bytes(),
            cancelled: tree.cancelled,
            unreadable_dirs: tree.unreadable_dirs,
            accounting,
            listing: tree.listing(&root, &cfg),
        };
        *slot.lock().map_err(lock_err)? = Some(tree);
        Ok(done)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn overview_cancel(state: State<'_, AppState>) {
    state.overview_cancel.store(true, Ordering::Relaxed);
}

#[tauri::command]
pub fn overview_list(state: State<'_, AppState>, path: String) -> Result<DirListing, String> {
    let guard = state.overview.lock().map_err(lock_err)?;
    let tree = guard.as_ref().ok_or("nothing scanned yet")?;
    tree.listing(&PathBuf::from(&path), &Config::load())
        .ok_or_else(|| format!("{path} is not in the scanned tree"))
}

#[tauri::command]
pub async fn overview_in_use(paths: Vec<String>) -> Result<Vec<InUse>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let targets: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
        find_in_use(&targets)
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn overview_delete(
    app: AppHandle,
    state: State<'_, AppState>,
    paths: Vec<String>,
    use_trash: bool,
) -> Result<OverviewDeleteDone, String> {
    let slot = state.overview.clone();
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let cfg = Config::load();
        let mut guard = slot.lock().map_err(lock_err)?;
        let tree = guard.as_mut().ok_or("nothing scanned yet")?;
        let targets: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
        let outcome = tree.delete_paths(&targets, &cfg, use_trash, |done, total, path| {
            let _ = app2.emit(
                "overview-delete-progress",
                OverviewDeleteProgress {
                    done,
                    total,
                    path: path.to_string(),
                },
            );
        });
        Ok(OverviewDeleteDone { outcome })
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------- Docker ----------

#[tauri::command]
pub async fn docker_inventory(days: u32) -> Result<DockerInventory, String> {
    tauri::async_runtime::spawn_blocking(move || docker_inv(&Config::load(), days))
        .await
        .map_err(|e| e.to_string())
}

fn run_docker_actions(actions: Vec<Action>) -> ApplySummary {
    let mut cfg = Config::load();
    cfg.apply = true;
    let (log, summary) = execute_actions_parallel(&actions, &cfg, false, 1, |_| {});
    let _ = write_report(&cfg, &[], &log);
    summary
}

#[tauri::command]
pub async fn docker_remove_images(ids: Vec<String>) -> Result<ApplySummary, String> {
    tauri::async_runtime::spawn_blocking(move || {
        // Re-read the inventory so container usage is current, not from page load.
        let cfg = Config::load();
        let inv = docker_inv(&cfg, cfg.docker_unused_days);
        if let Some(e) = inv.error {
            return Err(e);
        }
        let actions = image_remove_actions(&inv.images, &ids)?;
        Ok(run_docker_actions(actions))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn docker_prune_build_cache(days: u32, estimate_bytes: u64) -> Result<ApplySummary, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let action = Action::new(
            "docker",
            "docker_cmd",
            "builder_prune",
            estimate_bytes,
            format!("build cache unused >{days}d"),
        )
        .with_command(builder_prune_command(days));
        run_docker_actions(vec![action])
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn docker_tracker_install() -> Result<TrackerStatus, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let bin = docker_track::find_cli_binary()
            .ok_or("disk-cleaner CLI not found next to the app or on PATH")?;
        docker_track::install(&bin)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn docker_tracker_uninstall() -> Result<TrackerStatus, String> {
    tauri::async_runtime::spawn_blocking(docker_track::uninstall)
        .await
        .map_err(|e| e.to_string())?
}
