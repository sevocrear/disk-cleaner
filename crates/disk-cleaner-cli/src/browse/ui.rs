//! Rendering for `disk-cleaner browse`.

use super::app::{App, Mode};
use disk_cleaner_engine::tree::{NodeKind, TreeEntry, TreeScanProgress};
use disk_cleaner_engine::util::format_bytes;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Row, Table, TableState, Wrap};
use ratatui::Frame;

const BAR_WIDTH: usize = 12;

pub fn bar(fraction: f64, width: usize) -> String {
    let filled = ((fraction.clamp(0.0, 1.0) * width as f64).round() as usize).min(width);
    format!("{}{}", "█".repeat(filled), "·".repeat(width - filled))
}

fn flags(e: &TreeEntry) -> String {
    let mut f = Vec::new();
    if e.unreadable {
        f.push("unreadable");
    }
    if e.protected {
        f.push("protected");
    }
    if e.kind == NodeKind::OtherFs {
        f.push("mount");
    } else if e.blocked.is_some() && e.kind != NodeKind::Collapsed {
        f.push("locked");
    }
    f.join(" ")
}

fn display_name(e: &TreeEntry) -> String {
    match e.kind {
        NodeKind::Dir => format!("{}/", e.name),
        NodeKind::Symlink => format!("{}@", e.name),
        _ => e.name.clone(),
    }
}

pub fn draw(f: &mut Frame, app: &mut App, progress: &TreeScanProgress) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(2),
    ])
    .areas(f.area());

    draw_header(f, app, header);
    match &app.mode {
        Mode::Scanning => draw_scanning(f, app, progress, body),
        _ => draw_list(f, app, body),
    }
    draw_footer(f, app, footer);

    match &app.mode {
        Mode::Confirm(_) => draw_confirm(f, app),
        Mode::Help => draw_help(f),
        _ => {}
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let title = Line::from(vec![
        Span::styled(
            " disk-cleaner browse ",
            Style::new().fg(Color::Black).bg(Color::Cyan),
        ),
        Span::raw(" "),
        Span::styled(
            app.cwd.display().to_string(),
            Style::new().add_modifier(Modifier::BOLD),
        ),
    ]);
    let mut info = String::new();
    if let Some(acc) = &app.accounting {
        info.push_str(&format!(
            " {}: {} used of {} · {} free",
            acc.fs.mount_point,
            format_bytes(acc.fs.used_bytes),
            format_bytes(acc.fs.total_bytes),
            format_bytes(acc.fs.available_bytes),
        ));
        if acc.is_mount_root && acc.unaccounted_bytes > 0 {
            let parts: Vec<String> = acc
                .sources
                .iter()
                .map(|s| format!("{} {}", s.label, format_bytes(s.bytes)))
                .collect();
            info.push_str(&format!(
                " · not visible to scan: {}{}",
                format_bytes(acc.unaccounted_bytes),
                if parts.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", parts.join(", "))
                }
            ));
        }
    } else if let Some(l) = &app.listing {
        info = format!(" {} in {} items", format_bytes(l.bytes), l.items);
    }
    f.render_widget(
        Paragraph::new(vec![
            title,
            Line::styled(info, Style::new().fg(Color::Gray)),
        ]),
        area,
    );
}

fn draw_scanning(f: &mut Frame, app: &App, p: &TreeScanProgress, area: Rect) {
    let text = vec![
        Line::from(format!("Scanning {} …", app.root.display())),
        Line::from(format!(
            "{} files · {} dirs · {}",
            p.files,
            p.dirs,
            format_bytes(p.bytes)
        )),
        Line::styled(p.current.clone(), Style::new().fg(Color::DarkGray)),
        Line::from(""),
        Line::styled("q to quit", Style::new().fg(Color::DarkGray)),
    ];
    f.render_widget(
        Paragraph::new(text).block(Block::new().borders(Borders::TOP)),
        area,
    );
}

