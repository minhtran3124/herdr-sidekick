//! The changed-files panel: a collapsible tree, refreshed in the background.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::Frame;

use super::git::{self, Change, Staged};
use super::watch::Watch;
use crate::tui::{spread, truncate, SIDE_LABELS};

const SEL_BG: Color = Color::Rgb(44, 47, 58);
/// Only following the focused pane is polled (one `pane list`, no git).
const FOCUS_EVERY: Duration = Duration::from_secs(1);
/// File events trigger scans; this catches what a watch can miss (a dropped event, a file in a
/// directory git did not know yet). Without a watcher, scans run every POLL_SCAN instead.
const SAFETY_SCAN: Duration = Duration::from_secs(30);
const POLL_SCAN: Duration = Duration::from_secs(2);

enum Row {
    /// `label` folds single-child directory chains, e.g. `apps/web/src`.
    Dir { path: String, label: String, depth: usize },
    File { idx: usize, depth: usize },
}

#[derive(Default)]
struct Tree {
    dirs: BTreeMap<String, Tree>,
    files: Vec<usize>,
}

/// One scan: the directory being followed, its checkout (None outside git) and its changes.
struct Snap {
    dir: PathBuf,
    root: Option<PathBuf>,
    branch: String,
    changes: Vec<Change>,
}

pub struct List {
    dir: PathBuf,
    root: Option<PathBuf>,
    branch: String,
    changes: Vec<Change>,
    loaded: bool,
    collapsed: HashSet<String>,
    rows: Vec<Row>,
    /// Selection is kept by path so it survives refreshes that reorder rows.
    sel: Option<String>,
    top: usize,
    follow: bool,
    height: usize,
    /// Screen y -> row index, from the last render.
    hits: Vec<(u16, usize)>,
    rx: Receiver<Snap>,
    kick: Sender<()>,
}

impl List {
    pub fn new(dir: PathBuf) -> Self {
        let (rx, kick) = spawn(dir.clone());
        List {
            dir,
            root: None,
            branch: String::new(),
            changes: Vec::new(),
            loaded: false,
            collapsed: HashSet::new(),
            rows: Vec::new(),
            sel: None,
            top: 0,
            follow: true,
            height: 0,
            hits: Vec::new(),
            rx,
            kick,
        }
    }

    /// For `--snapshot`: wait for the first scan instead of rendering "loading".
    pub fn wait_loaded(&mut self) {
        if let Ok(snap) = self.rx.recv() {
            self.apply(snap);
        }
    }

    pub fn tick(&mut self) {
        if let Some(snap) = self.rx.try_iter().last() {
            self.apply(snap);
        }
    }

    fn apply(&mut self, snap: Snap) {
        self.loaded = true;
        self.dir = snap.dir;
        self.branch = snap.branch;
        if snap.root != self.root {
            // Another checkout: folds and selection belonged to the old tree.
            self.root = snap.root;
            self.collapsed.clear();
            self.sel = None;
            self.top = 0;
            self.changes.clear();
            self.rebuild();
        }
        if snap.changes != self.changes {
            self.changes = snap.changes;
            self.rebuild();
        }
    }

    fn rebuild(&mut self) {
        let mut tree = Tree::default();
        for (i, c) in self.changes.iter().enumerate() {
            let mut t = &mut tree;
            let parts: Vec<&str> = c.path.split('/').collect();
            for d in &parts[..parts.len() - 1] {
                t = t.dirs.entry(d.to_string()).or_default();
            }
            t.files.push(i);
        }
        self.rows.clear();
        flatten(&tree, "", 0, &self.collapsed, &mut self.rows);
        if self.sel_idx().is_none() {
            self.sel = self.rows.first().map(|r| self.key(r));
        }
    }

    fn key(&self, r: &Row) -> String {
        match r {
            Row::Dir { path, .. } => format!("{path}/"),
            Row::File { idx, .. } => self.changes[*idx].path.clone(),
        }
    }

    fn sel_idx(&self) -> Option<usize> {
        let sel = self.sel.as_ref()?;
        self.rows.iter().position(|r| &self.key(r) == sel)
    }

    fn select(&mut self, i: usize) {
        if let Some(r) = self.rows.get(i) {
            self.sel = Some(self.key(r));
            self.follow = true;
        }
    }

