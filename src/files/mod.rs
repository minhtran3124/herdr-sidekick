//! Open file: type a repo-relative path (fuzzy over `git ls-files`), view it highlighted.
//!   sidekick open [PATH[:LINE]]   picker overlay; with PATH (or $OPEN_PATH) straight to the file
//! PATH may also be relative to the pane's directory, absolute, or a `file://` URL (Ctrl+click).

mod fuzzy;

use std::path::PathBuf;
use std::time::Duration;

use ratatui::crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::Frame;
use two_face::re_exports::syntect::parsing::SyntaxSet;

use crate::changes::git;
use crate::highlight::{highlight, shift, Styled};
use crate::tui::{self, spread, truncate, Screen};

const MAX_RESULTS: usize = 200;
const TARGET_BG: Color = Color::Rgb(52, 50, 30);
const SEL_BG: Color = Color::Rgb(44, 47, 58);

pub fn open_main(mut args: Vec<String>) -> std::io::Result<()> {
    let snapshot = tui::take_snapshot_arg(&mut args);
    let input = args.first().cloned().or_else(|| std::env::var("OPEN_PATH").ok()).unwrap_or_default();
    let mut o = Open::new(std::env::current_dir()?, &input);
    if snapshot.is_none() {
        tui::resync_size();
    }
    match snapshot {
        Some(size) => tui::print_snapshot(&mut o, &size, (100, 30)),
        None => tui::run(&mut o, Duration::from_millis(250)),
    }
}

/// What the user typed, split into the path and an optional `:line` (and `:col`, ignored).
fn parse_target(input: &str) -> (String, Option<usize>) {
    let s = input.trim().trim_start_matches("file://");
    let mut parts = s.rsplitn(3, ':').collect::<Vec<_>>();
    parts.reverse();
    // path:line:col, path:line, or just path. A ':' inside the path stays part of it.
    let nums: Vec<Option<usize>> = parts.iter().skip(1).map(|p| p.parse().ok()).collect();
    match (parts.len(), nums.as_slice()) {
        (3, [Some(l), Some(_)]) => (parts[0].to_string(), Some(*l)),
        (3, [_, Some(l)]) => (format!("{}:{}", parts[0], parts[1]), Some(*l)),
        (2, [Some(l)]) => (parts[0].to_string(), Some(*l)),
        // A trailing ':' is a line number still being typed.
        _ => (s.trim_end_matches(':').to_string(), None),
    }
}

enum Prompt {
    Goto(String),
    Search(String),
}

struct Viewer {
    path: String,
    lines: Vec<Styled>,
    raw: Vec<String>,
    binary: bool,
    target: Option<usize>,
    top: usize,
    left: usize,
    height: usize,
    search: String,
    prompt: Option<Prompt>,
    /// Put `target` a third of the way down once the height is known (first render).
    center: bool,
}

pub struct Open {
    root: PathBuf,
    cwd: PathBuf,
    files: Vec<String>,
    /// `files` lowercased once, for matching on every keystroke.
    lower: Vec<String>,
    query: String,
    matches: Vec<(usize, Vec<usize>)>,
    sel: usize,
    /// Loaded on the first file shown, so the picker paints without waiting for it.
    syntaxes: Option<SyntaxSet>,
    view: Option<Viewer>,
    /// Opened straight to a file (argument or click): Esc closes instead of returning to the list.
    direct: bool,
}

impl Open {
    pub fn new(cwd: PathBuf, input: &str) -> Self {
        let root = git::toplevel(&cwd).unwrap_or_else(|| cwd.clone());
        let mut files = git::ls_files(&root);
        files.sort();
        files.dedup();
        let lower = files.iter().map(|f| f.to_lowercase()).collect();
        let mut o = Open {
            files,
            lower,
            root,
            cwd,
            query: String::new(),
            matches: Vec::new(),
            sel: 0,
            syntaxes: None,
            view: None,
            direct: false,
        };
        if !input.trim().is_empty() {
            o.query = input.trim().trim_start_matches("file://").to_string();
            let (path, line) = parse_target(input);
            if let Some(rel) = o.resolve(&path) {
                o.direct = true;
                o.show(rel, line);
            }
        }
        o.refilter();
        o
    }

