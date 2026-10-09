//! ncdu-style size tree: scan a directory once, then browse and delete in memory.
//!
//! Sizes are disk usage (`st_blocks * 512`), so sparse files count what they really
//! occupy and hard links are counted once. The scan stays on one filesystem unless
//! `cross_fs` is set; mount points below the root show up as `other_fs` entries.

use crate::config::Config;
use crate::safety::{manual_delete_block_reason, matches_protect, resolve_protect_globs};
use crate::trash::{force_remove_path, move_to_trash};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

/// Virtual filesystems that are never walked, even with `cross_fs`.
const VIRTUAL_ROOTS: &[&str] = &["/proc", "/sys", "/dev", "/run"];

/// Directories with more files than this keep only the largest ones; the rest
/// collapse into one aggregate entry so huge cache dirs don't blow up memory.
pub const DEFAULT_MAX_FILES_PER_DIR: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Dir,
    File,
    Symlink,
    /// Mount point of another filesystem (not crossed).
    OtherFs,
    /// Aggregate of small files dropped by `max_files_per_dir`.
    Collapsed,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub name: Box<str>,
    pub kind: NodeKind,
    /// Disk usage in bytes, including all descendants.
    pub bytes: u64,
    /// Number of descendants (files + dirs); 1 for leaves, N for collapsed.
    pub items: u64,
    /// Modification time (unix seconds), newest of self for leaves.
    pub mtime: i64,
    /// Directory could not be read (permission denied etc.).
    pub unreadable: bool,
    /// Children sorted by `bytes` descending.
    pub children: Vec<Node>,
}

impl Node {
    fn leaf(name: Box<str>, kind: NodeKind, bytes: u64, mtime: i64) -> Self {
        Self {
            name,
            kind,
            bytes,
            items: 1,
            mtime,
            unreadable: false,
            children: Vec::new(),
        }
    }

    fn child(&self, name: &OsStr) -> Option<&Node> {
        let name = name.to_string_lossy();
        self.children.iter().find(|c| *c.name == *name)
    }

    fn child_mut(&mut self, name: &OsStr) -> Option<&mut Node> {
        let name = name.to_string_lossy();
        self.children.iter_mut().find(|c| *c.name == *name)
    }
}

#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub cross_fs: bool,
    pub max_files_per_dir: usize,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            cross_fs: false,
            max_files_per_dir: DEFAULT_MAX_FILES_PER_DIR,
        }
    }
}

/// Live counters while a scan is running.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TreeScanProgress {
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
    pub current: String,
}

#[derive(Default)]
struct ScanCtx {
    files: AtomicU64,
    dirs: AtomicU64,
    bytes: AtomicU64,
    unreadable: AtomicU64,
    current: Mutex<String>,
    hardlinks: Mutex<HashSet<(u64, u64)>>,
}