fn draw_list(f: &mut Frame, app: &mut App, area: Rect) {
    let parent_bytes = app.listing.as_ref().map(|l| l.bytes).unwrap_or(0).max(1);
    let rows: Vec<Row> = app
        .entries()
        .iter()
        .map(|e| {
            let marked = app.marked.contains(std::path::Path::new(&e.path));
            let frac = e.bytes as f64 / parent_bytes as f64;
            let style = if e.blocked.is_some() || e.kind == NodeKind::Collapsed {
                Style::new().fg(Color::DarkGray)
            } else if e.kind == NodeKind::Dir {
                Style::new().fg(Color::LightBlue)
            } else {
                Style::new()
            };
            Row::new(vec![
                if marked {
                    "*".to_string()
                } else {
                    " ".to_string()
                },
                format!("{:>9}", format_bytes(e.bytes)),
                format!("{:>5.1}% {}", frac * 100.0, bar(frac, BAR_WIDTH)),
                if e.kind == NodeKind::Dir || e.kind == NodeKind::Collapsed {
                    format!("{:>7}", e.items)
                } else {
                    String::new()
                },
                display_name(e),
                flags(e),
            ])
            .style(if marked {
                style.fg(Color::Yellow)
            } else {
                style
            })
        })
        .collect();

    let empty = rows.is_empty();
    let table = Table::new(
        rows,
        [
            Constraint::Length(1),
            Constraint::Length(9),
            Constraint::Length(7 + BAR_WIDTH as u16),
            Constraint::Length(7),
            Constraint::Fill(1),
            Constraint::Length(20),
        ],
    )
    .header(
        Row::new(vec!["", "     size", "  share", "  items", "name", ""])
            .style(Style::new().add_modifier(Modifier::DIM)),
    )
    .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
    .block(Block::new().borders(Borders::TOP));

    app.page = area.height.saturating_sub(3).max(1) as usize;
    let mut state =
        TableState::default().with_selected(if empty { None } else { Some(app.cursor) });
    f.render_stateful_widget(table, area, &mut state);
    if empty {
        let msg = if app.listing.as_ref().is_some_and(|l| l.unreadable) {
            "  (cannot read this directory)"
        } else {
            "  (empty)"
        };
        let inner = Rect {
            y: area.y + 2,
            height: 1,
            ..area
        };
        f.render_widget(Paragraph::new(msg), inner);
    }
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let keys = "↑↓ move  →/Enter open  ←/Bksp up  Space mark  d delete  s sort  r rescan  o open  ? help  q quit";
    let mut status = app.status.clone();
    if !app.marked.is_empty() {
        let m = format!(
            "{} marked · {}",
            app.marked.len(),
            format_bytes(app.marked_bytes())
        );
        status = if status.is_empty() {
            m
        } else {
            format!("{m} · {status}")
        };
    }
    f.render_widget(
        Paragraph::new(vec![
            Line::styled(status, Style::new().fg(Color::Yellow)),
            Line::styled(
                format!("{keys}  · sort: {}", app.sort.label()),
                Style::new().fg(Color::DarkGray),
            ),
        ]),
        area,
    );
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(2));
    let h = h.min(area.height.saturating_sub(2));
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

fn draw_confirm(f: &mut Frame, app: &App) {
    let Mode::Confirm(c) = &app.mode else { return };
    let mut lines = vec![
        Line::styled(
            format!(
                "Delete {} item(s), {}?",
                c.paths.len(),
                format_bytes(c.bytes)
            ),
            Style::new().add_modifier(Modifier::BOLD),
        ),
        Line::from(""),
    ];
    for p in c.paths.iter().take(6) {
        // Paths below the current dir are shown relative so they fit.
        let shown = p.strip_prefix(&app.cwd).unwrap_or(p);
        lines.push(Line::from(format!("  {}", shown.display())));
    }
    if c.paths.len() > 6 {
        lines.push(Line::from(format!("  … and {} more", c.paths.len() - 6)));
    }
    if !c.in_use.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::styled(
            "In use by running processes:",
            Style::new().fg(Color::Red),
        ));
        for u in c.in_use.iter().take(4) {
            let procs: Vec<String> = u
                .processes
                .iter()
                .map(|p| format!("{} ({}, {})", p.name, p.pid, p.how))
                .collect();
            lines.push(Line::styled(
                format!("  {} ← {}", u.path, procs.join(", ")),
                Style::new().fg(Color::Red),
            ));
        }
    }
    if !c.protected.is_empty() {
        lines.push(Line::styled(
            format!(
                "{} protected item(s) (caches/models you may want to keep)",
                c.protected.len()
            ),
            Style::new().fg(Color::Yellow),
        ));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("[t]", Style::new().fg(Color::Cyan)),
        Span::raw(" Move to Trash   "),
        Span::styled("[D]", Style::new().fg(Color::Red)),
        Span::raw(" Delete permanently   "),
        Span::styled("[Esc]", Style::new().fg(Color::Cyan)),
        Span::raw(" Cancel"),
    ]));
    let area = centered(f.area(), 90, 0);
    let inner_w = area.width.saturating_sub(2).max(1) as usize;
    // Height must include wrapped rows, or the key hints at the bottom get cut off.
    let rows: usize = lines
        .iter()
        .map(|l| l.width().max(1).div_ceil(inner_w))
        .sum();
    let area = centered(f.area(), 90, rows as u16 + 2);
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::bordered().title(" Confirm delete ")),
        area,
    );
}