    fn move_sel(&mut self, d: isize) {
        let i = self.sel_idx().unwrap_or(0) as isize + d;
        self.select(i.clamp(0, self.rows.len().saturating_sub(1) as isize) as usize);
    }

    fn activate(&mut self, i: usize) {
        match self.rows.get(i) {
            Some(Row::Dir { path, .. }) => {
                let p = path.clone();
                if !self.collapsed.remove(&p) {
                    self.collapsed.insert(p);
                }
                self.rebuild();
            }
            Some(Row::File { idx, .. }) => self.open_diff(&self.changes[*idx].path),
            None => {}
        }
    }

    /// `f`: the file picker overlay, rooted at this panel's checkout.
    fn open_file(&self) {
        if let Some(root) = &self.root {
            crate::tui::open_overlay("open", Some(root), None);
        }
    }

    fn open_diff(&self, path: &str) {
        if let Some(root) = &self.root {
            crate::tui::open_overlay("diff", Some(root), Some(format!("CHANGES_FILE={path}")));
        }
    }

    /// Returns false to quit.
    pub fn event(&mut self, e: Event) -> bool {
        match e {
            Event::Key(k) if k.kind == KeyEventKind::Press => match k.code {
                KeyCode::Char('q') => {
                    hide_everywhere();
                    return false;
                }
                KeyCode::Char('j') | KeyCode::Down => self.move_sel(1),
                KeyCode::Char('k') | KeyCode::Up => self.move_sel(-1),
                KeyCode::Char('g') | KeyCode::Home => self.select(0),
                KeyCode::Char('G') | KeyCode::End => self.select(self.rows.len().saturating_sub(1)),
                KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Char('l') | KeyCode::Right => {
                    if let Some(i) = self.sel_idx() {
                        self.activate(i);
                    }
                }
                KeyCode::Char('h') | KeyCode::Left => {
                    if let Some(Row::Dir { path, .. }) = self.sel_idx().and_then(|i| self.rows.get(i)) {
                        self.collapsed.insert(path.clone());
                        self.rebuild();
                    }
                }
                KeyCode::Char('f') => self.open_file(),
                KeyCode::Char('r') => {
                    let _ = self.kick.send(());
                }
                _ => {}
            },
            Event::Mouse(m) => match m.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(&(_, i)) = self.hits.iter().find(|(y, _)| *y == m.row) {
                        self.select(i);
                        self.activate(i);
                    }
                }
                MouseEventKind::ScrollDown => {
                    self.follow = false;
                    self.top = (self.top + 3).min(self.rows.len().saturating_sub(self.height));
                }
                MouseEventKind::ScrollUp => {
                    self.follow = false;
                    self.top = self.top.saturating_sub(3);
                }
                _ => {}
            },
            _ => {}
        }
        true
    }

    pub fn render(&mut self, f: &mut Frame) {
        let area = f.area();
        let w = area.width as usize;
        let buf = f.buffer_mut();
        let Some(root) = &self.root else {
            let msg = if self.loaded { " not a git repository" } else { " loading…" };
            buf.set_line(0, 0, &Line::from(msg.dark_gray()), area.width);
            let dir = format!(" {}", self.dir.display());
            buf.set_line(0, 1, &Line::from(truncate(&dir, w).dark_gray()), area.width);
            return;
        };

        let all = match () {
            _ if self.changes.is_empty() => Staged::No,
            _ if self.changes.iter().all(|c| c.staged == Staged::All) => Staged::All,
            _ if self.changes.iter().any(|c| c.staged != Staged::No) => Staged::Partly,
            _ => Staged::No,
        };
        let n = self.changes.len();
        let title = format!("{n} File{} Changed", if n == 1 { "" } else { "s" });
        let repo = root.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let head = vec![" ".into(), checkbox(all), " ".into(), title.bold()];
        let ctx = format!("{repo} · {}", if self.branch.is_empty() { "detached" } else { &self.branch });
        buf.set_line(0, 0, &spread(head, vec![truncate(&ctx, w / 2).dark_gray(), " ".into()], w), area.width);

        let body = Rect::new(0, 2, area.width, area.height.saturating_sub(3));
        self.height = body.height as usize;
        self.hits.clear();
        if !self.loaded || self.changes.is_empty() {
            let msg = if self.loaded { " working tree clean" } else { " loading…" };
            buf.set_line(0, 2, &Line::from(msg.dark_gray()), area.width);
        }

        let sel = self.sel_idx();
        if self.follow {
            if let Some(s) = sel {
                if s < self.top {
                    self.top = s;
                } else if s >= self.top + self.height {
                    self.top = s + 1 - self.height;
                }
            }
        }
        self.top = self.top.min(self.rows.len().saturating_sub(self.height));
        for (y, i) in (body.y..body.bottom()).zip(self.top..self.rows.len()) {
            let line = self.row_line(&self.rows[i], w);
            if Some(i) == sel {
                buf.set_style(Rect::new(0, y, area.width, 1), Style::new().bg(SEL_BG));
            }
            buf.set_line(0, y, &line, area.width);
            self.hits.push((y, i));
        }

        let help = " ↵ diff  f open file  r refresh  q hide";
        buf.set_line(0, area.height.saturating_sub(1), &Line::from(help.dark_gray()), area.width);
    }

    fn row_line(&self, r: &Row, w: usize) -> Line<'static> {
        match r {
            Row::Dir { path, label, depth } => {
                let chev = if self.collapsed.contains(path) { "›" } else { "⌄" };
                let indent = " ".repeat(1 + depth * 2);
                Line::from(vec![indent.into(), chev.dark_gray(), " ".into(), truncate(label, w.saturating_sub(4 + depth * 2)).into()])
            }
            Row::File { idx, depth } => {
                let c = &self.changes[*idx];
                let mut right: Vec<Span> = Vec::new();
                if c.binary {
                    right.push("bin ".dark_gray());
                } else {
                    if c.ins > 0 {
                        right.push(format!("+{}", c.ins).green());
                    }
                    if c.del > 0 {
                        right.push(format!(" -{}", c.del).red());
                    }
                    right.push(" ".into());
                }
                let (icon, color) = match c.status {
                    'A' | '?' => ("⊞", Color::Green),
                    'D' => ("⊟", Color::Red),
                    'U' => ("⊠", Color::Magenta),
                    _ => ("⊡", Color::Yellow),
                };
                right.push(Span::styled(icon, color));
                right.push(" ".into());
                let name = c.path.rsplit('/').next().unwrap_or(&c.path);
                let mut left = vec![" ".repeat(1 + depth * 2).into(), checkbox(c.staged), " ".into()];
                if let Some((glyph, color)) = file_icon(name) {
                    left.push(Span::styled(glyph, color));
                    left.push(" ".into());
                }
                let used: usize = left.iter().chain(&right).map(|s| s.width()).sum::<usize>() + 1;
                let name = truncate(name, w.saturating_sub(used));
                left.push(if c.status == 'D' { name.dark_gray().crossed_out() } else { name.into() });
                spread(left, right, w)
            }
        }
    }
}

