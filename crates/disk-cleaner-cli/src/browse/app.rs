//! Browse state machine: keys in, effects out. No terminal I/O here.

use disk_cleaner_engine::config::Config;
use disk_cleaner_engine::inuse::InUse;
use disk_cleaner_engine::space::SpaceAccounting;
use disk_cleaner_engine::tree::{DeleteOutcome, DirListing, NodeKind, SizeTree, TreeEntry};
use disk_cleaner_engine::util::format_bytes;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Size,
    Name,
    Modified,
}

impl SortKey {
    fn next(self) -> Self {
        match self {
            SortKey::Size => SortKey::Name,
            SortKey::Name => SortKey::Modified,
            SortKey::Modified => SortKey::Size,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SortKey::Size => "size",
            SortKey::Name => "name",
            SortKey::Modified => "modified",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Confirm {
    pub paths: Vec<PathBuf>,
    pub bytes: u64,
    pub in_use: Vec<InUse>,
    pub protected: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum Mode {
    Scanning,
    Browse,
    Confirm(Confirm),
    Help,
}

/// What the run loop must do after a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    None,
    /// Ask the loop to check `/proc` for processes using these paths, then confirm.
    CheckInUse(Vec<PathBuf>),
    Delete {
        paths: Vec<PathBuf>,
        trash: bool,
    },
    Rescan,
    Open(PathBuf),
    Quit,
}

pub struct App {
    pub root: PathBuf,
    pub cfg: Config,
    pub tree: Option<SizeTree>,
    pub accounting: Option<SpaceAccounting>,
    pub cwd: PathBuf,
    pub listing: Option<DirListing>,
    pub cursor: usize,
    pub marked: BTreeSet<PathBuf>,
    pub sort: SortKey,
    pub mode: Mode,
    pub status: String,
    /// Cursor per directory, restored when coming back up.
    remembered: HashMap<PathBuf, usize>,
    /// Rows visible in the list (set by the renderer) for PageUp/PageDown.
    pub page: usize,
}

impl App {
    pub fn new(root: PathBuf, cfg: Config) -> Self {
        Self {
            cwd: root.clone(),
            root,
            cfg,
            tree: None,
            accounting: None,
            listing: None,
            cursor: 0,
            marked: BTreeSet::new(),
            sort: SortKey::Size,
            mode: Mode::Scanning,
            status: String::new(),
            remembered: HashMap::new(),
            page: 20,
        }
    }

    pub fn set_tree(&mut self, tree: SizeTree, accounting: Option<SpaceAccounting>) {
        self.tree = Some(tree);
        self.accounting = accounting;
        self.mode = Mode::Browse;
        // Stay where we were after a rescan when that dir still exists.
        if self.tree.as_ref().and_then(|t| t.find(&self.cwd)).is_none() {
            self.cwd = self.root.clone();
        }
        self.marked
            .retain(|p| self.tree.as_ref().is_some_and(|t| t.find(p).is_some()));
        self.refresh();
    }

    pub fn entries(&self) -> &[TreeEntry] {
        self.listing
            .as_ref()
            .map(|l| l.entries.as_slice())
            .unwrap_or(&[])
    }

    pub fn current(&self) -> Option<&TreeEntry> {
        self.entries().get(self.cursor)
    }

    pub fn marked_bytes(&self) -> u64 {
        let Some(tree) = &self.tree else { return 0 };
        self.marked
            .iter()
            .filter(|p| !self.marked.iter().any(|q| q != *p && p.starts_with(q)))
            .filter_map(|p| tree.find(p))
            .map(|n| n.bytes)
            .sum()
    }

    pub fn refresh(&mut self) {
        let Some(tree) = &self.tree else { return };
        self.listing = tree.listing(&self.cwd, &self.cfg);
        if let Some(l) = &mut self.listing {
            match self.sort {
                SortKey::Size => {}
                SortKey::Name => l.entries.sort_by_key(|e| e.name.to_lowercase()),
                SortKey::Modified => l.entries.sort_by_key(|e| std::cmp::Reverse(e.mtime)),
            }
        }
        let n = self.entries().len();
        self.cursor = self.cursor.min(n.saturating_sub(1));
    }

    fn enter(&mut self) {
        let Some(e) = self.current() else { return };
        if e.kind != NodeKind::Dir {
            self.status = match e.kind {
                NodeKind::OtherFs => "another filesystem — browse it separately".into(),
                NodeKind::Collapsed => "small files are aggregated".into(),
                _ => "not a directory".into(),
            };
            return;
        }
        let target = PathBuf::from(&e.path);
        self.remembered.insert(self.cwd.clone(), self.cursor);
        self.cwd = target;
        self.cursor = self.remembered.get(&self.cwd).copied().unwrap_or(0);
        self.refresh();
    }

    fn up(&mut self) {
        if self.cwd == self.root {
            return;
        }
        let child = self.cwd.clone();
        self.remembered.insert(self.cwd.clone(), self.cursor);
        self.cwd = self
            .cwd
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.root.clone());
        self.refresh();
        // Put the cursor on the directory we just left.
        if let Some(i) = self
            .entries()
            .iter()
            .position(|e| Path::new(&e.path) == child)
        {
            self.cursor = i;
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        let n = self.entries().len();
        if n == 0 {
            return;
        }
        let next = (self.cursor as isize + delta).clamp(0, n as isize - 1);
        self.cursor = next as usize;
    }

    fn toggle_mark(&mut self) {
        let Some(e) = self.current() else { return };
        if let Some(reason) = &e.blocked {
            self.status = format!("{}: {}", e.name, reason);
            self.move_cursor(1);
            return;
        }
        let p = PathBuf::from(&e.path);
        if !self.marked.remove(&p) {
            self.marked.insert(p);
        }
        self.move_cursor(1);
    }

    /// Paths `d` acts on: marked items, else the item under the cursor.
    fn delete_targets(&mut self) -> Option<Vec<PathBuf>> {
        if !self.marked.is_empty() {
            return Some(self.marked.iter().cloned().collect());
        }
        let e = self.current()?;
        if let Some(reason) = &e.blocked {
            self.status = format!("cannot delete {}: {}", e.name, reason);
            return None;
        }
        Some(vec![PathBuf::from(&e.path)])
    }

    /// Called by the run loop with the `/proc` check result for `paths`.
    pub fn open_confirm(&mut self, paths: Vec<PathBuf>, in_use: Vec<InUse>) {
        let Some(tree) = &self.tree else { return };
        let bytes = paths
            .iter()
            .filter(|p| !paths.iter().any(|q| q != *p && p.starts_with(q)))
            .filter_map(|p| tree.find(p))
            .map(|n| n.bytes)
            .sum();
        let globs = disk_cleaner_engine::safety::resolve_protect_globs(&self.cfg);
        let protected = paths
            .iter()
            .filter(|p| disk_cleaner_engine::safety::matches_protect(p, &globs))
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        self.mode = Mode::Confirm(Confirm {
            paths,
            bytes,
            in_use,
            protected,
        });
    }

    pub fn apply_outcome(&mut self, outcome: &DeleteOutcome) {
        for p in &outcome.removed {
            self.marked.remove(Path::new(p));
        }
        self.marked
            .retain(|p| !outcome.removed.iter().any(|r| p.starts_with(r)));
        self.mode = Mode::Browse;
        self.refresh();
        let verb = if outcome.trashed {
            "moved to Trash"
        } else {
            "deleted"
        };
        self.status = if outcome.failures.is_empty() {
            format!(
                "{} item(s) {verb} · {}",
                outcome.removed.len(),
                format_bytes(outcome.bytes_freed)
            )
        } else {
            format!(
                "{} item(s) {verb} · {} · {} failed: {}",
                outcome.removed.len(),
                format_bytes(outcome.bytes_freed),
                outcome.failures.len(),
                outcome.failures[0].error
            )
        };
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Effect {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Effect::Quit;
        }
        match &self.mode {
            Mode::Scanning => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => Effect::Quit,
                _ => Effect::None,
            },
            Mode::Help => {
                self.mode = Mode::Browse;
                Effect::None
            }
            Mode::Confirm(c) => {
                let paths = c.paths.clone();
                match key.code {
                    KeyCode::Char('t') => Effect::Delete { paths, trash: true },
                    KeyCode::Char('D') => Effect::Delete {
                        paths,
                        trash: false,
                    },
                    KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('q') => {
                        self.mode = Mode::Browse;
                        self.status = "cancelled".into();
                        Effect::None
                    }
                    _ => Effect::None,
                }
            }
            Mode::Browse => self.on_browse_key(key),
        }
    }

    fn on_browse_key(&mut self, key: KeyEvent) -> Effect {
        self.status.clear();
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Effect::Quit,
            KeyCode::Up | KeyCode::Char('k') => self.move_cursor(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_cursor(1),
            KeyCode::PageUp => self.move_cursor(-(self.page as isize)),
            KeyCode::PageDown => self.move_cursor(self.page as isize),
            KeyCode::Home | KeyCode::Char('g') => self.cursor = 0,
            KeyCode::End | KeyCode::Char('G') => self.move_cursor(isize::MAX / 2),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.enter(),
            KeyCode::Left | KeyCode::Backspace | KeyCode::Char('h') => self.up(),
            KeyCode::Char(' ') => self.toggle_mark(),
            KeyCode::Char('u') => {
                self.marked.clear();
                self.status = "marks cleared".into();
            }
            KeyCode::Char('s') => {
                self.sort = self.sort.next();
                self.refresh();
                self.status = format!("sorted by {}", self.sort.label());
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                if let Some(paths) = self.delete_targets() {
                    return Effect::CheckInUse(paths);
                }
            }
            KeyCode::Char('r') => {
                self.mode = Mode::Scanning;
                return Effect::Rescan;
            }
            KeyCode::Char('o') => {
                let target = self
                    .current()
                    .filter(|e| !e.path.is_empty())
                    .map(|e| PathBuf::from(&e.path))
                    .unwrap_or_else(|| self.cwd.clone());
                return Effect::Open(target);
            }
            KeyCode::Char('?') => self.mode = Mode::Help,
            _ => {}
        }
        Effect::None
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use disk_cleaner_engine::tree::{scan_tree, ScanOptions};
    use std::fs;
    use std::sync::atomic::AtomicBool;

    pub(crate) fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    pub(crate) fn fixture() -> (tempfile::TempDir, App) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        for (p, len) in [
            ("big/a.bin", 300_000usize),
            ("big/inner/b.bin", 200_000),
            ("mid/c.bin", 100_000),
            ("zeta.bin", 10_000),
        ] {
            let path = root.join(p);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, vec![1u8; len]).unwrap();
        }
        let mut cfg = Config::default();
        cfg.home = tmp.path().join("home");
        fs::create_dir_all(&cfg.home).unwrap();
        let tree = scan_tree(
            &root,
            &ScanOptions::default(),
            &AtomicBool::new(false),
            |_| {},
        );
        let mut app = App::new(root, cfg);
        app.set_tree(tree, None);
        (tmp, app)
    }

    fn names(app: &App) -> Vec<String> {
        app.entries().iter().map(|e| e.name.clone()).collect()
    }

    #[test]
    fn starts_at_root_sorted_by_size() {
        let (_t, app) = fixture();
        assert!(matches!(app.mode, Mode::Browse));
        assert_eq!(names(&app), vec!["big", "mid", "zeta.bin"]);
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn navigate_down_up_restores_cursor() {
        let (_t, mut app) = fixture();
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.cwd, app.root.join("big"));
        assert_eq!(names(&app), vec!["a.bin", "inner"]);
        // Left goes back with the cursor on "big".
        app.on_key(key(KeyCode::Char('j')));
        app.on_key(key(KeyCode::Left));
        assert_eq!(app.cwd, app.root);
        assert_eq!(app.current().unwrap().name, "big");
        // Up at the root is a no-op.
        app.on_key(key(KeyCode::Backspace));
        assert_eq!(app.cwd, app.root);
        // Enter on a file does not navigate.
        app.on_key(key(KeyCode::End));
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.cwd, app.root);
        assert!(app.status.contains("not a directory"));
    }

