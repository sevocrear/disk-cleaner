use regex::Regex;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::safety::path_is_denied;

pub fn now_ts() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub fn days_to_seconds(days: u32) -> f64 {
    days as f64 * 86400.0
}

pub fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim().replace(' ', "");
    if s.is_empty() {
        return Err("empty size".into());
    }
    let re = Regex::new(r"(?i)^(\d+(?:\.\d+)?)([kmgt]?b?)?$").unwrap();
    let caps = re
        .captures(&s)
        .ok_or_else(|| format!("invalid size: {s}"))?;
    let num: f64 = caps[1].parse().map_err(|e| format!("{e}"))?;
    let unit = caps
        .get(2)
        .map(|m| m.as_str().to_uppercase())
        .unwrap_or_default();
    let unit = unit.trim_end_matches('B');
    let mult = match unit {
        "" => 1u64,
        "K" => 1024,
        "M" => 1024 * 1024,
        "G" => 1024 * 1024 * 1024,
        "T" => 1024u64.pow(4),
        _ => return Err(format!("invalid size: {s}")),
    };
    Ok((num * mult as f64) as u64)
}

pub fn format_bytes(n: u64) -> String {
    let n = n as f64;
    for (unit, div) in [
        ("TB", 1024f64.powi(4)),
        ("GB", 1024f64.powi(3)),
        ("MB", 1024f64.powi(2)),
        ("KB", 1024f64),
    ] {
        if n >= div {
            return format!("{:.1}{unit}", n / div);
        }
    }
    format!("{n}B")
}

pub fn docker_until_filter(days: u32) -> String {
    let hours = (days * 24).max(1);
    format!("until={hours}h")
}

/// Parse a Docker reclaimable field like `1.234GB (50%)` or `0B`.
pub fn parse_docker_reclaimable_field(field: &str) -> Option<u64> {
    let token = field.trim().split_whitespace().next()?;
    parse_size(token).ok()
}

/// Parse `docker system df --format '{{.Type}}\t{{.Reclaimable}}'` into type → bytes.
pub fn parse_docker_system_df(stdout: &str) -> std::collections::HashMap<String, u64> {
    let mut map = std::collections::HashMap::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((ty, reclaim)) = line.split_once('\t') else {
            continue;
        };
        if let Some(bytes) = parse_docker_reclaimable_field(reclaim) {
            map.insert(ty.trim().to_string(), bytes);
        }
    }
    map
}

pub fn iter_files(root: &Path, cross_fs: bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !root.exists() {
        return out;
    }
    let root_dev = match std::fs::metadata(root) {
        Ok(m) => file_dev(&m),
        Err(_) => return out,
    };
    let mut stack = vec![root.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = match std::fs::read_dir(&current) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                if path_is_denied(&path) {
                    continue;
                }
                if !cross_fs {
                    if let Ok(meta) = std::fs::symlink_metadata(&path) {
                        if file_dev(&meta) != root_dev {
                            continue;
                        }
                    }
                }
                stack.push(path);
            } else if ft.is_file() {
                out.push(path);
            }
        }
    }
    out
}

#[cfg(unix)]
fn file_dev(meta: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.dev()
}

#[cfg(not(unix))]
fn file_dev(_meta: &std::fs::Metadata) -> u64 {
    0
}

pub fn file_age_ok_for_delete(path: &Path, cutoff: f64) -> bool {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return false,
    };
    let mtime = meta_time(&meta, true);
    let atime = meta_time(&meta, false);
    mtime < cutoff && atime < cutoff
}

fn meta_time(meta: &std::fs::Metadata, mtime: bool) -> f64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if mtime {
            meta.mtime() as f64
        } else {
            meta.atime() as f64
        }
    }
    #[cfg(not(unix))]
    {
        let _ = mtime;
        meta.modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)
    }
}

pub fn dir_size(path: &Path) -> u64 {
    if path.is_file() {
        return std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    }
    iter_files(path, false)
        .into_iter()
        .map(|f| std::fs::metadata(f).map(|m| m.len()).unwrap_or(0))
        .sum()
}