    /// Repo-relative path for `p` when it names an existing file: as given from the repo root,
    /// from the pane's directory, or absolute inside the repo.
    fn resolve(&self, p: &str) -> Option<String> {
        let p = p.trim_start_matches("./");
        // `join` keeps an absolute `p` as is, so this covers absolute paths too.
        let candidates = [self.root.join(p), self.cwd.join(p)];
        let hit = candidates.into_iter().find(|c| c.is_file())?;
        let hit = hit.canonicalize().unwrap_or(hit);
        let root = self.root.canonicalize().unwrap_or_else(|_| self.root.clone());
        Some(hit.strip_prefix(&root).map(|r| r.to_string_lossy().into_owned()).unwrap_or_else(|_| hit.to_string_lossy().into_owned()))
    }

    fn refilter(&mut self) {
        let (path, _) = parse_target(&self.query);
        let q = path.trim_start_matches("./");
        self.matches = fuzzy::rank(q, &self.lower, MAX_RESULTS);
        self.sel = 0;
    }

    fn show(&mut self, rel: String, line: Option<usize>) {
        let full = self.root.join(&rel);
        let full = if full.is_file() { full } else { PathBuf::from(&rel) };
        let bytes = std::fs::read(&full).unwrap_or_default();
        let binary = git::is_binary(&bytes);
        let text = String::from_utf8(bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
        let raw: Vec<String> = if binary { Vec::new() } else { text.lines().map(String::from).collect() };
        let ss = self.syntaxes.get_or_insert_with(two_face::syntax::extra_newlines);
        let lines = if binary { Vec::new() } else { highlight(ss, &rel, &text) };
        let target = line.map(|l| l.saturating_sub(1).min(raw.len().saturating_sub(1)));
        self.view = Some(Viewer {
            path: rel,
            lines,
            raw,
            binary,
            target,
            top: 0,
            left: 0,
            height: 0,
            search: String::new(),
            prompt: None,
            center: target.is_some(),
        });
    }

    fn open_selected(&mut self) {
        let (_, line) = parse_target(&self.query);
        if let Some((i, _)) = self.matches.get(self.sel) {
            let rel = self.files[*i].clone();
            self.show(rel, line);
        }
    }

    fn step(&mut self, d: isize) {
        self.sel = (self.sel as isize + d).clamp(0, self.matches.len().saturating_sub(1) as isize) as usize;
    }

    /// Returns false to quit.
    fn key_pick(&mut self, code: KeyCode, mods: KeyModifiers) -> bool {
        match code {
            KeyCode::Esc => return false,
            KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => return false,
            KeyCode::Enter => self.open_selected(),
            KeyCode::Down | KeyCode::Tab => self.step(1),
            KeyCode::Char('n') if mods.contains(KeyModifiers::CONTROL) => self.step(1),
            KeyCode::Up | KeyCode::BackTab => self.step(-1),
            KeyCode::Char('p') if mods.contains(KeyModifiers::CONTROL) => self.step(-1),
            KeyCode::Char('u') if mods.contains(KeyModifiers::CONTROL) => {
                self.query.clear();
                self.refilter();
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.refilter();
            }
            KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) => {
                self.query.push(c);
                self.refilter();
            }
            _ => {}
        }
        true
    }

    fn key_view(&mut self, code: KeyCode, mods: KeyModifiers) -> bool {
        let direct = self.direct;
        let Some(v) = self.view.as_mut() else { return true };
        if let Some(p) = v.prompt.as_mut() {
            let buf = match p {
                Prompt::Goto(s) | Prompt::Search(s) => s,
            };
            match code {
                KeyCode::Esc => v.prompt = None,
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Char(c) => buf.push(c),
                KeyCode::Enter => v.submit(),
                _ => {}
            }
            return true;
        }
        let page = v.height.saturating_sub(2).max(1) as isize;
        match code {
            KeyCode::Char('q') => return false,
            KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => return false,
            KeyCode::Esc => {
                if direct {
                    return false;
                }
                self.view = None;
            }
            KeyCode::Char('j') | KeyCode::Down => v.scroll(1),
            KeyCode::Char('k') | KeyCode::Up => v.scroll(-1),
            KeyCode::Char('d') | KeyCode::PageDown | KeyCode::Char(' ') => v.scroll(page),
            KeyCode::Char('u') | KeyCode::PageUp => v.scroll(-page),
            KeyCode::Char('g') | KeyCode::Home => v.top = 0,
            KeyCode::Char('G') | KeyCode::End => v.scroll(isize::MAX / 2),
            KeyCode::Char('h') | KeyCode::Left => v.left = v.left.saturating_sub(8),
            KeyCode::Char('l') | KeyCode::Right => v.left += 8,
            KeyCode::Char(':') => v.prompt = Some(Prompt::Goto(String::new())),
            KeyCode::Char('/') => v.prompt = Some(Prompt::Search(String::new())),
            KeyCode::Char('n') => v.find(true, false),
            KeyCode::Char('N') => v.find(false, false),
            _ => {}
        }
        true
    }