    #[test]
    fn cursor_is_clamped() {
        let (_t, mut app) = fixture();
        app.on_key(key(KeyCode::Up));
        assert_eq!(app.cursor, 0);
        app.on_key(key(KeyCode::PageDown));
        assert_eq!(app.cursor, 2);
        app.on_key(key(KeyCode::Char('g')));
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn sort_cycles() {
        let (_t, mut app) = fixture();
        app.on_key(key(KeyCode::Char('s')));
        assert_eq!(app.sort, SortKey::Name);
        assert_eq!(names(&app), vec!["big", "mid", "zeta.bin"]);
        app.on_key(key(KeyCode::Char('s')));
        assert_eq!(app.sort, SortKey::Modified);
        app.on_key(key(KeyCode::Char('s')));
        assert_eq!(app.sort, SortKey::Size);
    }

    #[test]
    fn mark_and_delete_flow() {
        let (_t, mut app) = fixture();
        app.on_key(key(KeyCode::Char(' '))); // big
        app.on_key(key(KeyCode::Char(' '))); // mid
        assert_eq!(app.marked.len(), 2);
        assert!(app.marked_bytes() >= 600_000);

        let eff = app.on_key(key(KeyCode::Char('d')));
        let Effect::CheckInUse(paths) = eff else {
            panic!("{eff:?}")
        };
        assert_eq!(paths.len(), 2);
        app.open_confirm(paths.clone(), vec![]);
        let Mode::Confirm(c) = &app.mode else {
            panic!()
        };
        assert_eq!(c.bytes, app.marked_bytes());

        // Unknown keys keep the dialog; Esc cancels.
        assert_eq!(app.on_key(key(KeyCode::Char('x'))), Effect::None);
        assert!(matches!(app.mode, Mode::Confirm(_)));
        app.on_key(key(KeyCode::Esc));
        assert!(matches!(app.mode, Mode::Browse));

        app.on_key(key(KeyCode::Char('d')));
        app.open_confirm(paths.clone(), vec![]);
        let eff = app.on_key(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::SHIFT));
        assert_eq!(
            eff,
            Effect::Delete {
                paths: paths.clone(),
                trash: false
            }
        );

        let cfg = app.cfg.clone();
        let outcome = app
            .tree
            .as_mut()
            .unwrap()
            .delete_paths(&paths, &cfg, false, |_, _, _| {});
        app.apply_outcome(&outcome);
        assert_eq!(names(&app), vec!["zeta.bin"]);
        assert!(app.marked.is_empty());
        assert!(app.status.contains("2 item(s) deleted"), "{}", app.status);
        assert!(!app.root.join("big").exists());
    }

