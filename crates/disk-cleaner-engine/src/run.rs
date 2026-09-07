use crate::action::PhaseResult;
use crate::config::Config;
use crate::phases::{
    phase_apps, phase_caches, phase_docker, phase_dupes, phase_files, phase_media,
};

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

pub fn run_phases(cfg: &Config) -> Vec<PhaseResult> {
    run_phases_with_progress(cfg, |_| {})
}

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
        let r = match name {
            "docker" => phase_docker(cfg),
            "caches" => phase_caches(cfg),
            "files" => phase_files(cfg),
            "apps" => phase_apps(cfg),
            "dupes" => phase_dupes(cfg),
            "media" => phase_media(cfg),
            _ => continue,
        };
        results.push(r);
    }
    results
}
