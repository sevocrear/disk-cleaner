use crate::action::PhaseResult;
use crate::config::Config;
use crate::phases::{
    phase_apps, phase_caches, phase_docker, phase_dupes, phase_files, phase_media,
};
use std::sync::Mutex;

pub const PHASE_ORDER: &[&str] = &["docker", "caches", "files", "apps", "dupes", "media"];

pub fn should_run_phase(name: &str, cfg: &Config) -> bool {
    if let Some(ref only) = cfg.only {
        return only == name;
    }
    match name {
        "docker" => !cfg.skip_docker,
        "caches" => !cfg.skip_caches,
        "files" => !cfg.skip_files,
        "apps" => !cfg.skip_apps,
        "dupes" => !cfg.skip_dupes,
        "media" => !cfg.skip_media,
        _ => false,
    }
}

fn run_one_phase(name: &str, cfg: &Config) -> PhaseResult {
    match name {
        "docker" => phase_docker(cfg),
        "caches" => phase_caches(cfg),
        "files" => phase_files(cfg),
        "apps" => phase_apps(cfg),
        "dupes" => phase_dupes(cfg),
        "media" => phase_media(cfg),
        _ => PhaseResult::new(name),
    }
}

pub fn run_phases(cfg: &Config) -> Vec<PhaseResult> {
    run_phases_with_progress(cfg, |_| {})
}

/// Sequential scan (CLI-friendly ordered output).
pub fn run_phases_with_progress<F>(cfg: &Config, mut on_progress: F) -> Vec<PhaseResult>
where
    F: FnMut(&str),
{
    let mut results = Vec::new();
    for &name in PHASE_ORDER {
        if !should_run_phase(name, cfg) {
            continue;
        }
        on_progress(name);
        results.push(run_one_phase(name, cfg));
    }
    results
}

/// Parallel per-category scan for the GUI. Emits start/done callbacks from worker threads.
pub fn run_phases_parallel<S, D>(cfg: &Config, on_start: S, on_done: D) -> Vec<PhaseResult>
where
    S: Fn(&str) + Sync,
    D: Fn(PhaseResult) + Sync,
{
    let collected: Mutex<Vec<PhaseResult>> = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for &name in PHASE_ORDER {
            if !should_run_phase(name, cfg) {
                continue;
            }
            let on_start = &on_start;
            let on_done = &on_done;
            let collected = &collected;
            scope.spawn(move || {
                on_start(name);
                let result = run_one_phase(name, cfg);
                on_done(result.clone());
                if let Ok(mut guard) = collected.lock() {
                    guard.push(result);
                }
            });
        }
    });
    let mut results = collected.into_inner().unwrap_or_default();
    results.sort_by_key(|r| {
        PHASE_ORDER
            .iter()
            .position(|n| *n == r.name.as_str())
            .unwrap_or(usize::MAX)
    });
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn test_cfg() -> Config {
        let mut cfg = Config::default();
        cfg.home = PathBuf::from("/tmp/disk-cleaner-test-nonexistent");
        cfg.skip_docker = true;
        cfg.skip_caches = true;
        cfg.skip_files = false;
        cfg.skip_apps = true;
        cfg.skip_dupes = true;
        cfg.skip_media = true;
        cfg.cmd_timeout_sec = 2;
        cfg
    }

    #[test]
    fn parallel_runs_enabled_phases_once() {
        let cfg = test_cfg();
        let starts = AtomicUsize::new(0);
        let dones = AtomicUsize::new(0);
        let results = run_phases_parallel(
            &cfg,
            |_| {
                starts.fetch_add(1, Ordering::SeqCst);
            },
            |_| {
                dones.fetch_add(1, Ordering::SeqCst);
            },
        );
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        assert_eq!(dones.load(Ordering::SeqCst), 1);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "files");
    }
}
