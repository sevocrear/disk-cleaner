use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TRASH_NAME_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrashItem {
    pub name: String,
    pub original_path: String,
    pub trash_path: String,
    pub info_path: String,
    pub bytes: u64,
    pub deleted_at: Option<String>,
    pub is_dir: bool,
}

fn trash_dirs(home: &Path) -> (PathBuf, PathBuf) {
    let base = home.join(".local/share/Trash");
    (base.join("files"), base.join("info"))
}

pub fn list_trash(home: &Path) -> Vec<TrashItem> {
    let (files_dir, info_dir) = trash_dirs(home);
    let mut items = Vec::new();
    if !files_dir.is_dir() {
        return items;
    }
    let Ok(entries) = std::fs::read_dir(&files_dir) else {
        return items;
    };
    for entry in entries.flatten() {
        let trash_path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let info_path = info_dir.join(format!("{name}.trashinfo"));
        let (original, deleted_at) = parse_trashinfo(&info_path).unwrap_or_default();
        let is_dir = trash_path.is_dir();
        let bytes = if is_dir {
            crate::util::dir_size(&trash_path)
        } else {
            std::fs::metadata(&trash_path).map(|m| m.len()).unwrap_or(0)
        };
        items.push(TrashItem {
            name,
            original_path: original,
            trash_path: trash_path.to_string_lossy().to_string(),
            info_path: info_path.to_string_lossy().to_string(),
            bytes,
            deleted_at,
            is_dir,
        });
    }
    items.sort_by(|a, b| b.deleted_at.cmp(&a.deleted_at));
    items
}

fn parse_trashinfo(path: &Path) -> Option<(String, Option<String>)> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut original = String::new();
    let mut deleted = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("Path=") {
            original = urlencoding_decode(rest);
        } else if let Some(rest) = line.strip_prefix("DeletionDate=") {
            deleted = Some(rest.trim().to_string());
        }
    }
    Some((original, deleted))
}

fn urlencoding_decode(s: &str) -> String {
    // Minimal percent-decode
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Ok(h), Ok(l)) = (
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 2]).unwrap_or(""), 16),
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 2..i + 3]).unwrap_or(""), 16),
            ) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn urlencoding_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Move a path into XDG trash.
pub fn move_to_trash(home: &Path, path: &Path) -> std::io::Result<()> {
    let (files_dir, info_dir) = trash_dirs(home);
    std::fs::create_dir_all(&files_dir)?;
    std::fs::create_dir_all(&info_dir)?;
    let name = unique_trash_name(&files_dir, path)?;
    let dest = files_dir.join(&name);
    if let Err(_) = std::fs::rename(path, &dest) {
        // cross-device: copy then remove
        if path.is_dir() {
            copy_dir_all(path, &dest)?;
            std::fs::remove_dir_all(path)?;
        } else {
            std::fs::copy(path, &dest)?;
            std::fs::remove_file(path)?;
        }
    }
    let info = format!(
        "[Trash Info]\nPath={}\nDeletionDate={}\n",
        urlencoding_encode(&path.to_string_lossy()),
        Utc::now().format("%Y-%m-%dT%H:%M:%S")
    );
    std::fs::write(info_dir.join(format!("{name}.trashinfo")), info)?;
    Ok(())
}

fn unique_trash_name(files_dir: &Path, path: &Path) -> std::io::Result<String> {
    let base = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "item".into());
    // Process id + atomic seq keeps parallel workers from colliding on the same name.
    let seq = TRASH_NAME_SEQ.fetch_add(1, Ordering::Relaxed);
    let mut name = format!("{base}.{}.{}", std::process::id(), seq);
    let mut i = 1u64;
    while files_dir.join(&name).exists() {
        name = format!("{base}.{}.{}.{}", std::process::id(), seq, i);
        i += 1;
    }
    Ok(name)
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), to)?;
        }
    }
    Ok(())
}

pub fn restore_trash_item(item: &TrashItem) -> std::io::Result<()> {
    let src = PathBuf::from(&item.trash_path);
    let dest = PathBuf::from(&item.original_path);
    if dest.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("target exists: {}", dest.display()),
        ));
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if let Err(_) = std::fs::rename(&src, &dest) {
        if src.is_dir() {
            copy_dir_all(&src, &dest)?;
            std::fs::remove_dir_all(&src)?;
        } else {
            std::fs::copy(&src, &dest)?;
            std::fs::remove_file(&src)?;
        }
    }
    let _ = std::fs::remove_file(&item.info_path);
    Ok(())
}

pub fn delete_trash_item(item: &TrashItem) -> std::io::Result<()> {
    let p = PathBuf::from(&item.trash_path);
    if p.is_dir() {
        std::fs::remove_dir_all(&p)?;
    } else if p.exists() {
        std::fs::remove_file(&p)?;
    }
    let _ = std::fs::remove_file(&item.info_path);
    Ok(())
}

pub fn empty_trash(home: &Path) -> std::io::Result<u64> {
    let mut n = 0u64;
    for item in list_trash(home) {
        delete_trash_item(&item)?;
        n += 1;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn trash_roundtrip() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let file = home.join("doc.txt");
        std::fs::write(&file, b"hello").unwrap();
        move_to_trash(home, &file).unwrap();
        assert!(!file.exists());
        let items = list_trash(home);
        assert_eq!(items.len(), 1);
        restore_trash_item(&items[0]).unwrap();
        assert!(file.exists());
    }
}