    #[test]
    fn delete_without_marks_targets_cursor_and_t_trashes() {
        let (_t, mut app) = fixture();
        app.on_key(key(KeyCode::Char('j')));
        let eff = app.on_key(key(KeyCode::Delete));
        assert_eq!(eff, Effect::CheckInUse(vec![app.root.join("mid")]));
        app.open_confirm(vec![app.root.join("mid")], vec![]);
        assert_eq!(
            app.on_key(key(KeyCode::Char('t'))),
            Effect::Delete {
                paths: vec![app.root.join("mid")],
                trash: true
            }
        );
    }

    #[test]
    fn blocked_entries_cannot_be_marked_or_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        fs::create_dir_all(home.join("x")).unwrap();
        fs::write(home.join("x/f"), b"1").unwrap();
        let mut cfg = Config::default();
        cfg.home = home.clone();
        let tree = scan_tree(
            tmp.path(),
            &ScanOptions::default(),
            &AtomicBool::new(false),
            |_| {},
        );
        let mut app = App::new(tmp.path().to_path_buf(), cfg);
        app.set_tree(tree, None);
        assert_eq!(app.current().unwrap().name, "home");
        app.on_key(key(KeyCode::Char(' ')));
        assert!(app.marked.is_empty());
        assert!(app.status.contains("home"));
        app.cursor = 0;
        assert_eq!(app.on_key(key(KeyCode::Char('d'))), Effect::None);
        assert!(app.status.contains("cannot delete"));
    }

    #[test]
    fn quit_rescan_open_help() {
        let (_t, mut app) = fixture();
        assert_eq!(
            app.on_key(key(KeyCode::Char('o'))),
            Effect::Open(app.root.join("big"))
        );
        app.on_key(key(KeyCode::Char('?')));
        assert!(matches!(app.mode, Mode::Help));
        app.on_key(key(KeyCode::Char('x')));
        assert!(matches!(app.mode, Mode::Browse));
        assert_eq!(app.on_key(key(KeyCode::Char('r'))), Effect::Rescan);
        assert!(matches!(app.mode, Mode::Scanning));
        assert_eq!(app.on_key(key(KeyCode::Char('q'))), Effect::Quit);
        assert_eq!(
            app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Effect::Quit
        );
    }
}
