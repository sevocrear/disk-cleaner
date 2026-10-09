//! Optional Docker usage tracker.
//!
//! `disk-cleaner docker-track` follows `docker events` and records when each image
//! was used to create or start a container. Docker itself forgets this once the
//! container is removed (`docker run --rm`), so the record makes "unused for N days"
//! exact. It runs as a systemd user service installed from the CLI or the GUI.

use crate::cmd::run_command;
use crate::config::Config;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const UNIT_NAME: &str = "disk-cleaner-docker-track.service";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageStore {
    /// When tracking started (unix seconds).
    #[serde(default)]
    pub since: Option<i64>,
    /// Image id (`sha256:…`) or reference (`name:tag`) → last use (unix seconds).
    #[serde(default)]
    pub images: BTreeMap<String, i64>,
}

impl UsageStore {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// Atomic write (tmp + rename) so a reader never sees a torn file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(&tmp, text)?;
        std::fs::rename(tmp, path)
    }

    /// Keep the newest timestamp per key. Returns true when something changed.
    pub fn record(&mut self, key: &str, ts: i64) -> bool {
        if key.is_empty() {
            return false;
        }
        let e = self.images.entry(key.to_string()).or_insert(i64::MIN);
        if ts > *e {
            *e = ts;
            true
        } else {
            false
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrackerStatus {
    /// systemd user unit file exists.
    pub installed: bool,
    /// systemd reports the unit active.
    pub active: bool,
    pub store_path: String,
    pub tracked_images: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<i64>,
}

pub fn store_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("disk-cleaner")
        .join("docker-usage.json")
}

pub fn unit_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("systemd/user")
        .join(UNIT_NAME)
}

/// One `docker events --format '{{json .}}'` line → (container id, image ref, unix time).
pub fn parse_event_line(line: &str) -> Option<(String, String, i64)> {
    #[derive(Deserialize)]
    struct Actor {
        #[serde(rename = "ID", default)]
        id: String,
        #[serde(rename = "Attributes", default)]
        attributes: BTreeMap<String, String>,
    }
    #[derive(Deserialize)]
    struct Ev {
        #[serde(rename = "Type", default)]
        ty: String,
        #[serde(rename = "Action", default)]
        action: String,
        #[serde(rename = "Actor")]
        actor: Actor,
        #[serde(default)]
        time: i64,
    }
    let ev: Ev = serde_json::from_str(line.trim()).ok()?;
    if ev.ty != "container" || !matches!(ev.action.as_str(), "create" | "start") {
        return None;
    }
    let image = ev
        .actor
        .attributes
        .get("image")
        .cloned()
        .unwrap_or_default();
    Some((ev.actor.id, image, ev.time))
}

/// Apply one event line to the store. `resolve` maps a container id to its image id.
pub fn handle_event(
    store: &mut UsageStore,
    line: &str,
    resolve: impl Fn(&str) -> Option<String>,
) -> bool {
    let Some((cid, image, ts)) = parse_event_line(line) else {
        return false;
    };
    let mut changed = store.record(&image, ts);
    if let Some(id) = resolve(&cid) {
        changed |= store.record(&id, ts);
    }
    changed
}

fn resolve_image_id(cfg: &Config, cid: &str) -> Option<String> {
    let cp = run_command(
        &[
            "docker".into(),
            "inspect".into(),
            "--format".into(),
            "{{.Image}}".into(),
            cid.into(),
        ],
        cfg,
    );
    let id = cp.stdout.trim();
    (cp.status == 0 && !id.is_empty()).then(|| id.to_string())
}