pub fn newest_activity(path: &Path) -> Option<f64> {
    if !path.exists() {
        return None;
    }
    if path.is_file() {
        let meta = std::fs::metadata(path).ok()?;
        return Some(meta_time(&meta, true).max(meta_time(&meta, false)));
    }
    let mut newest: Option<f64> = None;
    for f in iter_files(path, false) {
        if let Ok(meta) = std::fs::metadata(&f) {
            let t = meta_time(&meta, true).max(meta_time(&meta, false));
            newest = Some(newest.map_or(t, |n| n.max(t)));
        }
    }
    if newest.is_none() {
        if let Ok(meta) = std::fs::metadata(path) {
            return Some(meta_time(&meta, true).max(meta_time(&meta, false)));
        }
    }
    newest
}

pub fn hash_file(path: &Path, limit: Option<u64>) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 1024 * 1024];
    let mut remaining = limit;
    loop {
        let to_read = match remaining {
            Some(0) => break,
            Some(r) => (buf.len() as u64).min(r) as usize,
            None => buf.len(),
        };
        let n = file.read(&mut buf[..to_read])?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        if let Some(r) = remaining.as_mut() {
            *r = r.saturating_sub(n as u64);
        }
    }
    Ok(hex::encode(hasher.finalize()))
}

pub fn parse_reclaimed_bytes(text: &str) -> Option<u64> {
    if text.is_empty() {
        return None;
    }
    let re1 = Regex::new(r"(?i)(?:total reclaimed space|total):\s*([0-9.]+)\s*([KMGT])B?\b").ok()?;
    let re2 = Regex::new(r"(?i)(?:total reclaimed space|total):\s*([0-9.]+)\s*B\b").ok()?;
    for line in text.lines() {
        let line = line.trim();
        if let Some(c) = re1.captures(line) {
            if let Ok(v) = parse_size(&format!("{}{}", &c[1], &c[2])) {
                return Some(v);
            }
        }
        if let Some(c) = re2.captures(line) {
            if let Ok(v) = c[1].parse::<f64>() {
                return Some(v as u64);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(parse_size("1M").unwrap(), 1024 * 1024);
        assert_eq!(parse_size("5G").unwrap(), 5 * 1024 * 1024 * 1024);
        assert_eq!(parse_size("0.5kb").unwrap(), 512);
        assert_eq!(parse_size("1mb").unwrap(), 1024 * 1024);
        assert_eq!(parse_size("1gb").unwrap(), 1024 * 1024 * 1024);
        assert_eq!(parse_size("3.4Gb").unwrap(), parse_size("3.4G").unwrap());
        assert_eq!(parse_size("0.1b").unwrap(), 0);
        assert_eq!(parse_size("2.5mB").unwrap(), 2_621_440);
        assert_eq!(parse_size("1 MB").unwrap(), 1024 * 1024);
        assert_eq!(parse_size("100").unwrap(), 100);
        assert!(parse_size("xyz").is_err());
        assert!(parse_size("").is_err());

        assert_eq!(format_bytes(1024 * 1024), "1.0MB");
        assert_eq!(format_bytes(5 * 1024 * 1024 * 1024), "5.0GB");
        assert_eq!(format_bytes(512), "512B");
        assert!(format_bytes(1500).contains("KB") || format_bytes(1500).contains('B'));

        let n = parse_size("3.4Gb").unwrap();
        assert_eq!(parse_size(&format_bytes(n)).unwrap(), n);
    }

    #[test]
    fn docker_reclaimable_and_system_df() {
        assert_eq!(parse_docker_reclaimable_field("0B").unwrap(), 0);
        assert_eq!(
            parse_docker_reclaimable_field("1.5GB (45%)").unwrap(),
            parse_size("1.5G").unwrap()
        );
        assert_eq!(
            parse_docker_reclaimable_field("512MB (10%)").unwrap(),
            parse_size("512M").unwrap()
        );

        let df = parse_docker_system_df(
            "Containers\t0B (0%)\nImages\t1.0GB (50%)\nLocal Volumes\t2.5GB (100%)\nBuild Cache\t0B (0%)\n",
        );
        assert_eq!(df.get("Containers").copied(), Some(0));
        assert_eq!(df.get("Images").copied(), Some(parse_size("1G").unwrap()));
        assert_eq!(
            df.get("Local Volumes").copied(),
            Some(parse_size("2.5G").unwrap())
        );
        assert_eq!(df.get("Build Cache").copied(), Some(0));
    }
}
