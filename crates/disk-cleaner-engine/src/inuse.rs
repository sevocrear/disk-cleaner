//! Which running processes use a path (cwd, executable, open files, mapped libs).
//!
//! Reads `/proc/<pid>/{cwd,exe,fd/*,maps}` for every process we are allowed to
//! inspect — normally all processes of the current user, which is what matters
//! before deleting something under `$HOME` (e.g. a cache a running tool needs).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Max processes reported per path; enough to explain, small enough to show.
const MAX_PROCS_PER_PATH: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessUse {
    pub pid: u32,
    pub name: String,
    /// `cwd`, `exe`, `open`, or `mapped`.
    pub how: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InUse {
    pub path: String,
    pub processes: Vec<ProcessUse>,
}

/// One path a process references.
#[derive(Debug, Clone)]
pub struct ProcRef {
    pub pid: u32,
    pub name: String,
    pub how: &'static str,
    pub path: PathBuf,
}

fn proc_name(pid_dir: &Path) -> String {
    std::fs::read_to_string(pid_dir.join("comm"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// Every path referenced by readable processes, excluding this process.
pub fn snapshot_proc_refs() -> Vec<ProcRef> {
    snapshot_from(Path::new("/proc"), std::process::id(), |_| true)
}

/// Walk `/proc` in parallel, keeping only references accepted by `keep`.
/// Filtering here (instead of after) avoids materializing ~40k paths on a desktop.
fn snapshot_from(
    proc_root: &Path,
    self_pid: u32,
    keep: impl Fn(&Path) -> bool + Sync,
) -> Vec<ProcRef> {
    use rayon::prelude::*;
    let Ok(entries) = std::fs::read_dir(proc_root) else {
        return Vec::new();
    };
    let pids: Vec<(u32, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let pid = e.file_name().to_str()?.parse::<u32>().ok()?;
            (pid != self_pid).then(|| (pid, e.path()))
        })
        .collect();
    pids.into_par_iter()
        .flat_map_iter(|(pid, dir)| {
            let mut found: Vec<(&'static str, PathBuf)> = Vec::new();
            let mut push = |how: &'static str, path: PathBuf| {
                let path = strip_deleted(path);
                if keep(&path) {
                    found.push((how, path));
                }
            };
            if let Ok(p) = std::fs::read_link(dir.join("cwd")) {
                push("cwd", p);
            }
            if let Ok(p) = std::fs::read_link(dir.join("exe")) {
                push("exe", p);
            }
            if let Ok(fds) = std::fs::read_dir(dir.join("fd")) {
                for fd in fds.flatten() {
                    if let Ok(p) = std::fs::read_link(fd.path()) {
                        if p.is_absolute() {
                            push("open", p);
                        }
                    }
                }
            }
            if let Ok(maps) = std::fs::read_to_string(dir.join("maps")) {
                let mut last = "";
                for line in maps.lines() {
                    // address perms offset dev inode pathname
                    let Some(path) = line.splitn(6, char::is_whitespace).nth(5) else {
                        continue;
                    };
                    let path = path.trim();
                    // Consecutive mappings of one file repeat the path.
                    if path.starts_with('/') && path != last {
                        last = path;
                        push("mapped", PathBuf::from(path));
                    }
                }
            }
            let name = if found.is_empty() {
                String::new()
            } else {
                proc_name(&dir)
            };
            found.into_iter().map(move |(how, path)| ProcRef {
                pid,
                name: name.clone(),
                how,
                path,
            })
        })
        .collect()
}

fn strip_deleted(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_suffix(" (deleted)") {
        Some(rest) => PathBuf::from(rest),
        None => p,
    }
}

/// Match targets against a process snapshot. Only targets with users are returned.
pub fn match_in_use(targets: &[PathBuf], refs: &[ProcRef]) -> Vec<InUse> {
    let mut out = Vec::new();
    for t in targets {
        // pid → (name, strongest how); stable order by pid.
        let mut users: BTreeMap<u32, (String, &'static str)> = BTreeMap::new();
        for r in refs {
            if r.path.starts_with(t) {
                let e = users.entry(r.pid).or_insert((r.name.clone(), r.how));
                if how_rank(r.how) < how_rank(e.1) {
                    e.1 = r.how;
                }
            }
        }
        if users.is_empty() {
            continue;
        }
        out.push(InUse {
            path: t.to_string_lossy().into_owned(),
            processes: users
                .into_iter()
                .take(MAX_PROCS_PER_PATH)
                .map(|(pid, (name, how))| ProcessUse {
                    pid,
                    name,
                    how: how.to_string(),
                })
                .collect(),
        });
    }
    out
}

fn how_rank(how: &str) -> u8 {
    match how {
        "exe" => 0,
        "cwd" => 1,
        "open" => 2,
        _ => 3,
    }
}

/// Convenience: snapshot `/proc` once and match all targets.
pub fn find_in_use(targets: &[PathBuf]) -> Vec<InUse> {
    if targets.is_empty() {
        return Vec::new();
    }
    let refs = snapshot_from(Path::new("/proc"), std::process::id(), |p| {
        targets.iter().any(|t| p.starts_with(t))
    });
    match_in_use(targets, &refs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn r(pid: u32, how: &'static str, path: &str) -> ProcRef {
        ProcRef {
            pid,
            name: format!("p{pid}"),
            how,
            path: PathBuf::from(path),
        }
    }

    #[test]
    fn match_is_component_prefix_and_ranked() {
        let refs = vec![
            r(10, "mapped", "/home/u/.cache/uv/env/lib.so"),
            r(10, "open", "/home/u/.cache/uv/lock"),
            r(11, "cwd", "/home/u/.cache/uvx"),
            r(12, "exe", "/home/u/.cache/uv/bin/tool"),
        ];
        let got = match_in_use(
            &[
                PathBuf::from("/home/u/.cache/uv"),
                PathBuf::from("/home/u/free"),
            ],
            &refs,
        );
        assert_eq!(got.len(), 1, "unused target omitted");
        let procs = &got[0].processes;
        // /home/u/.cache/uvx is not inside /home/u/.cache/uv
        assert_eq!(
            procs.iter().map(|p| p.pid).collect::<Vec<_>>(),
            vec![10, 12]
        );
        assert_eq!(procs[0].how, "open");
        assert_eq!(procs[1].how, "exe");
    }

    #[test]
    fn deleted_suffix_is_stripped() {
        assert_eq!(
            strip_deleted(PathBuf::from("/tmp/x (deleted)")),
            PathBuf::from("/tmp/x")
        );
        assert_eq!(
            strip_deleted(PathBuf::from("/tmp/y")),
            PathBuf::from("/tmp/y")
        );
    }

    #[test]
    fn detects_child_process_cwd_and_open_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("busy");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("held.txt");
        std::fs::write(&file, b"x").unwrap();
        // Keep the file open on fd 3 while sleeping inside the directory.
        let mut child = Command::new("bash")
            .arg("-c")
            .arg("exec 3<held.txt; sleep 30")
            .current_dir(&dir)
            .spawn()
            .unwrap();
        let pid = child.id();
        let mut found = Vec::new();
        for _ in 0..50 {
            found = find_in_use(&[dir.clone(), tmp.path().join("idle")]);
            let ok = found
                .iter()
                .any(|u| u.processes.iter().any(|p| p.pid == pid));
            if ok {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].path, dir.to_string_lossy());
        let p = found[0].processes.iter().find(|p| p.pid == pid).unwrap();
        assert!(p.how == "cwd" || p.how == "open", "{p:?}");
        assert!(!p.name.is_empty());
    }
}
