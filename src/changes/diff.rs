//! Full-file diff against HEAD: changed lines plus CONTEXT lines around them, the rest folded
//! into "N unmodified lines" rows that expand on click.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use ratatui::crossterm::event::{Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::Frame;
use similar::{Algorithm, ChangeTag, TextDiff};
use two_face::re_exports::syntect::parsing::SyntaxSet;

use super::git;
use crate::highlight::{highlight, shift, Styled};

const CONTEXT: usize = 3;
const ADD_BG: Color = Color::Rgb(24, 48, 34);
const DEL_BG: Color = Color::Rgb(62, 28, 32);
const FOLD_BG: Color = Color::Rgb(36, 38, 48);

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Ctx,
    Add,
    Del,
}

struct DLine {
    kind: Kind,
    /// Index into the new file's lines (Ctx/Add) or the old file's (Del).
    src: usize,
}

enum Row {
    Line(usize),
    Fold { start: usize, len: usize },
}


pub struct Diff {
    root: PathBuf,
    path: String,
    syntaxes: SyntaxSet,
    stamp: Option<(SystemTime, u64)>,
    /// A commit changes the diff without touching the file, so HEAD is watched too.
    head: String,
    head_checked: Instant,
    /// Changed files at open / last `]` `[`, for stepping between them.
    files: Vec<String>,
    binary: bool,
    lines: Vec<DLine>,
    old: Vec<Styled>,
    new: Vec<Styled>,
    ins: usize,
    del: usize,
    expanded: HashSet<usize>,
    expand_all: bool,
    rows: Vec<Row>,
    top: usize,
    left: usize,
    height: usize,
    /// Screen y -> fold start, from the last render.
    hits: Vec<(u16, usize)>,
}

impl Diff {
    pub fn new(root: PathBuf, path: String) -> Self {
        let mut d = Diff {
            root,
            path,
            syntaxes: two_face::syntax::extra_newlines(),
            stamp: None,
            head: String::new(),
            head_checked: Instant::now(),
            files: Vec::new(),
            binary: false,
            lines: Vec::new(),
            old: Vec::new(),
            new: Vec::new(),
            ins: 0,
            del: 0,
            expanded: HashSet::new(),
            expand_all: false,
            rows: Vec::new(),
            top: 0,
            left: 0,
            height: 0,
            hits: Vec::new(),
        };
        d.files = git::changes(&d.root).into_iter().map(|c| c.path).collect();
        d.open(d.path.clone());
        d
    }

    fn open(&mut self, path: String) {
        self.path = path;
        self.stamp = self.read_stamp();
        self.head = git::head(&self.root);
        (self.top, self.left, self.expand_all) = (0, 0, false);
        self.load();
    }

    /// `]` / `[`: the next or previous changed file, re-listed so new edits are included.
    fn step(&mut self, d: i8) {
        self.files = git::changes(&self.root).into_iter().map(|c| c.path).collect();
        // The list is sorted, so the neighbour by name also works when the open file has
        // dropped out of it (committed or reverted).
        let next = if d > 0 {
            self.files.iter().find(|p| **p > self.path)
        } else {
            self.files.iter().rev().find(|p| **p < self.path)
        };
        if let Some(p) = next.cloned() {
            self.open(p);
        }
    }

    fn read_stamp(&self) -> Option<(SystemTime, u64)> {
        let m = std::fs::metadata(self.root.join(&self.path)).ok()?;
        Some((m.modified().ok()?, m.len()))
    }

    /// Agents edit files while you read: reload when the working copy changes.
    pub fn tick(&mut self) {
        let stamp = self.read_stamp();
        // rev-parse is a process spawn; once a second is plenty for noticing a commit.
        let head = if self.head_checked.elapsed() >= Duration::from_secs(1) {
            self.head_checked = Instant::now();
            git::head(&self.root)
        } else {
            self.head.clone()
        };
        if stamp != self.stamp || head != self.head {
            if head != self.head {
                self.files = git::changes(&self.root).into_iter().map(|c| c.path).collect();
            }
            (self.stamp, self.head) = (stamp, head);
            self.load();
        }
    }

