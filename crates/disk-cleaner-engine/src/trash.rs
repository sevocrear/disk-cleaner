use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs;
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
    let Ok(entries) = fs::read_dir(&files_dir) else {
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
            fs::metadata(&trash_path).map(|m| m.len()).unwrap_or(0)
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
    let text = fs::read_to_string(path).ok()?;
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
    fs::create_dir_all(&files_dir)?;
    fs::create_dir_all(&info_dir)?;
    let name = unique_trash_name(&files_dir, path)?;
    let dest = files_dir.join(&name);
    if fs::rename(path, &dest).is_err() {
        // cross-device: copy then remove
        if path.is_dir() {
            copy_dir_all(path, &dest)?;
            force_remove_path(path)?;
        } else {
            fs::copy(path, &dest)?;
            force_remove_path(path)?;
        }
    }
    let info = format!(
        "[Trash Info]\nPath={}\nDeletionDate={}\n",
        urlencoding_encode(&path.to_string_lossy()),
        Utc::now().format("%Y-%m-%dT%H:%M:%S")
    );
    fs::write(info_dir.join(format!("{name}.trashinfo")), info)?;
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
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &to)?;
        } else {
            fs::copy(entry.path(), to)?;
        }
    }
    Ok(())
}

/// Clear the owner write bit when missing so unlinks / rmdir can succeed.
/// Desktop trash often contains trees with mode 0555 (e.g. copied archives).
fn ensure_owner_writable(path: &Path, meta: &fs::Metadata) {
    let mut perms = meta.permissions();
    if perms.readonly() {
        perms.set_readonly(false);
        let _ = fs::set_permissions(path, perms);
    }
}

/// Permanently remove a trash path, including read-only dirs/files and symlinks.
pub fn force_remove_path(path: &Path) -> std::io::Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let ft = meta.file_type();
    if ft.is_symlink() {
        return fs::remove_file(path);
    }
    if ft.is_dir() {
        ensure_owner_writable(path, &meta);
        for entry in fs::read_dir(path)? {
            force_remove_path(&entry?.path())?;
        }
        return fs::remove_dir(path);
    }
    ensure_owner_writable(path, &meta);
    fs::remove_file(path)
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
        fs::create_dir_all(parent)?;
    }
    if fs::rename(&src, &dest).is_err() {
        if src.is_dir() {
            copy_dir_all(&src, &dest)?;
            force_remove_path(&src)?;
        } else {
            fs::copy(&src, &dest)?;
            force_remove_path(&src)?;
        }
    }
    let _ = fs::remove_file(&item.info_path);
    Ok(())
}

pub fn delete_trash_item(item: &TrashItem) -> std::io::Result<()> {
    let p = PathBuf::from(&item.trash_path);
    force_remove_path(&p)?;
    let _ = fs::remove_file(&item.info_path);
    Ok(())
}