    fn render_pick(&mut self, f: &mut Frame) {
        let area = f.area();
        let w = area.width as usize;
        let buf = f.buffer_mut();
        let repo = self.root.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let head = vec![" ".into(), "Open file".bold()];
        buf.set_line(0, 0, &spread(head, vec![format!("{repo} ").dark_gray()], w), area.width);
        let input = vec![" ".into(), "› ".cyan(), self.query.clone().into(), "█".dark_gray()];
        buf.set_line(0, 1, &Line::from(input), area.width);
        let count = if self.files.is_empty() {
            " no files (not a git repository?)".to_string()
        } else {
            format!(" {} of {} · type a path, path:line jumps to the line", self.matches.len(), self.files.len())
        };
        buf.set_line(0, 2, &Line::from(truncate(&count, w).dark_gray()), area.width);

        let body = Rect::new(0, 3, area.width, area.height.saturating_sub(4));
        let top = self.sel.saturating_sub(body.height.saturating_sub(1) as usize);
        for (y, (k, (i, hits))) in (body.y..body.bottom()).zip(self.matches.iter().enumerate().skip(top)) {
            let path = &self.files[*i];
            let mut spans: Vec<Span> = vec![if k == self.sel { " ▸ ".cyan() } else { "   ".into() }];
            for (ci, c) in path.chars().enumerate() {
                let s = c.to_string();
                spans.push(if hits.contains(&ci) { s.yellow().bold() } else { s.into() });
            }
            if k == self.sel {
                buf.set_style(Rect::new(0, y, area.width, 1), Style::new().bg(SEL_BG));
            }
            buf.set_line(0, y, &Line::from(spans), area.width);
        }
        let help = " ↵ open  ↑↓ select  ^U clear  esc close";
        buf.set_line(0, area.height.saturating_sub(1), &Line::from(help.dark_gray()), area.width);
    }

    fn render_view(&mut self, f: &mut Frame) {
        let area = f.area();
        let w = area.width as usize;
        let buf = f.buffer_mut();
        let Some(v) = self.view.as_mut() else { return };
        let pos = v.target.map(|t| format!(":{}", t + 1)).unwrap_or_default();
        let head = vec![" ".into(), v.path.clone().bold(), pos.yellow()];
        let right = vec![format!("{} lines ", v.raw.len()).dark_gray()];
        buf.set_line(0, 0, &spread(head, right, w), area.width);

        let body = Rect::new(0, 1, area.width, area.height.saturating_sub(2));
        v.height = body.height as usize;
        if v.center {
            v.center = false;
            v.top = v.target.unwrap_or(0).saturating_sub(v.height / 3);
        }
        if v.binary {
            buf.set_line(1, 2, &Line::from("binary file, not shown".dark_gray()), area.width);
        }
        v.scroll(0);
        let num_w = v.raw.len().max(1).to_string().len();
        for (y, i) in (body.y..body.bottom()).zip(v.top..v.lines.len()) {
            let is_target = v.target == Some(i);
            if is_target {
                buf.set_style(Rect::new(0, y, area.width, 1), Style::new().bg(TARGET_BG));
            }
            let num = Span::styled(format!(" {:>num_w$}  ", i + 1), if is_target { Color::Yellow } else { Color::DarkGray });
            let mut spans = vec![num];
            spans.extend(shift(&v.lines[i], v.left));
            buf.set_line(0, y, &Line::from(spans), area.width);
        }

        let foot = match &v.prompt {
            Some(Prompt::Goto(s)) => Line::from(vec![" go to line: ".yellow(), s.clone().into(), "█".dark_gray()]),
            Some(Prompt::Search(s)) => Line::from(vec![" /".yellow(), s.clone().into(), "█".dark_gray()]),
            None => {
                let back = if self.direct { "" } else { "esc back  " };
                Line::from(format!(" j/k scroll  : line  / find  n/N next  ←→ scroll  {back}q close").dark_gray())
            }
        };
        buf.set_line(0, area.height.saturating_sub(1), &foot, area.width);
    }
}

