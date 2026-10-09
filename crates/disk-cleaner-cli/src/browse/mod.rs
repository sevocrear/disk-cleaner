//! `disk-cleaner browse`: ncdu-style interactive disk usage browser.

pub mod app;
pub mod ui;

use app::{App, Effect, Mode};
use disk_cleaner_engine::config::Config;
use disk_cleaner_engine::inuse::find_in_use;
use disk_cleaner_engine::space::{space_accounting, SpaceAccounting};
use disk_cleaner_engine::tree::{scan_tree, ScanOptions, SizeTree, TreeScanProgress};
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

type ScanResult = (SizeTree, Option<SpaceAccounting>);

fn start_scan(
    root: PathBuf,
    cfg: Config,
    progress: Arc<Mutex<TreeScanProgress>>,
) -> Receiver<ScanResult> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let opts = ScanOptions {
            cross_fs: cfg.cross_fs,
            ..Default::default()
        };
        let tree = scan_tree(&root, &opts, &AtomicBool::new(false), |p| {
            if let Ok(mut g) = progress.lock() {
                *g = p;
            }
        });
        let acc = space_accounting(&root, tree.total_bytes(), tree.unreadable_dirs, &cfg);
        let _ = tx.send((tree, acc));
    });
    rx
}

pub fn run(path: Option<PathBuf>, cfg: Config) -> Result<(), String> {
    let root = path
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")))
        .canonicalize()
        .map_err(|e| format!("cannot open path: {e}"))?;
    if !root.is_dir() {
        return Err(format!("not a directory: {}", root.display()));
    }

    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, root, cfg);
    ratatui::restore();
    result
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    root: PathBuf,
    cfg: Config,
) -> Result<(), String> {
    let mut app = App::new(root.clone(), cfg.clone());
    let progress = Arc::new(Mutex::new(TreeScanProgress::default()));
    let mut pending = Some(start_scan(root.clone(), cfg.clone(), progress.clone()));

    loop {
        if let Some(rx) = &pending {
            if let Ok((tree, acc)) = rx.try_recv() {
                app.set_tree(tree, acc);
                pending = None;
            }
        }
        let snapshot = progress.lock().map(|p| p.clone()).unwrap_or_default();
        terminal
            .draw(|f| ui::draw(f, &mut app, &snapshot))
            .map_err(|e| e.to_string())?;

        if !event::poll(Duration::from_millis(120)).map_err(|e| e.to_string())? {
            continue;
        }
        let Event::Key(key) = event::read().map_err(|e| e.to_string())? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match app.on_key(key) {
            Effect::None => {}
            Effect::Quit => return Ok(()),
            Effect::CheckInUse(paths) => {
                app.status = "checking running processes…".into();
                let snapshot = progress.lock().map(|p| p.clone()).unwrap_or_default();
                let _ = terminal.draw(|f| ui::draw(f, &mut app, &snapshot));
                let in_use = find_in_use(&paths);
                app.status.clear();
                app.open_confirm(paths, in_use);
            }
            Effect::Delete { paths, trash } => {
                app.mode = Mode::Browse;
                app.status = format!("deleting {} item(s)…", paths.len());
                let snapshot = progress.lock().map(|p| p.clone()).unwrap_or_default();
                let _ = terminal.draw(|f| ui::draw(f, &mut app, &snapshot));
                let cfg = app.cfg.clone();
                if let Some(tree) = app.tree.as_mut() {
                    let outcome = tree.delete_paths(&paths, &cfg, trash, |_, _, _| {});
                    app.apply_outcome(&outcome);
                }
            }
            Effect::Rescan => {
                if let Ok(mut p) = progress.lock() {
                    *p = TreeScanProgress::default();
                }
                pending = Some(start_scan(root.clone(), cfg.clone(), progress.clone()));
            }
            Effect::Open(p) => {
                let _ = std::process::Command::new("xdg-open")
                    .arg(&p)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();
            }
        }
    }
}