/// Follow `docker events` forever, writing to `path`. Reconnects when Docker restarts.
pub fn run_tracker(path: &Path, cfg: &Config) -> ! {
    let mut store = UsageStore::load(path);
    if store.since.is_none() {
        store.since = Some(chrono::Utc::now().timestamp());
        let _ = store.save(path);
    }
    loop {
        let child = Command::new("docker")
            .args([
                "events",
                "--filter",
                "type=container",
                "--filter",
                "event=create",
                "--filter",
                "event=start",
                "--format",
                "{{json .}}",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        if let Ok(mut child) = child {
            if let Some(out) = child.stdout.take() {
                for line in BufReader::new(out).lines().map_while(Result::ok) {
                    if handle_event(&mut store, &line, |cid| resolve_image_id(cfg, cid)) {
                        if let Err(e) = store.save(path) {
                            eprintln!("docker-track: save failed: {e}");
                        }
                    }
                }
            }
            let _ = child.wait();
        }
        eprintln!("docker-track: docker events ended; retrying in 10s");
        std::thread::sleep(std::time::Duration::from_secs(10));
    }
}

pub fn unit_contents(bin: &Path) -> String {
    format!(
        "[Unit]\n\
         Description=Disk Cleaner: record Docker image usage\n\
         After=default.target\n\
         \n\
         [Service]\n\
         ExecStart={} docker-track\n\
         Restart=on-failure\n\
         RestartSec=30\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        bin.display()
    )
}

fn systemctl(args: &[&str]) -> Result<String, String> {
    let out = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .map_err(|e| format!("systemctl: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() {
        Ok(stdout)
    } else {
        Err(format!(
            "systemctl --user {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

pub fn status() -> TrackerStatus {
    let store = UsageStore::load(&store_path());
    TrackerStatus {
        installed: unit_path().is_file(),
        active: systemctl(&["is-active", UNIT_NAME])
            .map(|s| s == "active")
            .unwrap_or(false),
        store_path: store_path().to_string_lossy().into_owned(),
        tracked_images: store.images.len(),
        since: store.since,
    }
}

/// Write the unit for `bin` (the `disk-cleaner` CLI) and enable it now.
pub fn install(bin: &Path) -> Result<TrackerStatus, String> {
    if !bin.is_file() {
        return Err(format!("CLI binary not found: {}", bin.display()));
    }
    let unit = unit_path();
    if let Some(dir) = unit.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&unit, unit_contents(bin)).map_err(|e| e.to_string())?;
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", "--now", UNIT_NAME])?;
    Ok(status())
}

pub fn uninstall() -> Result<TrackerStatus, String> {
    let _ = systemctl(&["disable", "--now", UNIT_NAME]);
    let unit = unit_path();
    if unit.exists() {
        std::fs::remove_file(&unit).map_err(|e| e.to_string())?;
    }
    let _ = systemctl(&["daemon-reload"]);
    Ok(status())
}

/// Locate the `disk-cleaner` CLI: next to the running binary, else on PATH.
pub fn find_cli_binary() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join("disk-cleaner");
            if sibling.is_file() {
                return Some(sibling);
            }
        }
    }
    which::which("disk-cleaner").ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const START: &str = r#"{"Type":"container","Action":"start","Actor":{"ID":"072a498e","Attributes":{"image":"hello-world","name":"dc-evtest"}},"scope":"local","time":1791561463,"timeNano":1791561463105593791}"#;

    #[test]
    fn parses_real_event_lines() {
        assert_eq!(
            parse_event_line(START),
            Some(("072a498e".into(), "hello-world".into(), 1_791_561_463))
        );
        let die = START.replace("\"start\"", "\"die\"");
        assert_eq!(parse_event_line(&die), None);
        let image_ev = START.replace("\"container\"", "\"image\"");
        assert_eq!(parse_event_line(&image_ev), None);
        assert_eq!(parse_event_line("garbage"), None);
    }

    #[test]
    fn handle_event_records_ref_and_resolved_id() {
        let mut store = UsageStore::default();
        assert!(handle_event(&mut store, START, |cid| {
            assert_eq!(cid, "072a498e");
            Some("sha256:5e23".into())
        }));
        assert_eq!(store.images.get("hello-world"), Some(&1_791_561_463));
        assert_eq!(store.images.get("sha256:5e23"), Some(&1_791_561_463));
        // Same event again: nothing newer.
        assert!(!handle_event(&mut store, START, |_| Some(
            "sha256:5e23".into()
        )));
        // Older timestamps never overwrite newer ones.
        assert!(!store.record("hello-world", 5));
        assert!(!store.record("", 9_999_999_999));
    }

    #[test]
    fn store_roundtrip_is_atomic_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested/usage.json");
        let mut s = UsageStore::default();
        s.since = Some(10);
        s.record("img:1", 20);
        s.save(&path).unwrap();
        assert!(!path.with_extension("json.tmp").exists());
        let back = UsageStore::load(&path);
        assert_eq!(back.since, Some(10));
        assert_eq!(back.images.get("img:1"), Some(&20));
        assert!(UsageStore::load(&tmp.path().join("missing.json"))
            .images
            .is_empty());
    }

    #[test]
    fn unit_runs_cli_subcommand() {
        let u = unit_contents(Path::new("/home/u/.local/bin/disk-cleaner"));
        assert!(u.contains("ExecStart=/home/u/.local/bin/disk-cleaner docker-track\n"));
        assert!(u.contains("WantedBy=default.target"));
    }
}
