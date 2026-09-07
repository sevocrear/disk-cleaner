use crate::action::{ApplyRecord, PhaseResult};
use crate::config::Config;
use crate::VERSION;
use chrono::Utc;
use serde::Serialize;
use std::path::PathBuf;

#[derive(Serialize)]
struct ReportPayload {
    version: String,
    apply: bool,
    timestamp: String,
    phases: Vec<PhaseSummary>,
    actions: Vec<ApplyRecord>,
}

#[derive(Serialize)]
struct PhaseSummary {
    name: String,
    reclaimable_bytes: u64,
    notes: Vec<String>,
    action_count: usize,
}

pub fn write_report(cfg: &Config, results: &[PhaseResult], log: &[ApplyRecord]) -> std::io::Result<PathBuf> {
    let report_dir = cfg.report_dir.clone().unwrap_or_else(|| {
        cfg.home
            .join(".local/share/disk-cleaner/reports")
    });
    std::fs::create_dir_all(&report_dir)?;
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let path = report_dir.join(format!("report_{stamp}.json"));
    let payload = ReportPayload {
        version: VERSION.to_string(),
        apply: cfg.apply,
        timestamp: stamp,
        phases: results
            .iter()
            .map(|r| PhaseSummary {
                name: r.name.clone(),
                reclaimable_bytes: r.reclaimable_bytes,
                notes: r.notes.clone(),
                action_count: r.actions.len(),
            })
            .collect(),
        actions: log.to_vec(),
    };
    let text = serde_json::to_string_pretty(&payload)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&path, text)?;
    Ok(path)
}