impl ScanCtx {
    fn progress(&self) -> TreeScanProgress {
        TreeScanProgress {
            files: self.files.load(Ordering::Relaxed),
            dirs: self.dirs.load(Ordering::Relaxed),
            bytes: self.bytes.load(Ordering::Relaxed),
            current: self.current.lock().map(|s| s.clone()).unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SizeTree {
    pub root_path: PathBuf,
    pub root: Node,
    pub cancelled: bool,
    /// Directories that could not be read.
    pub unreadable_dirs: u64,
    pub scanned_at: i64,
}

/// One row of a directory listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeEntry {
    pub name: String,
    /// Absolute path; empty for collapsed aggregates.
    pub path: String,
    pub kind: NodeKind,
    pub bytes: u64,
    pub items: u64,
    pub mtime: i64,
    pub unreadable: bool,
    /// Matches a protect glob (deletable, but the UI should warn).
    pub protected: bool,
    /// Why the entry cannot be deleted, if it can't.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirListing {
    pub root: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub bytes: u64,
    pub items: u64,
    pub unreadable: bool,
    pub entries: Vec<TreeEntry>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeleteFailure {
    pub path: String,
    pub error: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeleteOutcome {
    pub removed: Vec<String>,
    pub bytes_freed: u64,
    pub failures: Vec<DeleteFailure>,
    /// True when items went to Trash (space is only freed after emptying it).
    pub trashed: bool,
}

#[cfg(unix)]
fn meta_dev_ino_blocks(meta: &std::fs::Metadata) -> (u64, u64, u64, u64, i64) {
    use std::os::unix::fs::MetadataExt;
    (
        meta.dev(),
        meta.ino(),
        meta.blocks() * 512,
        meta.nlink(),
        meta.mtime(),
    )
}

#[cfg(not(unix))]
fn meta_dev_ino_blocks(meta: &std::fs::Metadata) -> (u64, u64, u64, u64, i64) {
    (0, 0, meta.len(), 1, 0)
}

fn is_virtual_root(path: &Path) -> bool {
    VIRTUAL_ROOTS.iter().any(|v| path == Path::new(v))
}

fn name_of(path: &Path) -> Box<str> {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
        .into_boxed_str()
}

/// Scan `root` into a size tree. `cancel` stops early (partial tree is returned);
/// `on_progress` is called from worker threads at most every ~64 directories.
pub fn scan_tree<F>(
    root: &Path,
    opts: &ScanOptions,
    cancel: &AtomicBool,
    on_progress: F,
) -> SizeTree
where
    F: Fn(TreeScanProgress) + Sync,
{
    let ctx = ScanCtx::default();
    let root_path = root.to_path_buf();
    let root_meta = std::fs::symlink_metadata(root);
    let mut node = match root_meta {
        Ok(meta) if meta.is_dir() => {
            let (dev, ..) = meta_dev_ino_blocks(&meta);
            scan_dir(root, name_of(root), dev, opts, cancel, &ctx, &on_progress)
        }
        Ok(meta) => {
            let (_, _, bytes, _, mtime) = meta_dev_ino_blocks(&meta);
            Node::leaf(name_of(root), NodeKind::File, bytes, mtime)
        }
        Err(_) => {
            let mut n = Node::leaf(name_of(root), NodeKind::Dir, 0, 0);
            n.items = 0;
            n.unreadable = true;
            ctx.unreadable.fetch_add(1, Ordering::Relaxed);
            n
        }
    };
    node.name = root_path.to_string_lossy().into_owned().into_boxed_str();
    on_progress(ctx.progress());
    SizeTree {
        root_path,
        root: node,
        cancelled: cancel.load(Ordering::Relaxed),
        unreadable_dirs: ctx.unreadable.load(Ordering::Relaxed),
        scanned_at: chrono::Utc::now().timestamp(),
    }
}

fn scan_dir<F>(
    path: &Path,
    name: Box<str>,
    root_dev: u64,
    opts: &ScanOptions,
    cancel: &AtomicBool,
    ctx: &ScanCtx,
    on_progress: &F,
) -> Node
where
    F: Fn(TreeScanProgress) + Sync,
{
    let mut node = Node::leaf(name, NodeKind::Dir, 0, 0);
    node.items = 0;
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        let (_, _, bytes, _, mtime) = meta_dev_ino_blocks(&meta);
        node.bytes = bytes;
        node.mtime = mtime;
    }
    let dirs_seen = ctx.dirs.fetch_add(1, Ordering::Relaxed) + 1;
    if dirs_seen.is_multiple_of(64) {
        if let Ok(mut cur) = ctx.current.try_lock() {
            *cur = path.to_string_lossy().into_owned();
        }
        on_progress(ctx.progress());
    }
    if cancel.load(Ordering::Relaxed) {
        return node;
    }

    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(_) => {
            node.unreadable = true;
            ctx.unreadable.fetch_add(1, Ordering::Relaxed);
            return node;
        }
    };

    let mut files: Vec<Node> = Vec::new();
    let mut subdirs: Vec<(PathBuf, Box<str>)> = Vec::new();
    let mut leaves: Vec<Node> = Vec::new();

    for entry in entries.flatten() {
        let child_path = entry.path();
        let child_name: Box<str> = entry
            .file_name()
            .to_string_lossy()
            .into_owned()
            .into_boxed_str();
        // DirEntry::metadata does not follow symlinks on unix.
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let (dev, ino, bytes, nlink, mtime) = meta_dev_ino_blocks(&meta);
        let ft = meta.file_type();
        if ft.is_symlink() {
            leaves.push(Node::leaf(child_name, NodeKind::Symlink, bytes, mtime));
        } else if ft.is_dir() {
            if is_virtual_root(&child_path) || (!opts.cross_fs && dev != root_dev) {
                leaves.push(Node::leaf(child_name, NodeKind::OtherFs, 0, mtime));
            } else {
                subdirs.push((child_path, child_name));
            }
        } else {
            let counted = if nlink > 1 && !ft.is_dir() {
                ctx.hardlinks
                    .lock()
                    .map(|mut set| set.insert((dev, ino)))
                    .unwrap_or(true)
            } else {
                true
            };
            let bytes = if counted { bytes } else { 0 };
            ctx.files.fetch_add(1, Ordering::Relaxed);
            ctx.bytes.fetch_add(bytes, Ordering::Relaxed);
            files.push(Node::leaf(child_name, NodeKind::File, bytes, mtime));
        }
    }

    let child_dirs: Vec<Node> = subdirs
        .into_par_iter()
        .map(|(p, n)| scan_dir(&p, n, root_dev, opts, cancel, ctx, on_progress))
        .collect();

    if files.len() > opts.max_files_per_dir {
        files.sort_unstable_by_key(|n| std::cmp::Reverse(n.bytes));
        let rest = files.split_off(opts.max_files_per_dir);
        let bytes = rest.iter().map(|n| n.bytes).sum();
        let mtime = rest.iter().map(|n| n.mtime).max().unwrap_or(0);
        let count = rest.len() as u64;
        let mut agg = Node::leaf(
            format!("({count} smaller files)").into_boxed_str(),
            NodeKind::Collapsed,
            bytes,
            mtime,
        );
        agg.items = count;
        files.push(agg);
    }

    let mut children = child_dirs;
    children.append(&mut files);
    children.append(&mut leaves);
    for c in &children {
        node.bytes = node.bytes.saturating_add(c.bytes);
        node.items = node.items.saturating_add(item_contribution(c));
    }
    sort_children(&mut children);
    node.children = children;
    node
}

/// How many items a child adds to its parent's `items` count.
fn item_contribution(n: &Node) -> u64 {
    if n.kind == NodeKind::Dir {
        1 + n.items
    } else {
        n.items
    }
}

fn remove_rec(node: &mut Node, comps: &[&OsStr]) -> Option<(u64, u64)> {
    let (first, rest) = comps.split_first()?;
    let delta = if rest.is_empty() {
        let name = first.to_string_lossy();
        let idx = node.children.iter().position(|n| *n.name == *name)?;
        let removed = node.children.remove(idx);
        (removed.bytes, item_contribution(&removed))
    } else {
        let delta = remove_rec(node.child_mut(first)?, rest)?;
        sort_children(&mut node.children);
        delta
    };
    node.bytes = node.bytes.saturating_sub(delta.0);
    node.items = node.items.saturating_sub(delta.1);
    Some(delta)
}

fn sort_children(children: &mut [Node]) {
    children.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
}

/// Path components of `path` below `root`, or None when outside.
fn rel_components<'a>(root: &Path, path: &'a Path) -> Option<Vec<&'a OsStr>> {
    let rel = path.strip_prefix(root).ok()?;
    let mut out = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(os) => out.push(os),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(out)
}