fn draw_help(f: &mut Frame) {
    let lines: Vec<Line> = [
        "↑/k ↓/j      move            PgUp/PgDn  page",
        "g / G        first / last",
        "→ l Enter    open directory  ← h Bksp   parent",
        "Space        mark / unmark   u          clear marks",
        "d / Del      delete marked (or current) — asks first",
        "s            sort: size → name → modified",
        "r            rescan          o          open in file manager",
        "q / Esc      quit",
        "",
        "Sizes are disk usage; hard links count once. 'locked' items",
        "(system paths, your home, mount points) cannot be deleted.",
    ]
    .into_iter()
    .map(Line::from)
    .collect();
    let area = centered(f.area(), 66, lines.len() as u16 + 2);
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" Keys ")),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browse::app::tests::{fixture, key};
    use disk_cleaner_engine::inuse::{InUse, ProcessUse};
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::KeyCode;
    use ratatui::Terminal;

    fn render(app: &mut App, progress: &TreeScanProgress) -> String {
        let mut term = Terminal::new(TestBackend::new(120, 24)).unwrap();
        term.draw(|f| draw(f, app, progress)).unwrap();
        let buf = term.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn bar_widths() {
        assert_eq!(bar(0.0, 4), "····");
        assert_eq!(bar(0.5, 4), "██··");
        assert_eq!(bar(2.0, 4), "████");
    }

    #[test]
    fn renders_listing_with_sizes_and_keys() {
        let (_t, mut app) = fixture();
        let screen = render(&mut app, &TreeScanProgress::default());
        assert!(screen.contains("disk-cleaner browse"));
        assert!(screen.contains("big/"));
        assert!(screen.contains("mid/"));
        assert!(screen.contains("zeta.bin"));
        assert!(screen.contains('%'));
        assert!(screen.contains("d delete"));
        // Rows are ordered by size.
        assert!(screen.find("big/").unwrap() < screen.find("zeta.bin").unwrap());
    }

    #[test]
    fn renders_marks_and_confirm_dialog_with_in_use() {
        let (_t, mut app) = fixture();
        app.on_key(key(KeyCode::Char(' ')));
        let screen = render(&mut app, &TreeScanProgress::default());
        assert!(screen.contains("1 marked"));

        let p = app.root.join("big");
        app.open_confirm(
            vec![p.clone()],
            vec![InUse {
                path: p.to_string_lossy().into_owned(),
                processes: vec![ProcessUse {
                    pid: 42,
                    name: "uvx".into(),
                    how: "cwd".into(),
                }],
            }],
        );
        let screen = render(&mut app, &TreeScanProgress::default());
        assert!(screen.contains("Confirm delete"));
        assert!(screen.contains("Delete 1 item(s)"));
        assert!(screen.contains("In use by running processes"));
        assert!(screen.contains("uvx (42, cwd)"));
        assert!(screen.contains("[t] Move to Trash"));
        assert!(screen.contains("[D] Delete permanently"));
    }

    #[test]
    fn confirm_dialog_keeps_key_hints_with_long_paths() {
        let (_t, mut app) = fixture();
        let long = std::path::PathBuf::from(format!(
            "/elsewhere/{}/file.bin",
            "very-long-segment".repeat(12)
        ));
        app.open_confirm(vec![long, app.root.join("big")], vec![]);
        let screen = render(&mut app, &TreeScanProgress::default());
        assert!(screen.contains("[Esc] Cancel"), "{screen}");
        // In-tree paths are shown relative to the current directory.
        assert!(screen.contains("  big"));
    }

    #[test]
    fn renders_scanning_progress_and_help() {
        let (_t, mut app) = fixture();
        app.mode = Mode::Scanning;
        let p = TreeScanProgress {
            files: 1234,
            dirs: 56,
            bytes: 10 * 1024 * 1024,
            current: "/x/y".into(),
        };
        let screen = render(&mut app, &p);
        assert!(screen.contains("Scanning"));
        assert!(screen.contains("1234 files · 56 dirs · 10.0MB"));
        assert!(screen.contains("/x/y"));

        app.mode = Mode::Help;
        let screen = render(&mut app, &p);
        assert!(screen.contains(" Keys "));
        assert!(screen.contains("mark / unmark"));
    }

    #[test]
    fn empty_dir_message() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = disk_cleaner_engine::config::Config::default();
        cfg.home = tmp.path().join("h");
        let tree = disk_cleaner_engine::tree::scan_tree(
            tmp.path(),
            &Default::default(),
            &std::sync::atomic::AtomicBool::new(false),
            |_| {},
        );
        let mut app = App::new(tmp.path().to_path_buf(), cfg);
        app.set_tree(tree, None);
        let screen = render(&mut app, &TreeScanProgress::default());
        assert!(screen.contains("(empty)"));
    }
}