    fn load(&mut self) {
        let old = git::head_blob(&self.root, &self.path);
        let new = std::fs::read(self.root.join(&self.path)).unwrap_or_default();
        self.binary = git::is_binary(&old) || git::is_binary(&new);
        self.lines.clear();
        (self.ins, self.del) = (0, 0);
        if self.binary {
            self.rows.clear();
            return;
        }
        let (old, new) = (String::from_utf8_lossy(&old), String::from_utf8_lossy(&new));
        let diff = TextDiff::configure()
            .algorithm(Algorithm::Patience)
            .timeout(Duration::from_secs(2))
            .diff_lines(old.as_ref(), new.as_ref());
        for c in diff.iter_all_changes() {
            let line = match c.tag() {
                ChangeTag::Equal => DLine { kind: Kind::Ctx, src: c.new_index().unwrap_or(0) },
                ChangeTag::Insert => DLine { kind: Kind::Add, src: c.new_index().unwrap_or(0) },
                ChangeTag::Delete => DLine { kind: Kind::Del, src: c.old_index().unwrap_or(0) },
            };
            self.lines.push(line);
        }
        self.ins = self.lines.iter().filter(|l| l.kind == Kind::Add).count();
        self.del = self.lines.iter().filter(|l| l.kind == Kind::Del).count();
        self.old = highlight(&self.syntaxes, &self.path, &old);
        self.new = highlight(&self.syntaxes, &self.path, &new);
        // Fold starts shift when the diff changes, so stale expansions would open the wrong folds.
        self.expanded.clear();
        self.rebuild();
    }

    fn rebuild(&mut self) {
        let n = self.lines.len();
        let mut near = vec![self.expand_all; n];
        for (i, l) in self.lines.iter().enumerate() {
            if l.kind != Kind::Ctx {
                near[i.saturating_sub(CONTEXT)..(i + CONTEXT + 1).min(n)].fill(true);
            }
        }
        self.rows.clear();
        let mut i = 0;
        while i < n {
            if near[i] {
                self.rows.push(Row::Line(i));
                i += 1;
                continue;
            }
            let start = i;
            while i < n && !near[i] {
                i += 1;
            }
            // A one-line fold costs as much space as the line itself.
            if i - start < 2 || self.expanded.contains(&start) {
                self.rows.extend((start..i).map(Row::Line));
            } else {
                self.rows.push(Row::Fold { start, len: i - start });
            }
        }
    }

    fn is_change(&self, r: usize) -> bool {
        matches!(self.rows.get(r), Some(Row::Line(i)) if self.lines[*i].kind != Kind::Ctx)
    }

    /// Row indexes where a run of changed lines begins.
    fn hunks(&self) -> Vec<usize> {
        (0..self.rows.len()).filter(|&r| self.is_change(r) && (r == 0 || !self.is_change(r - 1))).collect()
    }

    fn scroll(&mut self, d: isize) {
        let max = self.rows.len().saturating_sub(self.height);
        self.top = (self.top as isize + d).clamp(0, max as isize) as usize;
    }

    fn jump(&mut self, forward: bool) {
        let cur = self.top + CONTEXT;
        let hunks = self.hunks();
        let to = if forward { hunks.into_iter().find(|&h| h > cur) } else { hunks.into_iter().rev().find(|&h| h < cur) };
        if let Some(h) = to {
            self.top = h.saturating_sub(CONTEXT);
            self.scroll(0);
        }
    }