impl Viewer {
    /// Enter in the `:` or `/` prompt.
    fn submit(&mut self) {
        match self.prompt.take() {
            Some(Prompt::Goto(s)) => {
                if let Ok(n) = s.trim().parse::<usize>() {
                    let t = n.saturating_sub(1).min(self.raw.len().saturating_sub(1));
                    self.target = Some(t);
                    self.top = t.saturating_sub(self.height / 2);
                }
            }
            Some(Prompt::Search(s)) => {
                self.search = s;
                self.find(true, true);
            }
            None => {}
        }
    }

    fn scroll(&mut self, d: isize) {
        let max = self.lines.len().saturating_sub(self.height.max(1));
        self.top = (self.top as isize + d).clamp(0, max as isize) as usize;
    }

    /// Moves `target` to the next (or previous) line containing `search`, case-insensitive.
    /// `from_top` starts at the screen's first line instead of after the current match.
    fn find(&mut self, forward: bool, from_top: bool) {
        if self.search.is_empty() || self.raw.is_empty() {
            return;
        }
        let needle = self.search.to_lowercase();
        let n = self.raw.len();
        let start = match (from_top, self.target) {
            (true, _) | (false, None) => self.top,
            (false, Some(t)) => {
                if forward {
                    t + 1
                } else {
                    t + n - 1
                }
            }
        };
        let hit = (0..n)
            .map(|k| if forward { (start + k) % n } else { (start + n - k) % n })
            .find(|&i| self.raw[i].to_lowercase().contains(&needle));
        if let Some(i) = hit {
            self.target = Some(i);
            if i < self.top || i >= self.top + self.height {
                self.top = i.saturating_sub(self.height / 2);
            }
        }
    }
}

impl Screen for Open {
    fn tick(&mut self) {}

    fn render(&mut self, f: &mut Frame) {
        if self.view.is_some() {
            self.render_view(f)
        } else {
            self.render_pick(f)
        }
    }

    fn event(&mut self, e: Event) -> bool {
        match e {
            Event::Key(k) if k.kind == KeyEventKind::Press => {
                if self.view.is_some() {
                    self.key_view(k.code, k.modifiers)
                } else {
                    self.key_pick(k.code, k.modifiers)
                }
            }
            Event::Mouse(m) => {
                if let Some(v) = self.view.as_mut() {
                    match m.kind {
                        MouseEventKind::ScrollDown => v.scroll(3),
                        MouseEventKind::ScrollUp => v.scroll(-3),
                        MouseEventKind::ScrollRight => v.left += 8,
                        MouseEventKind::ScrollLeft => v.left = v.left.saturating_sub(8),
                        _ => {}
                    }
                } else {
                    match m.kind {
                        MouseEventKind::ScrollDown => self.step(1),
                        MouseEventKind::ScrollUp => self.step(-1),
                        _ => {}
                    }
                }
                true
            }
            _ => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_target;

    #[test]
    fn path_line_and_col_suffixes_are_split_off() {
        assert_eq!(parse_target("src/a.rs"), ("src/a.rs".into(), None));
        assert_eq!(parse_target("src/a.rs:42"), ("src/a.rs".into(), Some(42)));
        assert_eq!(parse_target("src/a.rs:42:7"), ("src/a.rs".into(), Some(42)));
        assert_eq!(parse_target("file:///repo/src/a.rs:3"), ("/repo/src/a.rs".into(), Some(3)));
        // A colon that is not followed by numbers stays in the path.
        assert_eq!(parse_target("weird:name.txt"), ("weird:name.txt".into(), None));
        // Typing the ':' before the line number keeps the list on the file.
        assert_eq!(parse_target("src/a.rs:"), ("src/a.rs".into(), None));
    }
}