impl SizeTree {
    pub fn total_bytes(&self) -> u64 {
        self.root.bytes
    }

    pub fn find(&self, path: &Path) -> Option<&Node> {
        let mut node = &self.root;
        for c in rel_components(&self.root_path, path)? {
            node = node.child(c)?;
        }
        Some(node)
    }

    /// List one directory of the tree. Entries carry delete guards for `cfg`.
    pub fn listing(&self, path: &Path, cfg: &Config) -> Option<DirListing> {
        let node = self.find(path)?;
        let globs = resolve_protect_globs(cfg);
        let entries = node
            .children
            .iter()
            .map(|c| {
                let child_path = if c.kind == NodeKind::Collapsed {
                    PathBuf::new()
                } else {
                    path.join(&*c.name)
                };
                let blocked = match c.kind {
                    NodeKind::Collapsed => Some("aggregate of small files".to_string()),
                    NodeKind::OtherFs => Some("another filesystem (mount point)".to_string()),
                    _ => manual_delete_block_reason(&child_path, &cfg.home),
                };
                let protected =
                    c.kind != NodeKind::Collapsed && matches_protect(&child_path, &globs);
                TreeEntry {
                    name: c.name.to_string(),
                    path: child_path.to_string_lossy().into_owned(),
                    kind: c.kind,
                    bytes: c.bytes,
                    items: c.items,
                    mtime: c.mtime,
                    unreadable: c.unreadable,
                    protected,
                    blocked,
                }
            })
            .collect();
        let parent = if path == self.root_path {
            None
        } else {
            path.parent().map(|p| p.to_string_lossy().into_owned())
        };
        Some(DirListing {
            root: self.root_path.to_string_lossy().into_owned(),
            path: path.to_string_lossy().into_owned(),
            parent,
            bytes: node.bytes,
            items: node.items,
            unreadable: node.unreadable,
            entries,
        })
    }