    /// Returns false to quit (the overlay closes when the process exits).
    pub fn event(&mut self, e: Event) -> bool {
        let page = self.height.saturating_sub(2).max(1) as isize;
        match e {
            Event::Key(k) if k.kind == KeyEventKind::Press => match k.code {
                KeyCode::Char('q') | KeyCode::Esc => return false,
                KeyCode::Char('j') | KeyCode::Down => self.scroll(1),
                KeyCode::Char('k') | KeyCode::Up => self.scroll(-1),
                KeyCode::Char('d') | KeyCode::PageDown | KeyCode::Char(' ') => self.scroll(page),
                KeyCode::Char('u') | KeyCode::PageUp => self.scroll(-page),
                KeyCode::Char('g') | KeyCode::Home => self.top = 0,
                KeyCode::Char('G') | KeyCode::End => self.scroll(isize::MAX / 2),
                KeyCode::Char('h') | KeyCode::Left => self.left = self.left.saturating_sub(8),
                KeyCode::Char('l') | KeyCode::Right => self.left += 8,
                KeyCode::Char('n') => self.jump(true),
                KeyCode::Char('p') | KeyCode::Char('N') => self.jump(false),
                KeyCode::Char(']') => self.step(1),
                KeyCode::Char('[') => self.step(-1),
                KeyCode::Char('e') => {
                    self.expand_all = !self.expand_all;
                    self.rebuild();
                    self.scroll(0);
                }
                _ => {}
            },
            Event::Mouse(m) => match m.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(&(_, start)) = self.hits.iter().find(|(y, _)| *y == m.row) {
                        self.expanded.insert(start);
                        self.rebuild();
                    }
                }
                MouseEventKind::ScrollDown => self.scroll(3),
                MouseEventKind::ScrollUp => self.scroll(-3),
                MouseEventKind::ScrollRight => self.left += 8,
                MouseEventKind::ScrollLeft => self.left = self.left.saturating_sub(8),
                _ => {}
            },
            _ => {}
        }
        true
    }

    pub fn render(&mut self, f: &mut Frame) {
        let area = f.area();
        let buf = f.buffer_mut();
        let mut head: Vec<Span> = vec![" ".into(), self.path.clone().bold(), "  ".into()];
        if let Some(i) = self.files.iter().position(|p| *p == self.path) {
            head.push(format!("{}/{}  ", i + 1, self.files.len()).dark_gray());
        }
        if self.ins > 0 {
            head.push(format!("+{} ", self.ins).green());
        }
        if self.del > 0 {
            head.push(format!("-{} ", self.del).red());
        }
        let help = "[ ] file  n/p change  e expand  ←→ scroll  q close ";
        let used: usize = head.iter().map(|s| s.width()).sum();
        if used + help.len() < area.width as usize {
            head.push(" ".repeat(area.width as usize - used - help.len()).into());
            head.push(help.dark_gray());
        }
        buf.set_line(0, 0, &Line::from(head), area.width);

        let body = Rect::new(0, 1, area.width, area.height.saturating_sub(1));
        self.height = body.height as usize;
        self.hits.clear();
        if self.binary || self.lines.is_empty() {
            let msg = if self.binary { " binary file, not shown" } else { " no changes" };
            buf.set_line(0, 2, &Line::from(msg.dark_gray()), area.width);
            return;
        }

        let num_w = self.old.len().max(self.new.len()).max(1).to_string().len();
        self.scroll(0);
        for (y, r) in (body.y..body.bottom()).zip(self.top..self.rows.len()) {
            let rect = Rect::new(0, y, area.width, 1);
            match self.rows[r] {
                Row::Fold { start, len } => {
                    buf.set_style(rect, Style::new().bg(FOLD_BG));
                    let label = format!("{:>w$}  ↕  {len} unmodified lines", "", w = num_w + 1);
                    buf.set_line(0, y, &Line::from(label.gray()), area.width);
                    self.hits.push((y, start));
                }
                Row::Line(i) => {
                    let l = &self.lines[i];
                    let (bg, bar, num) = match l.kind {
                        Kind::Ctx => (None, " ".into(), Color::DarkGray),
                        Kind::Add => (Some(ADD_BG), "▎".green(), Color::Rgb(120, 180, 130)),
                        Kind::Del => (Some(DEL_BG), "▎".red(), Color::Rgb(200, 120, 120)),
                    };
                    if let Some(bg) = bg {
                        buf.set_style(rect, Style::new().bg(bg));
                    }
                    let src = if l.kind == Kind::Del { &self.old } else { &self.new };
                    let mut spans = vec![bar, Span::styled(format!("{:>num_w$}  ", l.src + 1), num)];
                    spans.extend(shift(src.get(l.src).map(Vec::as_slice).unwrap_or_default(), self.left));
                    buf.set_line(0, y, &Line::from(spans), area.width);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use two_face::re_exports::syntect::easy::HighlightLines;
    use two_face::re_exports::syntect::util::LinesWithEndings;
    use two_face::theme::EmbeddedThemeName;

    /// The pure-Rust regex engine (syntect-fancy) must still colour the languages this repo
    /// diffs most; a syntax it cannot compile would fall back to one plain colour.
    #[test]
    fn fancy_regex_still_highlights_common_languages() {
        let ss = two_face::syntax::extra_newlines();
        let themes = two_face::theme::extra();
        let theme = themes.get(EmbeddedThemeName::OneHalfDark);
        let samples = [
            ("tsx", "export const A = () => <div className=\"x\">{n + 1}</div>;\n"),
            ("ts", "const n: number = 1; // note\n"),
            ("py", "def f(x: int) -> str:\n    return f\"{x}\"\n"),
            ("rs", "fn main() { let s = \"hi\"; }\n"),
            ("sh", "if [ -f \"$x\" ]; then echo ok; fi\n"),
        ];
        for (ext, src) in samples {
            let syntax = ss.find_syntax_by_extension(ext).unwrap_or_else(|| panic!("no syntax for .{ext}"));
            let mut h = HighlightLines::new(syntax, theme);
            let mut colours = std::collections::HashSet::new();
            for line in LinesWithEndings::from(src) {
                for (style, _) in h.highlight_line(line, &ss).unwrap() {
                    colours.insert((style.foreground.r, style.foreground.g, style.foreground.b));
                }
            }
            assert!(colours.len() >= 3, ".{ext} got {} colours", colours.len());
        }
    }
}
