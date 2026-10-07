//! Live transcript of one subagent: prompt, replies, tool calls and their results.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::Frame;
use serde_json::Value;
use unicode_width::UnicodeWidthChar;

use super::data::{self, fmt_dur, fmt_tokens, Parents, Section, Status, Summary, Told};
use super::list::{badge, session_blocked};
use crate::tui::{self, truncate};

const RELOAD: Duration = Duration::from_millis(700);
/// Long enough to see the final status, short enough not to pile up finished panes.
const AUTO_CLOSE_AFTER: Duration = Duration::from_secs(3);
/// Agent panes look secondary next to the main Claude pane: a tinted header band and a gutter.
const BAND_BG: Color = Color::Rgb(38, 34, 62);
const GUTTER: Color = Color::Rgb(84, 74, 130);
const PROMPT_LINES: usize = 6;
const RESULT_LINES: usize = 3;
/// Expanded tool output is capped so a 5k-line Read does not drown the transcript.
const EXPANDED_MAX: usize = 400;

enum Item {
    Prompt(String),
    /// A background task or child agent reported back (`<task-notification>`).
    Note(String),
    Text(String),
    Thinking(String),
    Tool { name: String, summary: String, input: String, result: Option<(String, bool)> },
}

pub struct View {
    path: PathBuf,
    items: Vec<Item>,
    kind: String,
    desc: String,
    model: String,
    effort: String,
    /// Label last given to this split pane (None: not a split pane, or a snapshot).
    label: Option<String>,
    sum: Summary,
    mtime: i64,
    /// Foreground agent: its approval prompts show in the parent pane (see Summary::status).
    can_ask: bool,
    session: String,
    parent_blocked: bool,
    /// Split agent panes close themselves once their agent is done (overlays never do).
    auto_close: bool,
    /// Only a pane that watched the agent run closes itself: one opened on a finished agent
    /// was opened to read it.
    seen_live: bool,
    finished_at: Option<Instant>,
    /// Any key or click means the user is reading: then the pane stays.
    touched: bool,
    meta: Value,
    parents: Parents,
    told: Option<Told>,
    out_tokens: u64,
    size: u64,
    checked: Option<Instant>,
    /// Items the user clicked open; `all_open` expands every tool call.
    open: HashSet<usize>,
    all_open: bool,
    thinking: bool,
    lines: Vec<(Line<'static>, Option<usize>)>,
    built_for: Option<u16>,
    top: usize,
    follow: bool,
    height: usize,
}

impl View {
    pub fn new(path: PathBuf) -> Self {
        let meta_path = path.with_file_name(
            path.file_name().map(|n| n.to_string_lossy().replace(".jsonl", ".meta.json")).unwrap_or_default(),
        );
        let meta: Value = std::fs::read(meta_path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let mut v = View {
            path,
            items: Vec::new(),
            kind: meta["agentType"].as_str().unwrap_or("agent").into(),
            desc: meta["description"].as_str().unwrap_or("").into(),
            model: meta["model"].as_str().unwrap_or("").into(),
            effort: String::new(),
            label: None,
            sum: Summary::default(),
            mtime: 0,
            can_ask: meta["requestNonInteractive"] != true,
            session: String::new(),
            parent_blocked: false,
            auto_close: std::env::var("AGENT_PANE").as_deref() == Ok("split"),
            seen_live: false,
            finished_at: None,
            touched: false,
            meta: meta.clone(),
            parents: Parents::default(),
            told: None,
            out_tokens: 0,
            size: u64::MAX,
            checked: None,
            open: HashSet::new(),
            all_open: false,
            thinking: false,
            lines: Vec::new(),
            built_for: None,
            top: 0,
            follow: true,
            height: 0,
        };
        v.tick();
        v
    }

    /// Label for this view's split pane (overlays keep herdr's own label).
    pub fn pane_label(&self) -> String {
        let id = self.path.file_stem().map(|s| s.to_string_lossy().trim_start_matches("agent-").to_string()).unwrap_or_default();
        data::pane_label(&self.desc, &self.kind, &id, &data::model_tag(&self.model, &self.effort))
    }

    /// Starts keeping the split pane's label current (model and effort change as it runs).
    pub fn follow_label(&mut self) {
        self.label = Some(String::new());
        self.sync_label();
    }

    fn sync_label(&mut self) {
        let now = self.pane_label();
        if self.label.as_ref().is_some_and(|l| *l != now) {
            tui::herdr(&["pane", "rename", "$PANE", &now]);
            self.label = Some(now);
        }
    }

    pub fn tick(&mut self) {
        if self.checked.is_some_and(|t| t.elapsed() < RELOAD) {
            return;
        }
        self.checked = Some(Instant::now());
        let size = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        if size != self.size {
            self.size = size;
            self.load();
            self.built_for = None;
            self.sync_label();
        }
        self.told = self.parents.told(&self.path, &self.meta);
        // Only an agent with an open call can be the one waiting on the parent's approval prompt.
        self.parent_blocked = self.can_ask && self.status().is_live() && session_blocked(&self.session);
        let st = self.status();
        if st.is_live() {
            self.seen_live = true;
            self.finished_at = None;
        } else if self.seen_live && st.section() == Section::Finished {
            // NEEDS YOU (blocked, failed) is not finished: those panes stay for the user.
            self.finished_at.get_or_insert_with(Instant::now);
        }
    }

    /// Done and left alone for AUTO_CLOSE_AFTER: the pane closes.
    pub fn done(&self) -> bool {
        self.auto_close && !self.touched && self.finished_at.is_some_and(|t| t.elapsed() >= AUTO_CLOSE_AFTER)
    }

    fn status(&self) -> Status {
        self.sum.status(data::now_ms(), self.mtime, self.parent_blocked, self.can_ask, self.told.as_ref())
    }

    fn load(&mut self) {
        let records = data::read_records(&self.path);
        let mut items = Vec::new();
        let mut by_id: HashMap<String, usize> = HashMap::new();
        let mut tokens: HashMap<String, u64> = HashMap::new();
        let mut sum = Summary::default();
        for r in &records {
            sum.feed(r);
            if self.session.is_empty() {
                self.session = r["sessionId"].as_str().unwrap_or("").to_string();
            }
            let msg = &r["message"];
            match r["type"].as_str() {
                Some("user") => {
                    if let Some(s) = msg["content"].as_str() {
                        items.push(match notification(s) {
                            Some(n) => Item::Note(n),
                            None => Item::Prompt(clean(s)),
                        });
                    }
                    for b in msg["content"].as_array().into_iter().flatten() {
                        match b["type"].as_str() {
                            Some("tool_result") => {
                                let text = clean(&result_text(&b["content"]));
                                let err = b["is_error"] == true;
                                match b["tool_use_id"].as_str().and_then(|id| by_id.get(id)) {
                                    Some(&i) => {
                                        if let Item::Tool { result, .. } = &mut items[i] {
                                            *result = Some((text, err));
                                        }
                                    }
                                    None => items.push(Item::Text(text)),
                                }
                            }
                            Some("text") => items.push(Item::Prompt(clean(b["text"].as_str().unwrap_or("")))),
                            _ => {}
                        }
                    }
                }
                Some("assistant") => {
                    if let Some(m) = msg["model"].as_str().filter(|m| !m.starts_with('<')) {
                        self.model = m.to_string();
                    }
                    if let Some(e) = r["effort"].as_str() {
                        self.effort = e.to_string();
                    }
                    if let (Some(id), Some(n)) = (msg["id"].as_str(), msg["usage"]["output_tokens"].as_u64()) {
                        tokens.insert(id.to_string(), n);
                    }
                    for b in msg["content"].as_array().into_iter().flatten() {
                        match b["type"].as_str() {
                            Some("text") => items.push(Item::Text(clean(b["text"].as_str().unwrap_or("")))),
                            Some("thinking") => items.push(Item::Thinking(clean(b["thinking"].as_str().unwrap_or("")))),
                            Some("tool_use") => {
                                if let Some(id) = b["id"].as_str() {
                                    by_id.insert(id.to_string(), items.len());
                                }
                                items.push(Item::Tool {
                                    name: b["name"].as_str().unwrap_or("tool").to_string(),
                                    summary: data::tool_summary(&b["input"], &sum.cwd),
                                    input: clean(&pretty_input(&b["input"])),
                                    result: None,
                                });
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        self.items = items;
        self.sum = sum;
        self.out_tokens = tokens.values().sum();
        self.mtime = data::file_mtime(&self.path).unwrap_or(0);
    }

    /// Returns false to quit.
    pub fn event(&mut self, e: Event) -> bool {
        if matches!(&e, Event::Key(_)) || matches!(&e, Event::Mouse(m) if !matches!(m.kind, MouseEventKind::Moved)) {
            self.touched = true;
        }
        let page = self.height.saturating_sub(2).max(1);
        match e {
            Event::Key(k) if k.kind == KeyEventKind::Press => match k.code {
                KeyCode::Char('q') | KeyCode::Esc => return false,
                KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => return false,
                KeyCode::Char('j') | KeyCode::Down => self.scroll(1),
                KeyCode::Char('k') | KeyCode::Up => self.scroll(-1),
                KeyCode::Char('d') | KeyCode::PageDown | KeyCode::Char(' ') => self.scroll(page as isize),
                KeyCode::Char('u') | KeyCode::PageUp => self.scroll(-(page as isize)),
                KeyCode::Char('g') | KeyCode::Home => {
                    self.follow = false;
                    self.top = 0;
                }
                KeyCode::Char('G') | KeyCode::End => self.follow = true,
                KeyCode::Char('o') => {
                    self.all_open = !self.all_open;
                    self.open.clear();
                    self.built_for = None;
                }
                KeyCode::Char('t') => {
                    self.thinking = !self.thinking;
                    self.built_for = None;
                }
                _ => {}
            },
            Event::Mouse(m) => match m.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    let y = m.row as usize;
                    if y >= 2 {
                        if let Some(Some(i)) = self.lines.get(self.top + y - 2).map(|(_, i)| *i) {
                            if !self.open.remove(&i) {
                                self.open.insert(i);
                            }
                            self.built_for = None;
                        }
                    }
                }
                MouseEventKind::ScrollDown => self.scroll(3),
                MouseEventKind::ScrollUp => self.scroll(-3),
                _ => {}
            },
            _ => {}
        }
        true
    }

    fn scroll(&mut self, d: isize) {
        let max = self.lines.len().saturating_sub(self.height);
        let top = (self.top as isize + d).clamp(0, max as isize) as usize;
        self.top = top;
        self.follow = top >= max;
    }

    fn is_open(&self, i: usize) -> bool {
        self.open.contains(&i) || (self.all_open && matches!(self.items[i], Item::Tool { .. }))
    }

    fn build(&mut self, width: u16) {
        let w = (width as usize).saturating_sub(2).max(10);
        let mut out: Vec<(Line<'static>, Option<usize>)> = Vec::new();
        for (i, item) in self.items.iter().enumerate() {
            let open = self.is_open(i);
            match item {
                Item::Prompt(s) => {
                    out.push((Line::from(vec![" ".into(), "▍".cyan(), " Prompt".cyan().bold()]), Some(i)));
                    let lines = wrap(s, w.saturating_sub(3));
                    let n = if open { lines.len() } else { PROMPT_LINES.min(lines.len()) };
                    for l in &lines[..n] {
                        out.push((Line::from(vec!["   ".into(), l.clone().into()]), Some(i)));
                    }
                    if n < lines.len() {
                        out.push((Line::from(format!("   … +{} lines (click)", lines.len() - n).dark_gray()), Some(i)));
                    }
                }
                Item::Note(s) => {
                    out.push((Line::from(vec![" ".into(), "◐ ".magenta(), truncate(s, w.saturating_sub(3)).magenta()]), None));
                }
                Item::Text(s) => {
                    if s.trim().is_empty() {
                        continue;
                    }
                    for l in wrap(s, w.saturating_sub(1)) {
                        out.push((Line::from(vec![" ".into(), l.into()]), None));
                    }
                }
                Item::Thinking(s) => {
                    if !self.thinking {
                        continue;
                    }
                    out.push((Line::from(" ∴ thinking".dark_gray().italic()), None));
                    for l in wrap(s, w.saturating_sub(3)) {
                        out.push((Line::from(vec!["   ".into(), l.dark_gray().italic()]), None));
                    }
                }
                Item::Tool { name, summary, input, result } => {
                    let dot = match result {
                        None => "⏺".yellow(),
                        Some((_, true)) => "⏺".red(),
                        Some(_) => "⏺".green(),
                    };
                    let head = vec![" ".into(), dot, " ".into(), name.clone().bold(), "  ".into()];
                    let used: usize = head.iter().map(Span::width).sum();
                    let mut head = head;
                    head.push(truncate(summary, w.saturating_sub(used)).gray());
                    out.push((Line::from(head), Some(i)));
                    if open {
                        for l in wrap(input, w.saturating_sub(5)).into_iter().take(EXPANDED_MAX) {
                            out.push((Line::from(vec!["     ".into(), l.dark_gray()]), Some(i)));
                        }
                    }
                    let Some((text, err)) = result else {
                        out.push((Line::from("   ⎿ running…".dark_gray()), Some(i)));
                        continue;
                    };
                    let style = if *err { Style::new().fg(Color::Red) } else { Style::new().fg(Color::Gray) };
                    let lines = wrap(text, w.saturating_sub(5));
                    let n = if open { lines.len().min(EXPANDED_MAX) } else { RESULT_LINES.min(lines.len()) };
                    for (k, l) in lines[..n].iter().enumerate() {
                        let lead = if k == 0 { "   ⎿ " } else { "     " };
                        out.push((Line::from(vec![lead.dark_gray(), Span::styled(l.clone(), style)]), Some(i)));
                    }
                    if lines.is_empty() {
                        out.push((Line::from("   ⎿ (no output)".dark_gray()), Some(i)));
                    }
                    if n < lines.len() {
                        out.push((Line::from(format!("     … +{} lines (click)", lines.len() - n).dark_gray()), Some(i)));
                    }
                }
            }
            out.push((Line::default(), None));
        }
        self.lines = out;
        self.built_for = Some(width);
    }

    pub fn render(&mut self, f: &mut Frame) {
        let area = f.area();
        // One column goes to the gutter.
        let body_w = area.width.saturating_sub(1);
        if self.built_for != Some(body_w) {
            self.build(body_w);
        }
        let w = area.width as usize;
        let buf = f.buffer_mut();
        let now = data::now_ms();

        let status = self.status();
        let (icon, color) = badge(&status, now);
        let end = if status.is_live() { now } else { self.sum.last_ms };
        let took = fmt_dur((end - self.sum.start_ms).max(0));
        let mut right = Vec::new();
        let tag = data::model_tag(&self.model, &self.effort);
        if !tag.is_empty() {
            right.push(Span::styled(format!("{tag} "), Style::new().fg(Color::Cyan)));
            right.push("│ ".dark_gray());
        }
        right.push(Span::styled(format!("{icon} {}", status.label(now)), color));
        right.push(format!(" · {took} ").gray());
        let rw: usize = right.iter().map(Span::width).sum();
        let chip = Span::styled(" ◇ SUBAGENT ", Style::new().fg(Color::Rgb(20, 18, 34)).bg(GUTTER).bold());
        let tag = " read-only ".dark_gray();
        let used_left = chip.width() + tag.width() + 2;
        let title = if self.desc.is_empty() { self.kind.clone() } else { self.desc.clone() };
        let mut left = vec![chip, tag, "│ ".dark_gray(), truncate(&title, w.saturating_sub(rw + used_left + 2)).white().bold()];
        let used: usize = left.iter().map(Span::width).sum::<usize>() + rw;
        left.push(" ".repeat(w.saturating_sub(used)).into());
        left.extend(right);
        buf.set_style(ratatui::layout::Rect::new(0, 0, area.width, 1), Style::new().bg(BAND_BG));
        buf.set_line(0, 0, &Line::from(left), area.width);
        let id = self.path.file_stem().map(|s| s.to_string_lossy().trim_start_matches("agent-").to_string()).unwrap_or_default();
        let sub = format!("  {} · {} tools · {} out tokens · {id}", self.kind, self.sum.tools, fmt_tokens(self.out_tokens));
        buf.set_line(0, 1, &Line::from(truncate(&sub, w).dark_gray()), area.width);

        self.height = area.height.saturating_sub(3) as usize;
        let max = self.lines.len().saturating_sub(self.height);
        self.top = if self.follow { max } else { self.top.min(max) };
        for dy in 0..self.height as u16 {
            buf.set_string(0, 2 + dy, "▏", Style::new().fg(GUTTER));
        }
        for (dy, (line, _)) in self.lines.iter().skip(self.top).take(self.height).enumerate() {
            buf.set_line(1, 2 + dy as u16, line, body_w);
        }

        let pos = if self.lines.len() > self.height { format!("{}% ", (self.top + self.height) * 100 / self.lines.len()) } else { String::new() };
        let help = format!(
            " j/k scroll  click expand  o {}  t {}  G follow  q close",
            if self.all_open { "collapse" } else { "expand all" },
            if self.thinking { "hide thinking" } else { "thinking" }
        );
        let mut foot = vec![truncate(&help, w.saturating_sub(pos.len())).dark_gray()];
        let fw: usize = foot.iter().map(Span::width).sum();
        foot.push(" ".repeat(w.saturating_sub(fw + pos.len())).into());
        foot.push(pos.dark_gray());
        buf.set_line(0, area.height.saturating_sub(1), &Line::from(foot), area.width);
    }
}

/// `<summary>` of a task notification, with its status when not completed.
fn notification(s: &str) -> Option<String> {
    let tag = |t: &str| s.split(&format!("<{t}>")).nth(1)?.split(&format!("</{t}>")).next().map(str::trim);
    s.contains("<task-notification>").then(|| {
        let summary = tag("summary").unwrap_or("background task reported");
        match tag("status") {
            Some(st) if st != "completed" => format!("{summary} ({st})"),
            _ => summary.to_string(),
        }
    })
}

fn result_text(c: &Value) -> String {
    match c {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(|p| match p["type"].as_str() {
                Some("text") => p["text"].as_str().unwrap_or("").to_string(),
                Some(t) => format!("[{t}]"),
                None => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Bash/Agent inputs read best as their main field; anything else as `key: value` lines.
fn pretty_input(input: &Value) -> String {
    let Some(obj) = input.as_object() else { return input.to_string() };
    obj.iter()
        .map(|(k, v)| match v {
            Value::String(s) => format!("{k}: {s}"),
            _ => format!("{k}: {v}"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Tabs to spaces, and no escape sequences or other control characters from tool output.
fn clean(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\t' => out.push_str("    "),
            '\n' => out.push('\n'),
            '\x1b' => {
                // Skip a CSI sequence: ESC [ … final byte in @..~
                if chars.peek() == Some(&'[') {
                    chars.next();
                    for d in chars.by_ref() {
                        if ('@'..='~').contains(&d) {
                            break;
                        }
                    }
                }
            }
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Hard-wraps by display width (CJK and emoji count as two columns).
fn wrap(s: &str, width: usize) -> Vec<String> {
    let width = width.max(4);
    let mut out = Vec::new();
    for raw in s.trim_end().lines() {
        let mut line = String::new();
        let mut used = 0;
        for c in raw.chars() {
            let cw = c.width().unwrap_or(0);
            if used + cw > width {
                out.push(std::mem::take(&mut line));
                used = 0;
            }
            line.push(c);
            used += cw;
        }
        out.push(line);
    }
    out
}