    /// Drop `path` from the tree and subtract its size from every ancestor.
    /// Returns the bytes removed, or None when the path is not in the tree.
    pub fn remove(&mut self, path: &Path) -> Option<u64> {
        let comps = rel_components(&self.root_path, path)?;
        remove_rec(&mut self.root, &comps).map(|(bytes, _)| bytes)
    }

    /// Delete `paths` from disk (or move them to Trash) and update the tree.
    ///
    /// Each path must be inside the tree and pass [`manual_delete_block_reason`].
    /// Trash is refused for items on another filesystem than the home Trash:
    /// moving them would copy the data instead of freeing anything.
    pub fn delete_paths<F>(
        &mut self,
        paths: &[PathBuf],
        cfg: &Config,
        use_trash: bool,
        on_progress: F,
    ) -> DeleteOutcome
    where
        F: Fn(usize, usize, &str) + Sync,
    {
        let mut outcome = DeleteOutcome {
            trashed: use_trash,
            ..Default::default()
        };
        let mut jobs: Vec<(PathBuf, u64)> = Vec::new();
        let mut seen: HashSet<PathBuf> = HashSet::new();
        // Children of a selected parent are redundant.
        let mut sorted: Vec<&PathBuf> = paths.iter().collect();
        sorted.sort();
        for p in sorted {
            if jobs.iter().any(|(j, _)| p.starts_with(j)) || !seen.insert(p.clone()) {
                continue;
            }
            let fail = |error: String| DeleteFailure {
                path: p.to_string_lossy().into_owned(),
                error,
            };
            let Some(node) = self.find(p) else {
                outcome
                    .failures
                    .push(fail("not in the scanned tree".into()));
                continue;
            };
            if matches!(node.kind, NodeKind::Collapsed | NodeKind::OtherFs) || p == &self.root_path
            {
                outcome
                    .failures
                    .push(fail("cannot delete this entry".into()));
                continue;
            }
            if let Some(reason) = manual_delete_block_reason(p, &cfg.home) {
                outcome.failures.push(fail(reason));
                continue;
            }
            if use_trash && !same_filesystem(p, &cfg.home) {
                outcome.failures.push(fail(
                    "on another filesystem than Trash; delete permanently instead".into(),
                ));
                continue;
            }
            jobs.push((p.clone(), node.bytes));
        }

        let total = jobs.len();
        let done = AtomicU64::new(0);
        let results: Vec<(PathBuf, u64, Result<(), String>)> = jobs
            .into_par_iter()
            .map(|(p, bytes)| {
                let res = if use_trash {
                    move_to_trash(&cfg.home, &p)
                } else {
                    force_remove_path(&p)
                }
                .map_err(|e| e.to_string());
                let n = done.fetch_add(1, Ordering::Relaxed) as usize + 1;
                on_progress(n, total, &p.to_string_lossy());
                (p, bytes, res)
            })
            .collect();

        for (p, bytes, res) in results {
            match res {
                Ok(()) => {
                    self.remove(&p);
                    outcome.bytes_freed = outcome.bytes_freed.saturating_add(bytes);
                    outcome.removed.push(p.to_string_lossy().into_owned());
                }
                Err(error) => {
                    // A partial delete changes sizes; drop the subtree only if it is gone.
                    if std::fs::symlink_metadata(&p).is_err() {
                        self.remove(&p);
                    }
                    outcome.failures.push(DeleteFailure {
                        path: p.to_string_lossy().into_owned(),
                        error,
                    });
                }
            }
        }
        outcome
    }
}