/// Permanently remove every entry under XDG Trash `files/` and clear `info/`.
/// Continues past individual failures; returns an error only if nothing could be removed
/// while errors occurred, or if the trash dirs themselves are unreadable.
pub fn empty_trash(home: &Path) -> std::io::Result<u64> {
    let (files_dir, info_dir) = trash_dirs(home);
    let mut deleted = 0u64;
    let mut errors: Vec<String> = Vec::new();

    if files_dir.is_dir() {
        for entry in fs::read_dir(&files_dir)?.flatten() {
            let path = entry.path();
            match force_remove_path(&path) {
                Ok(()) => deleted += 1,
                Err(e) => errors.push(format!("{}: {e}", path.display())),
            }
        }
    }

    // Always clear sidecar .trashinfo (including orphans with no files/ entry).
    if info_dir.is_dir() {
        for entry in fs::read_dir(&info_dir)?.flatten() {
            let path = entry.path();
            if let Err(e) = fs::remove_file(&path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    errors.push(format!("{}: {e}", path.display()));
                }
            }
        }
    }

    if !errors.is_empty() && deleted == 0 && files_dir.is_dir() {
        let still = fs::read_dir(&files_dir)
            .map(|rd| rd.flatten().count())
            .unwrap_or(0);
        if still > 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                errors.join("; "),
            ));
        }
    }

    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    fn write_trashinfo(info_dir: &Path, name: &str, original: &str) {
        let body = format!(
            "[Trash Info]\nPath={}\nDeletionDate=2026-09-09T21:07:56\n",
            urlencoding_encode(original)
        );
        fs::write(info_dir.join(format!("{name}.trashinfo")), body).unwrap();
    }

    fn seed_item(home: &Path, name: &str, original: &str, is_dir: bool) -> PathBuf {
        let (files, info) = trash_dirs(home);
        fs::create_dir_all(&files).unwrap();
        fs::create_dir_all(&info).unwrap();
        let path = files.join(name);
        if is_dir {
            fs::create_dir_all(&path).unwrap();
            fs::write(path.join("inner.txt"), b"x").unwrap();
        } else {
            fs::write(&path, b"data").unwrap();
        }
        write_trashinfo(&info, name, original);
        path
    }

    #[test]
    fn trash_roundtrip() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let file = home.join("doc.txt");
        fs::write(&file, b"hello").unwrap();
        move_to_trash(home, &file).unwrap();
        assert!(!file.exists());
        let items = list_trash(home);
        assert_eq!(items.len(), 1);
        restore_trash_item(&items[0]).unwrap();
        assert!(file.exists());
    }

    #[test]
    fn empty_trash_removes_files_dirs_and_orphan_info() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        seed_item(home, "code.py", "/tmp/code.py", false);
        seed_item(home, "Telegram Desktop", "/tmp/Telegram Desktop", true);
        seed_item(home, "invoicesample.ru (11).pdf", "/tmp/invoicesample.ru (11).pdf", false);

        let (files, info) = trash_dirs(home);
        write_trashinfo(&info, "orphan-only", "/tmp/orphan");

        assert_eq!(list_trash(home).len(), 3);
        let n = empty_trash(home).unwrap();
        assert_eq!(n, 3);
        assert!(list_trash(home).is_empty());
        assert!(fs::read_dir(&files).unwrap().next().is_none());
        assert!(fs::read_dir(&info).unwrap().next().is_none());
    }

    #[test]
    fn empty_trash_clears_readonly_nested_dirs() {
        // Reproduces real XDG trash failure: nested dir mode 0555 blocks unlink.
        let dir = tempdir().unwrap();
        let home = dir.path();
        let (files, info) = trash_dirs(home);
        fs::create_dir_all(&files).unwrap();
        fs::create_dir_all(&info).unwrap();

        let root = files.join("Telegram Desktop");
        let nested = root.join("plans").join("!!! read-only pack");
        fs::create_dir_all(&nested).unwrap();
        let blocked = nested.join("Маршала Катукова д.11 (2).dwg");
        fs::write(&blocked, b"dwg-bytes").unwrap();
        fs::write(root.join("ok.pdf"), b"pdf").unwrap();

        let mut perms = fs::metadata(&nested).unwrap().permissions();
        perms.set_mode(0o555);
        fs::set_permissions(&nested, perms).unwrap();
        assert!(fs::metadata(&nested).unwrap().permissions().readonly());

        write_trashinfo(&info, "Telegram Desktop", "/home/user/Downloads/Telegram Desktop");

        // Plain remove_dir_all must fail — this is the bug Empty trash used to hit.
        assert!(fs::remove_dir_all(&root).is_err());
        assert!(root.exists());

        let n = empty_trash(home).unwrap();
        assert_eq!(n, 1);
        assert!(!root.exists());
        assert!(list_trash(home).is_empty());
    }

    #[test]
    fn delete_trash_item_handles_readonly_tree() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let path = seed_item(home, "locked-dir", "/tmp/locked-dir", true);
        let nested = path.join("sub");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("a.txt"), b"a").unwrap();
        let mut perms = fs::metadata(&nested).unwrap().permissions();
        perms.set_mode(0o555);
        fs::set_permissions(&nested, perms).unwrap();

        let items = list_trash(home);
        assert_eq!(items.len(), 1);
        delete_trash_item(&items[0]).unwrap();
        assert!(list_trash(home).is_empty());
    }

    #[test]
    fn force_remove_deletes_symlink_without_following() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("target.txt");
        fs::write(&target, b"keep").unwrap();
        let link = dir.path().join("link.txt");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        force_remove_path(&link).unwrap();
        assert!(!link.exists());
        assert!(target.exists());
    }

    #[test]
    fn empty_trash_on_missing_trash_is_ok() {
        let dir = tempdir().unwrap();
        assert_eq!(empty_trash(dir.path()).unwrap(), 0);
    }
}