fn flatten(t: &Tree, prefix: &str, depth: usize, collapsed: &HashSet<String>, out: &mut Vec<Row>) {
    for (name, sub) in &t.dirs {
        let (mut sub, mut path, mut label) = (sub, join(prefix, name), name.clone());
        while sub.files.is_empty() && sub.dirs.len() == 1 {
            let (n, s) = sub.dirs.iter().next().unwrap();
            path = join(&path, n);
            label = format!("{label}/{n}");
            sub = s;
        }
        let open = !collapsed.contains(&path);
        out.push(Row::Dir { path: path.clone(), label, depth });
        if open {
            flatten(sub, &path, depth + 1, collapsed, out);
        }
    }
    out.extend(t.files.iter().map(|&idx| Row::File { idx, depth }));
}

fn join(a: &str, b: &str) -> String {
    if a.is_empty() {
        b.to_string()
    } else {
        format!("{a}/{b}")
    }
}

fn checkbox(s: Staged) -> Span<'static> {
    match s {
        Staged::No => "☐".dark_gray(),
        Staged::Partly => "◪".cyan(),
        Staged::All => "☑".cyan(),
    }
}

/// Nerd Font file-type icon (Ghostty ships the glyphs). `CHANGES_ICONS=plain` turns them off.
fn file_icon(name: &str) -> Option<(&'static str, Color)> {
    if std::env::var("CHANGES_ICONS").as_deref() == Ok("plain") {
        return None;
    }
    let lower = name.to_ascii_lowercase();
    let ext = lower.rsplit_once('.').map_or("", |(_, e)| e);
    Some(match (lower.as_str(), ext) {
        ("makefile" | "justfile", _) | (_, "mk") => ("\u{f013}", Color::Gray),
        ("dockerfile", _) | (_, "dockerfile") => ("\u{f308}", Color::Blue),
        (n, _) if n.starts_with(".git") => ("\u{e702}", Color::Rgb(240, 80, 50)),
        (n, _) if n.starts_with(".env") => ("\u{e615}", Color::Yellow),
        (_, "json" | "jsonc") => ("\u{e60b}", Color::Yellow),
        (_, "ts" | "mts" | "cts") => ("\u{e628}", Color::Blue),
        (_, "tsx" | "jsx") => ("\u{e7ba}", Color::Cyan),
        (_, "js" | "mjs" | "cjs") => ("\u{e60c}", Color::Yellow),
        (_, "py") => ("\u{e606}", Color::Yellow),
        (_, "rs") => ("\u{e7a8}", Color::Rgb(222, 165, 132)),
        (_, "md" | "mdx") => ("\u{f48a}", Color::White),
        (_, "toml" | "yaml" | "yml" | "ini" | "cfg" | "conf") => ("\u{e615}", Color::Gray),
        (_, "sh" | "bash" | "zsh") => ("\u{e795}", Color::Green),
        (_, "html") => ("\u{e736}", Color::Rgb(228, 120, 80)),
        (_, "css" | "scss") => ("\u{e749}", Color::Blue),
        (_, "sql") => ("\u{e706}", Color::Gray),
        (_, "lock") => ("\u{f023}", Color::DarkGray),
        (_, "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "ico") => ("\u{f1c5}", Color::Magenta),
        _ => ("\u{ea7b}", Color::Gray),
    })
}


/// One worker per panel. It follows the focused pane every FOCUS_EVERY, and runs git only when
/// that pane moves to another directory, files change (watch.rs), `r` is pressed, or SAFETY_SCAN
/// passes. File events and `r` arrive on the same channel.
fn spawn(mut dir: PathBuf) -> (Receiver<Snap>, Sender<()>) {
    let (tx, rx) = mpsc::channel();
    let (kick, kicks) = mpsc::channel::<()>();
    let mut watch = Watch::new(kick.clone());
    std::thread::spawn(move || {
        let every = if watch.is_some() { SAFETY_SCAN } else { POLL_SCAN };
        let mut followed: Option<PathBuf> = None;
        let mut last: Option<Instant> = None;
        let mut dirty = true;
        loop {
            if let Some(d) = focused_cwd() {
                dir = d;
            }
            if followed.as_ref() != Some(&dir) {
                followed = Some(dir.clone());
                dirty = true;
            }
            let due = last.is_none_or(|t| t.elapsed() >= every);
            if dirty || due {
                dirty = false;
                last = Some(Instant::now());
                let root = git::toplevel(&dir);
                let (branch, changes) = match &root {
                    Some(r) => (git::branch(r), git::changes(r)),
                    None => (String::new(), Vec::new()),
                };
                if let Some(w) = watch.as_mut() {
                    w.sync(root.as_deref(), due);
                }
                if tx.send(Snap { dir: dir.clone(), root, branch, changes }).is_err() {
                    return;
                }
            }
            match crate::tui::wait_kick(&kicks, FOCUS_EVERY, last) {
                Some(kicked) => dirty |= kicked,
                None => return,
            }
        }
    });
    (rx, kick)
}

/// cwd of the focused pane when it is in this panel's tab and is not a side panel. None when
/// focus is in another tab or on the panel itself, so the list keeps its last directory.
fn focused_cwd() -> Option<PathBuf> {
    let me = std::env::var("HERDR_PANE_ID").ok()?;
    let panes = crate::tui::pane_list()?;
    let tab = crate::tui::tab_of(&panes, &me)?;
    let f = panes.iter().find(|p| p["focused"] == true && &p["tab_id"] == tab && p["pane_id"] != me.as_str())?;
    if f["label"].as_str().is_some_and(|l| SIDE_LABELS.contains(&l)) {
        return None;
    }
    f["foreground_cwd"].as_str().or(f["cwd"].as_str()).map(PathBuf::from)
}

fn hide_everywhere() {
    crate::tui::hide_everywhere(super::LABEL, "changes");
}