#[cfg(unix)]
fn same_filesystem(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::symlink_metadata(a), std::fs::metadata(b)) {
        (Ok(ma), Ok(mb)) => ma.dev() == mb.dev(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_filesystem(_a: &Path, _b: &Path) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &Path, len: usize) {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).unwrap();
        }
        // Random-ish content so filesystems can't dedupe/compress it away.
        let data: Vec<u8> = (0..len).map(|i| (i * 31 % 251) as u8).collect();
        fs::write(path, data).unwrap();
    }

    fn scan(root: &Path, opts: &ScanOptions) -> SizeTree {
        scan_tree(root, opts, &AtomicBool::new(false), |_| {})
    }

    fn cfg_with_home(home: &Path) -> Config {
        let mut cfg = Config::default();
        cfg.home = home.to_path_buf();
        cfg
    }

    #[test]
    fn sizes_aggregate_and_sort_desc() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(&root.join("big/a.bin"), 200_000);
        write(&root.join("big/nested/b.bin"), 100_000);
        write(&root.join("small/c.bin"), 10_000);
        write(&root.join("top.bin"), 50_000);

        let tree = scan(root, &ScanOptions::default());
        let names: Vec<&str> = tree.root.children.iter().map(|c| &*c.name).collect();
        assert_eq!(names, vec!["big", "top.bin", "small"]);

        let big = tree.find(&root.join("big")).unwrap();
        let nested = tree.find(&root.join("big/nested")).unwrap();
        assert!(big.bytes >= 300_000, "big={}", big.bytes);
        assert!(nested.bytes >= 100_000);
        assert!(big.bytes > nested.bytes);
        // big: a.bin, nested, nested/b.bin
        assert_eq!(big.items, 3);
        let sum: u64 = tree.root.children.iter().map(|c| c.bytes).sum();
        assert!(tree.total_bytes() >= sum);
        assert!(!tree.cancelled);
    }

    #[test]
    fn hardlinks_counted_once() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(&root.join("a/file.bin"), 400_000);
        fs::create_dir_all(root.join("b")).unwrap();
        fs::hard_link(root.join("a/file.bin"), root.join("b/link.bin")).unwrap();

        let tree = scan(root, &ScanOptions::default());
        let a = tree.find(&root.join("a")).unwrap().bytes;
        let b = tree.find(&root.join("b")).unwrap().bytes;
        let file_bytes = a.max(b);
        assert!(file_bytes >= 400_000);
        // One of the two dirs got the file, the other only its own dir block.
        assert!(a.min(b) < 100_000, "a={a} b={b}");
    }

    #[test]
    fn symlinks_are_not_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        write(&tmp.path().join("outside/huge.bin"), 500_000);
        fs::create_dir_all(&root).unwrap();
        std::os::unix::fs::symlink(tmp.path().join("outside"), root.join("link")).unwrap();

        let tree = scan(&root, &ScanOptions::default());
        let link = tree.find(&root.join("link")).unwrap();
        assert_eq!(link.kind, NodeKind::Symlink);
        assert!(tree.total_bytes() < 500_000);
    }

    #[test]
    fn many_files_collapse_into_aggregate() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for i in 0..30 {
            write(&root.join(format!("f{i:02}.bin")), 5_000 + i * 1_000);
        }
        let opts = ScanOptions {
            max_files_per_dir: 10,
            ..Default::default()
        };
        let tree = scan(root, &opts);
        let files = tree
            .root
            .children
            .iter()
            .filter(|c| c.kind == NodeKind::File)
            .count();
        assert_eq!(files, 10);
        let agg = tree
            .root
            .children
            .iter()
            .find(|c| c.kind == NodeKind::Collapsed)
            .unwrap();
        assert_eq!(agg.items, 20);
        assert_eq!(&*agg.name, "(20 smaller files)");
        // The kept files are the largest ones.
        assert!(tree.find(&root.join("f29.bin")).is_some());
        assert!(tree.find(&root.join("f00.bin")).is_none());
        assert_eq!(tree.root.items, 30);
    }

    #[test]
    fn cancel_returns_partial_tree() {
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("a/b.bin"), 1000);
        let cancel = AtomicBool::new(true);
        let tree = scan_tree(tmp.path(), &ScanOptions::default(), &cancel, |_| {});
        assert!(tree.cancelled);
        assert!(tree.root.children.is_empty());
    }

    #[test]
    fn unreadable_dir_is_flagged() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return; // root bypasses permissions
        }
        let tmp = tempfile::tempdir().unwrap();
        let locked = tmp.path().join("locked");
        write(&locked.join("secret.bin"), 1000);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let tree = scan(tmp.path(), &ScanOptions::default());
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        let node = tree.find(&locked).unwrap();
        assert!(node.unreadable);
        assert_eq!(tree.unreadable_dirs, 1);
    }

    #[test]
    fn listing_marks_guards_and_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        write(&home.join(".cache/huggingface/model.bin"), 2000);
        write(&home.join("docs/a.txt"), 1000);
        let cfg = cfg_with_home(&home);

        let tree = scan(tmp.path(), &ScanOptions::default());
        let top = tree.listing(tmp.path(), &cfg).unwrap();
        assert!(top.parent.is_none());
        let home_entry = top.entries.iter().find(|e| e.name == "home").unwrap();
        assert!(home_entry.blocked.as_deref().unwrap().contains("home"));

        let cache = tree.listing(&home.join(".cache"), &cfg).unwrap();
        assert_eq!(cache.parent.as_deref(), Some(home.to_str().unwrap()));
        let hf = cache
            .entries
            .iter()
            .find(|e| e.name == "huggingface")
            .unwrap();
        assert!(hf.protected);
        assert!(hf.blocked.is_none());
        assert!(tree.listing(&tmp.path().join("missing"), &cfg).is_none());
    }

    #[test]
    fn remove_updates_ancestors() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(&root.join("a/b/c.bin"), 300_000);
        write(&root.join("a/keep.bin"), 10_000);
        let mut tree = scan(root, &ScanOptions::default());
        let before_root = tree.total_bytes();
        let before_a = tree.find(&root.join("a")).unwrap().bytes;
        let b_bytes = tree.find(&root.join("a/b")).unwrap().bytes;

        assert_eq!(tree.remove(&root.join("a/b")), Some(b_bytes));
        assert!(tree.find(&root.join("a/b")).is_none());
        assert_eq!(
            tree.find(&root.join("a")).unwrap().bytes,
            before_a - b_bytes
        );
        assert_eq!(tree.total_bytes(), before_root - b_bytes);
        assert_eq!(tree.find(&root.join("a")).unwrap().items, 1);
        assert_eq!(tree.remove(&root.join("nope")), None);
        assert_eq!(tree.remove(Path::new("/elsewhere")), None);
    }

    #[test]
    fn delete_paths_removes_from_disk_and_tree() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let work = home.join("work");
        write(&work.join("junk/x.bin"), 100_000);
        write(&work.join("junk/ro/y.bin"), 1000);
        // Read-only dirs (Go/uv caches) must still delete.
        fs::set_permissions(work.join("junk/ro"), fs::Permissions::from_mode(0o555)).unwrap();
        write(&work.join("file.bin"), 50_000);
        write(&work.join("keep.bin"), 1000);
        let cfg = cfg_with_home(&home);

        let mut tree = scan(&work, &ScanOptions::default());
        let total = tree.total_bytes();
        let calls = Mutex::new(0usize);
        let out = tree.delete_paths(
            &[
                work.join("junk"),
                work.join("junk/x.bin"),
                work.join("file.bin"),
            ],
            &cfg,
            false,
            |_, _, _| *calls.lock().unwrap() += 1,
        );
        assert!(out.failures.is_empty(), "{:?}", out.failures);
        assert_eq!(out.removed.len(), 2, "nested child is deduped");
        assert_eq!(*calls.lock().unwrap(), 2);
        assert!(!work.join("junk").exists());
        assert!(!work.join("file.bin").exists());
        assert!(work.join("keep.bin").exists());
        assert_eq!(tree.total_bytes(), total - out.bytes_freed);
        assert!(!out.trashed);
    }

    #[test]
    fn delete_paths_to_trash_and_guards() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        write(&home.join("dl/old.iso"), 5000);
        let cfg = cfg_with_home(&home);

        let mut tree = scan(tmp.path(), &ScanOptions::default());
        let out = tree.delete_paths(
            &[
                home.join("dl/old.iso"),
                home.clone(),
                tmp.path().join("ghost"),
            ],
            &cfg,
            true,
            |_, _, _| {},
        );
        assert_eq!(
            out.removed,
            vec![home.join("dl/old.iso").to_string_lossy().to_string()]
        );
        assert!(out.trashed);
        assert_eq!(out.failures.len(), 2);
        assert!(home.exists());
        assert!(
            home.join(".local/share/Trash/files")
                .read_dir()
                .unwrap()
                .count()
                == 1
        );
    }
}
