//! The agents panel: subagents of the Claude session in the focused pane, newest first.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::Frame;

use super::data::{self, fmt_dur, Agent, Scanner, Section, Status};
use super::grid;
use crate::tui::{spread, state_file, truncate, SIDE_LABELS};

const SEL_BG: Color = Color::Rgb(44, 47, 58);
/// Polled: the focused pane, its blocked state, the open agent panes, the `★ main` title.
const FOCUS_EVERY: Duration = Duration::from_secs(1);
/// Transcripts are rescanned on file events; this is the fallback for what no event announces.
const SAFETY_SCAN: Duration = Duration::from_secs(30);

const FLASH: Duration = Duration::from_secs(4);
const AUTO_GAP: Duration = Duration::from_millis(2500);
/// Only agents started this recently auto-open, so a panel (re)start does not reopen history.
const AUTO_WINDOW_MS: i64 = 10 * 60 * 1000;
/// An agent pane closes itself 3s after its agent finishes (view.rs); the grid waits this long
/// for that close instead of moving panes while one of them is closing.
const CLOSE_GRACE_MS: i64 = 6_000;
/// FINISHED shows this many newest units (agent + children, or a workflow run) until expanded.
const FEW_DONE: usize = 3;
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// How much of a section shows. Click its header to shut / reopen it.
#[derive(Clone, Copy, PartialEq)]
enum Fold {
    All,
    /// FINISHED only: the FEW_DONE newest units, then a `+N more` row.
    Few,
    Shut,
}

enum Row {
    Section { sec: Section, n: usize, fold: Fold },
    /// Under a FINISHED section: `+n more` (n hidden agents) or, when expanded, `show less` (n = 0).
    More { sec: Section, n: usize },
    Group { run: String, name: String },
    Agent { idx: usize, depth: usize },
}

struct Snap {
    session: Option<String>,
    name: String,
    /// When this session's Claude process started; earlier subagents belong to a previous run.
    since: i64,
    agents: Vec<Agent>,
    groups: HashMap<String, String>,
    /// Labels of agent panes open in this tab.
    open: HashSet<String>,
    /// Pane id of the Claude pane the agents came from; it takes a grid cell like any agent.
    main: String,
}

pub struct List {
    session: Option<String>,
    name: String,
    since: i64,
    /// Every subagent of the session; `agents` is what is shown.
    all: Vec<Agent>,
    show_prev: bool,
    agents: Vec<Agent>,
    groups: HashMap<String, String>,
    open: HashSet<String>,
    main: String,
    /// A short footer message (e.g. why a pane did not open), shown for FLASH.
    flash: Option<(String, std::time::Instant)>,
    /// Open a pane for each newly started agent (`o` toggles, remembered in the state dir).
    auto: bool,
    auto_done: HashSet<String>,
    /// Agent pane labels the grid was last laid out for; a different open set re-grids.
    gridded: HashSet<String>,
    opened_at: Option<std::time::Instant>,
    loaded: bool,
    folds: HashMap<Section, Fold>,
    rows: Vec<Row>,
    /// Selection is kept by agent id / run id so it survives refreshes that reorder rows.
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
    pub fn new() -> Self {
        let (rx, kick) = spawn();
        List {
            session: None,
            name: String::new(),
            since: 0,
            all: Vec::new(),
            show_prev: false,
            agents: Vec::new(),
            groups: HashMap::new(),
            open: HashSet::new(),
            main: String::new(),
            flash: None,
            auto: !state_file("auto-off").is_some_and(|p| p.exists()),
            auto_done: HashSet::new(),
            gridded: HashSet::new(),
            opened_at: None,
            loaded: false,
            folds: HashMap::new(),
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
        if snap.session != self.session {
            self.sel = None;
            self.top = 0;
        }
        self.session = snap.session;
        self.name = snap.name;
        self.since = snap.since;
        self.all = snap.agents;
        self.groups = snap.groups;
        self.open = snap.open;
        self.main = snap.main;
        self.rebuild();
        self.auto_open();
        let settled = self.opened_at.is_none_or(|t| t.elapsed() >= AUTO_GAP) && !self.closing_soon(data::now_ms());
        if settled && self.open != self.gridded && in_herdr() {
            self.regrid();
        }
    }

    /// An open pane whose agent just finished: it is about to close itself, and a pane closing
    /// mid-regrid hands its space to the wrong neighbour (the main pane shrank 107 → 90).
    fn closing_soon(&self, now: i64) -> bool {
        self.all.iter().any(|a| {
            a.status.section() == Section::Finished && now - a.last_ms < CLOSE_GRACE_MS && self.open.contains(&a.pane_key())
        })
    }

    /// Lays the main Claude pane and its agent panes out as one grid of equal cells that fits
    /// their count (grid.rs): main top-left, then agents oldest first.
    /// Remembers the panes it actually laid out: a pane that has not labelled itself yet (its
    /// process is still starting) is missed now and picked up by the next scan.
    fn regrid(&mut self) {
        let order: HashMap<String, i64> = self.all.iter().map(|a| (a.pane_key(), a.start_ms)).collect();
        self.gridded = regrid_panes(&self.main, &order);
    }

    /// Subagents from earlier runs of a resumed session, hidden unless `p` shows them.
    fn earlier(&self, a: &Agent) -> bool {
        a.start_ms < self.since && !a.status.is_live()
    }

    /// Units (a plain agent with its children, or a workflow run) go into the section of their
    /// most urgent member, newest first, so a run never splits across sections.
    fn rebuild(&mut self) {
        self.agents = self.all.iter().filter(|a| self.show_prev || !self.earlier(a)).cloned().collect();
        let a = &self.agents;
        let ids: HashMap<&str, usize> = a.iter().enumerate().map(|(i, x)| (x.id.as_str(), i)).collect();
        let mut kids: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut runs: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut tops: Vec<usize> = Vec::new();
        for (i, x) in a.iter().enumerate() {
            match (&x.parent, &x.group) {
                (Some(p), _) if ids.contains_key(p.as_str()) => kids.entry(p.as_str()).or_default().push(i),
                (_, Some(g)) => runs.entry(g.as_str()).or_default().push(i),
                _ => tops.push(i),
            }
        }

        fn push(rows: &mut Vec<Row>, i: usize, depth: usize, a: &[Agent], kids: &HashMap<&str, Vec<usize>>) {
            rows.push(Row::Agent { idx: i, depth });
            let mut ch = kids.get(a[i].id.as_str()).cloned().unwrap_or_default();
            ch.sort_by_key(|&c| a[c].start_ms);
            for c in ch {
                push(rows, c, depth + 1, a, kids);
            }
        }

        // (section, start, rows)
        let mut units: Vec<(Section, i64, Vec<Row>)> = Vec::new();
        for &i in &tops {
            let mut rows = Vec::new();
            push(&mut rows, i, 0, a, &kids);
            units.push((unit_section(&rows, a), a[i].start_ms, rows));
        }
        for (run, members) in &runs {
            let mut members = members.clone();
            members.sort_by_key(|&m| a[m].start_ms);
            let name = self.groups.get(*run).cloned().unwrap_or_else(|| run.to_string());
            let mut rows = vec![Row::Group { run: run.to_string(), name }];
            for &m in &members {
                push(&mut rows, m, 1, a, &kids);
            }
            units.push((unit_section(&rows, a), a[members[0]].start_ms, rows));
        }
        units.sort_by(|x, y| x.0.cmp(&y.0).then(y.1.cmp(&x.1)));

        self.rows.clear();
        // Units are sorted by section: take one section's run off the front at a time.
        while let Some(&(sec, _, _)) = units.first() {
            let total = units.iter().take_while(|u| u.0 == sec).count();
            let fold = self.fold(sec);
            let n = a.iter().filter(|x| x.status.section() == sec).count();
            self.rows.push(Row::Section { sec, n, fold });
            let shown = match fold {
                Fold::Shut => 0,
                Fold::Few => total.min(FEW_DONE),
                Fold::All => total,
            };
            let mut hidden = 0;
            for (k, (_, _, rows)) in units.drain(..total).enumerate() {
                if k < shown {
                    self.rows.extend(rows);
                } else {
                    hidden += rows.iter().filter(|r| matches!(r, Row::Agent { .. })).count();
                }
            }
            if fold == Fold::Few && hidden > 0 {
                self.rows.push(Row::More { sec, n: hidden });
            } else if fold == Fold::All && sec == Section::Finished && total > FEW_DONE {
                self.rows.push(Row::More { sec, n: 0 });
            }
        }
        if self.sel_idx().is_none() {
            self.sel = self.rows.iter().find(|r| !matches!(r, Row::Section { .. })).map(|r| self.key(r));
        }
    }

    fn fold(&self, sec: Section) -> Fold {
        let default = if sec == Section::Finished { Fold::Few } else { Fold::All };
        self.folds.get(&sec).copied().unwrap_or(default)
    }

    /// Header click: shut the section, or reopen it to its default.
    fn toggle_section(&mut self, sec: Section) {
        if self.fold(sec) == Fold::Shut {
            self.folds.remove(&sec);
        } else {
            self.folds.insert(sec, Fold::Shut);
        }
        self.rebuild();
    }

    fn key(&self, r: &Row) -> String {
        match r {
            Row::Section { sec, .. } => format!("§{}", *sec as u8),
            Row::More { sec, .. } => format!("+{}", *sec as u8),
            Row::Group { run, .. } => run.clone(),
            Row::Agent { idx, .. } => self.agents[*idx].id.clone(),
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
            Some(Row::Agent { idx, .. }) => self.open_agent(*idx, false),
            Some(&Row::Section { sec, .. }) => self.toggle_section(sec),
            Some(&Row::More { sec, n }) => {
                self.folds.insert(sec, if n > 0 { Fold::All } else { Fold::Few });
                self.rebuild();
            }
            _ => {}
        }
    }

    /// Opens agent `idx` in its own split pane, unless one is already open in this tab.
    /// `quiet` (auto-open): when every pane is busy, skip without the footer message.
    fn open_agent(&mut self, idx: usize, quiet: bool) {
        {
            let a = &self.agents[idx];
            let label = a.pane_key();
            if self.open.contains(&label) {
                return;
            }
            let room = agent_area(&self.main).is_none_or(|(cells, (w, h))| grid::choose(cells.len() + 1, w, h).is_some());
            if !room {
                // No room for one more readable pane: the agent that finished longest ago makes
                // room; one that needs the user (blocked, failed) only when every finished pane
                // needs the user.
                let victim = self
                    .agents
                    .iter()
                    .filter(|x| self.open.contains(&x.pane_key()) && !x.status.is_live())
                    .min_by_key(|x| (x.status.section() == Section::Attention, x.last_ms))
                    .map(Agent::pane_key);
                let Some(victim) = victim else {
                    if !quiet {
                        self.flash = Some(("no room for another readable pane: close one (c)".to_string(), std::time::Instant::now()));
                    }
                    return;
                };
                close_agent_panes(Some(&victim));
                self.open.remove(&victim);
            }
            open_pane(&self.agents[idx]);
            self.open.insert(label);
            self.opened_at = Some(std::time::Instant::now());
        }
    }

    /// Auto-open: every newly started agent of this run at once (each at most once, so a pane
    /// the user closed stays closed), then one re-grid for the new count. Waits AUTO_GAP after
    /// any open, because the open-pane set comes from the worker's scan and lags a fresh split.
    fn auto_open(&mut self) {
        if !self.auto || !in_herdr() || self.opened_at.is_some_and(|t| t.elapsed() < AUTO_GAP) {
            return;
        }
        let now = data::now_ms();
        let mut new: Vec<usize> = (0..self.agents.len())
            .filter(|&i| {
                let a = &self.agents[i];
                a.status.is_live() && a.start_ms >= self.since && now - a.start_ms < AUTO_WINDOW_MS && !self.auto_done.contains(&a.id)
            })
            .collect();
        new.sort_by_key(|&i| self.agents[i].start_ms);
        for &i in &new {
            self.auto_done.insert(self.agents[i].id.clone());
            self.open_agent(i, true);
        }
        // No re-grid here: the new panes label themselves once started; apply() re-grids
        // when the scan sees them.
    }

    fn peek(&self, i: usize) {
        if let Some(Row::Agent { idx, .. }) = self.rows.get(i) {
            open_view(&self.agents[*idx].path);
        }
    }

    /// Returns false to quit.
    pub fn event(&mut self, e: Event) -> bool {
        match e {
            Event::Key(k) if k.kind == KeyEventKind::Press => match k.code {
                KeyCode::Char('q') => {
                    crate::tui::close_here("agents");
                    return false;
                }
                KeyCode::Char('Q') => {
                    hide_everywhere();
                    return false;
                }
                KeyCode::Char('j') | KeyCode::Down => self.move_sel(1),
                KeyCode::Char('k') | KeyCode::Up => self.move_sel(-1),
                KeyCode::Char('g') | KeyCode::Home => self.select(0),
                KeyCode::Char('G') | KeyCode::End => self.select(self.rows.len().saturating_sub(1)),
                KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => {
                    if let Some(i) = self.sel_idx() {
                        self.activate(i);
                    }
                }
                KeyCode::Char('v') | KeyCode::Char(' ') => {
                    if let Some(i) = self.sel_idx() {
                        self.peek(i);
                    }
                }
                KeyCode::Char('o') => {
                    self.auto = !self.auto;
                    if let Some(p) = state_file("auto-off") {
                        let _ = if self.auto { std::fs::remove_file(p) } else { std::fs::write(p, "") };
                    }
                    self.flash = Some((format!("auto-open {}", if self.auto { "on" } else { "off" }), std::time::Instant::now()));
                }
                KeyCode::Char('c') => {
                    close_agent_panes(None);
                    self.open.clear();
                }
                KeyCode::Char('p') => {
                    self.show_prev = !self.show_prev;
                    self.rebuild();
                }
                KeyCode::Char('a') => self.toggle_section(Section::Finished),
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
                    self.top = (self.top + 3).min(self.rows.len().saturating_sub(1));
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
        let now = data::now_ms();
        self.hits.clear();

        let count = |sec| self.agents.iter().filter(|a| a.status.section() == sec).count();
        let mut head = vec![" ".into(), "◈ ".cyan(), format!("{} agent{}", self.agents.len(), plural(self.agents.len())).bold()];
        for (sec, icon) in [(Section::Attention, "⚠"), (Section::Active, "●"), (Section::Finished, "✓")] {
            let n = count(sec);
            if n > 0 {
                head.push("  ".into());
                head.push(Span::styled(format!("{icon} {n}"), section_color(sec)));
            }
        }
        let ctx = match &self.session {
            Some(s) if !self.name.is_empty() => format!("{} ", self.name),
            Some(s) => format!("{} ", &s[..8.min(s.len())]),
            None => String::new(),
        };
        let room = w.saturating_sub(head.iter().map(Span::width).sum::<usize>() + 2);
        let ctx = if room >= 6 { truncate(&ctx, room) } else { String::new() };
        buf.set_line(0, 0, &spread(head, vec![ctx.dark_gray()], w), area.width);

        let msg = match () {
            _ if !self.loaded => Some("loading…"),
            _ if self.session.is_none() => Some("no Claude session in this tab"),
            _ if self.all.is_empty() => Some("no subagents yet"),
            _ if self.agents.is_empty() => Some("none since Claude started (p: earlier)"),
            _ => None,
        };
        if let Some(msg) = msg {
            buf.set_line(1, 2, &Line::from(msg.dark_gray()), area.width);
        }

        // Agent rows are two lines tall; section and group headers one.
        let body = Rect::new(0, 2, area.width, area.height.saturating_sub(3));
        let tall = |r: &Row| if matches!(r, Row::Agent { .. }) { 2 } else { 1 };
        let sel = self.sel_idx();
        if self.follow {
            if let Some(s) = sel {
                if s < self.top {
                    self.top = s;
                }
                while self.rows[self.top..=s].iter().map(tall).sum::<u16>() > body.height && self.top < s {
                    self.top += 1;
                }
            }
        }
        self.top = self.top.min(self.rows.len().saturating_sub(1));
        self.height = body.height as usize;
        let mut y = body.y;
        for i in self.top..self.rows.len() {
            let r = &self.rows[i];
            if y + tall(r) > body.bottom() {
                break;
            }
            let lines = self.row_lines(r, w, now);
            for (dy, line) in lines.iter().enumerate() {
                let yy = y + dy as u16;
                if Some(i) == sel {
                    buf.set_style(Rect::new(0, yy, area.width, 1), Style::new().bg(SEL_BG));
                }
                buf.set_line(0, yy, line, area.width);
                self.hits.push((yy, i));
            }
            y += tall(r);
        }

        // Hints by priority; the ones that do not fit the panel width are dropped.
        let earlier = self.all.iter().filter(|a| self.earlier(a)).count();
        let mut hints = vec!["↵ pane".to_string(), "q hide".into()];
        if !self.open.is_empty() {
            hints.push("c close".into());
        }
        hints.push("v peek".into());
        hints.push(if self.auto { "o auto ✓" } else { "o auto" }.into());
        hints.push(if self.fold(Section::Finished) == Fold::Shut { "a show ✓" } else { "a hide ✓" }.into());
        hints.push("Q hide all".into());
        if earlier > 0 {
            hints.push(if self.show_prev { "p hide earlier".into() } else { format!("p +{earlier} earlier") });
        }
        let mut help = String::new();
        for h in hints {
            if help.chars().count() + h.chars().count() + 3 <= w {
                help.push_str("  ");
                help.push_str(&h);
            }
        }
        let help = help.replacen("  ", " ", 1);
        let foot = match &self.flash {
            Some((msg, at)) if at.elapsed() < FLASH => Line::from(format!(" {msg}").yellow()),
            _ => Line::from(help.dark_gray()),
        };
        buf.set_line(0, area.height.saturating_sub(1), &foot, area.width);
    }

    fn row_lines(&self, r: &Row, w: usize, now: i64) -> Vec<Line<'static>> {
        match r {
            Row::Section { sec, n, fold } => {
                let title = match sec {
                    Section::Attention => "NEEDS YOU",
                    Section::Active => "RUNNING",
                    Section::Finished => "FINISHED",
                };
                let chev = if *fold == Fold::Shut { "▸" } else { "▾" };
                let head = format!(" {chev} {title} {n} ");
                let rule = "─".repeat(w.saturating_sub(head.chars().count() + 1));
                let color = section_color(*sec);
                vec![Line::from(vec![Span::styled(head, Style::new().fg(color).bold()), Span::styled(rule, Style::new().fg(Color::DarkGray))])]
            }
            Row::More { n, .. } => {
                let text = if *n > 0 { format!("+{n} more") } else { "show less".into() };
                vec![Line::from(vec!["   ".into(), text.dark_gray().italic()])]
            }
            Row::Group { name, .. } => {
                vec![Line::from(vec![" ".into(), "⧉ ".magenta(), truncate(name, w.saturating_sub(4)).magenta().bold()])]
            }
            Row::Agent { idx, depth } => {
                let a = &self.agents[*idx];
                let indent = " ".repeat(1 + depth * 2);
                let (icon, color) = badge(&a.status, now);
                let mut right = vec![Span::from(fmt_dur(a.elapsed_ms(now))).dark_gray(), " ".into()];
                if self.open.contains(&a.pane_key()) {
                    right.insert(0, "▣ ".cyan());
                }
                let title = if a.desc.is_empty() { a.kind.clone() } else { a.desc.clone() };
                let used = indent.len() + 2 + right.iter().map(Span::width).sum::<usize>() + 1;
                let left = vec![indent.clone().into(), Span::styled(icon, color), " ".into(), truncate(&title, w.saturating_sub(used)).into()];
                let line1 = spread(left, right, w);

                // Line 2: the status in its colour, then what it is doing (live) or its stats.
                let label = a.status.label(now);
                let mut rest = if a.status.is_live() && !a.activity.is_empty() {
                    a.activity.clone()
                } else {
                    format!("{} · {} tool{}", a.kind, a.tools, plural(a.tools))
                };
                if a.errors > 0 && !a.status.is_live() {
                    rest.push_str(&format!(" · {} err", a.errors));
                }
                let lead = format!("{indent}  ");
                let tag = a.model_tag();
                let tag_w = if tag.is_empty() { 0 } else { tag.chars().count() + 3 };
                let room = w.saturating_sub(lead.len() + label.chars().count() + 4 + tag_w);
                let mut line2 = vec![lead.into(), Span::styled(label, color), " · ".dark_gray()];
                if !tag.is_empty() {
                    line2.push(Span::styled(tag, Style::new().fg(Color::Cyan)));
                    line2.push(" · ".dark_gray());
                }
                line2.push(truncate(&rest, room).dark_gray());
                let line2 = Line::from(line2);
                vec![line1, line2]
            }
        }
    }
}

/// Icon and colour for a status; the transcript header uses the same pair.
pub fn badge(s: &Status, now: i64) -> (String, Color) {
    let spin = SPINNER[(now / 100) as usize % SPINNER.len()].to_string();
    match s {
        Status::Approval { .. } => ("⚠".into(), Color::LightRed),
        Status::Reported(r) if r == "NEEDS_CONTEXT" => ("?".into(), Color::LightRed),
        Status::Reported(_) | Status::Failed(_) => ("✗".into(), Color::LightRed),
        Status::Starting => ("○".into(), Color::LightBlue),
        Status::Thinking => (spin, Color::Yellow),
        Status::Tool { .. } => (spin, Color::Cyan),
        Status::Waiting { .. } => ("◐".into(), Color::Magenta),
        Status::Done => ("✓".into(), Color::Green),
        Status::Concerns => ("✓".into(), Color::Yellow),
        Status::Interrupted => ("■".into(), Color::DarkGray),
        Status::Stopped => ("◌".into(), Color::DarkGray),
        Status::Retried => ("↻".into(), Color::DarkGray),
    }
}

fn section_color(s: Section) -> Color {
    match s {
        Section::Attention => Color::LightRed,
        Section::Active => Color::Yellow,
        Section::Finished => Color::Green,
    }
}

fn unit_section(rows: &[Row], a: &[Agent]) -> Section {
    rows.iter()
        .filter_map(|r| match r {
            Row::Agent { idx, .. } => Some(a[*idx].status.section()),
            _ => None,
        })
        .min()
        .unwrap_or(Section::Finished)
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}


/// This tab's panes and this pane's id, from a pane list. None outside herdr.
fn tab_panes_in(panes: &[serde_json::Value]) -> Option<(Vec<serde_json::Value>, String)> {
    let me = std::env::var("HERDR_PANE_ID").ok()?;
    let tab = crate::tui::tab_of(panes, &me)?;
    Some((panes.iter().filter(|p| &p["tab_id"] == tab).cloned().collect(), me))
}

fn tab_panes() -> Option<(Vec<serde_json::Value>, String)> {
    tab_panes_in(&crate::tui::pane_list()?)
}

fn is_agent_pane(p: &serde_json::Value) -> bool {
    p["label"].as_str().is_some_and(|l| l.starts_with(data::PANE_PREFIX))
}

fn open_agent_panes(all: &[serde_json::Value]) -> HashSet<String> {
    let Some((panes, _)) = tab_panes_in(all) else { return HashSet::new() };
    panes.iter().filter(|p| is_agent_pane(p)).filter_map(|p| p["label"].as_str().map(|l| data::pane_key(l).to_string())).collect()
}

/// The first agent pane splits the largest work pane (the main Claude pane) to the right; later
/// ones split the largest agent pane along its long side. That is only a first placement: the
/// re-grid then gives main and every agent an equal cell. Terminal cells are about twice as
/// tall as wide, hence 2.2.
fn open_pane(a: &Agent) {
    let Some((panes, me)) = tab_panes() else { return };
    let Ok(bin) = std::env::var("HERDR_BIN_PATH") else { return };
    let Ok(out) = Command::new(&bin).args(["pane", "layout", "--pane", &me]).stderr(Stdio::null()).output() else { return };
    let layout: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    let any_agent = panes.iter().any(is_agent_pane);
    let work: HashSet<&str> = panes
        .iter()
        .filter(|p| p["pane_id"] != me.as_str())
        .filter(|p| if any_agent { is_agent_pane(p) } else { !p["label"].as_str().is_some_and(|l| SIDE_LABELS.contains(&l)) })
        .filter_map(|p| p["pane_id"].as_str())
        .collect();
    let rect = |p: &serde_json::Value| (p["rect"]["width"].as_u64().unwrap_or(0), p["rect"]["height"].as_u64().unwrap_or(0));
    let Some(target) = layout["result"]["layout"]["panes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["pane_id"].as_str().is_some_and(|id| work.contains(id)))
        .max_by_key(|p| rect(p).0 * rect(p).1 * 2)
    else {
        return;
    };
    let (w, h) = rect(target);
    let dir = if !any_agent || w as f64 > 2.2 * h as f64 { "right" } else { "down" };
    let plugin = crate::tui::plugin_id();
    let mut cmd = Command::new(bin);
    cmd.args(["plugin", "pane", "open", "--plugin", &plugin, "--entrypoint", "view", "--placement", "split"])
        .args(["--target-pane", target["pane_id"].as_str().unwrap_or(""), "--direction", dir])
        .args(["--env", &format!("AGENT_FILE={}", a.path.display()), "--env", "AGENT_PANE=split", "--no-focus"])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if !a.cwd.is_empty() {
        cmd.arg("--cwd").arg(&a.cwd);
    }
    // Waited for, so the next open (auto-open) sees this split in the layout.
    let _ = cmd.status();
}

/// A grid cell: (pane id, label, [x, y, w, h]). The main pane's label is empty.
type AgentPane = (String, String, [u16; 4]);

/// The grid's cells and the size of the area they cover: the main pane first, then this tab's
/// agent panes. Main joins only while its cells and the agents' tile one rectangle (nothing
/// else, such as a side panel, sits among them); else the agents are gridded on their own.
/// None when no agent pane is open.
fn agent_area(main: &str) -> Option<(Vec<AgentPane>, (u16, u16))> {
    let (panes, me) = tab_panes()?;
    let labels: HashMap<&str, &str> = panes
        .iter()
        .filter(|p| is_agent_pane(p))
        .filter_map(|p| Some((p["pane_id"].as_str()?, data::pane_key(p["label"].as_str()?))))
        .collect();
    let layout = herdr_json(&["pane", "layout", "--pane", &me])?;
    let n = |v: &serde_json::Value| v.as_u64().unwrap_or(0) as u16;
    let mut main_cell = None;
    let mut found: Vec<AgentPane> = Vec::new();
    for p in layout["result"]["layout"]["panes"].as_array()? {
        let Some(id) = p["pane_id"].as_str() else { continue };
        let r = &p["rect"];
        let rect = [n(&r["x"]), n(&r["y"]), n(&r["width"]), n(&r["height"])];
        if let Some(l) = labels.get(id) {
            found.push((id.to_string(), l.to_string(), rect));
        } else if !main.is_empty() && id == main {
            main_cell = Some((id.to_string(), String::new(), rect));
        }
    }
    if found.is_empty() {
        return None;
    }
    if let Some(m) = main_cell {
        let mut with_main = vec![m];
        with_main.extend(found.iter().cloned());
        if let Some(size) = grid::tiled(&with_main.iter().map(|c| c.2).collect::<Vec<_>>()) {
            return Some((with_main, size));
        }
    }
    let size = grid::bounds(&found.iter().map(|c| c.2).collect::<Vec<_>>());
    Some((found, size))
}

fn herdr_json(args: &[&str]) -> Option<serde_json::Value> {
    let bin = std::env::var("HERDR_BIN_PATH").ok()?;
    let out = Command::new(bin).args(args).stderr(Stdio::null()).output().ok()?;
    serde_json::from_slice(&out.stdout).ok()
}

/// Re-tiles the main pane and this tab's agent panes as grid::choose's grid of equal cells,
/// row-major: main first, then agents by `order` (agent start). herdr has no "set layout": the
/// anchor (main, or the oldest agent when main is not in the grid) stays, the others park in a
/// hidden tab so the anchor fills the whole area, then each moves back split off its neighbour
/// at the ratio that makes equal parts. `pane move` keeps the transcript process; nothing
/// restarts. Skipped when the panes already sit in that grid, so a scan with the same panes
/// costs one layout read. Returns the agent pane labels it saw.
fn regrid_panes(main: &str, order: &HashMap<String, i64>) -> HashSet<String> {
    let Some((mut panes, (w, h))) = agent_area(main) else { return HashSet::new() };
    let seen: HashSet<String> = panes.iter().filter(|p| !p.1.is_empty()).map(|p| p.1.clone()).collect();
    let (n, Some(tab)) = (panes.len(), tab_panes().and_then(|(p, me)| crate::tui::tab_of(&p, &me).and_then(|t| t.as_str().map(String::from)))) else { return seen };
    if n < 2 {
        return seen;
    }
    // The main cell's empty label sorts it first.
    panes.sort_by_key(|p| (!p.1.is_empty(), order.get(&p.1).copied().unwrap_or(i64::MAX), p.1.clone()));
    let Some((cols, _)) = grid::choose(n, w, h) else { return seen };
    let plan = grid::columns(n, cols);
    let (x0, y0) = (panes.iter().map(|p| p.2[0]).min().unwrap_or(0), panes.iter().map(|p| p.2[1]).min().unwrap_or(0));
    let in_place = plan.iter().enumerate().all(|(c, col)| {
        col.iter().enumerate().all(|(r, &i)| {
            let want = [x0 + (w as usize * c / cols) as u16, y0 + (h as usize * r / col.len()) as u16];
            let at = panes[i].2;
            at[0].abs_diff(want[0]) <= 2 && at[1].abs_diff(want[1]) <= 2 && at[2].abs_diff((w as usize / cols) as u16) <= 2
        })
    });
    if in_place {
        return seen;
    }
    let id = |i: usize| panes[i].0.as_str();
    let mv = |args: &[&str]| herdr_json(&[&["pane", "move"], args, &["--no-focus"]].concat());
    // Park every pane but the anchor; the parking tab closes by itself when its last pane leaves.
    let mut park: Option<String> = None;
    for i in 1..n {
        let out = match &park {
            None => mv(&[id(i), "--new-tab", "--label", "sidekick-regrid"]),
            Some(t) => mv(&[id(i), "--tab", t, "--split", "down"]),
        };
        if park.is_none() {
            park = out.and_then(|v| v["result"]["move_result"]["pane"]["tab_id"].as_str().map(String::from));
            if park.is_none() {
                return seen; // nothing moved: leave the layout as it was
            }
        }
    }
    let heads: Vec<usize> = plan.iter().map(|col| col[0]).collect();
    for c in 1..cols {
        let ratio = format!("{:.4}", grid::keep_ratio(cols, c));
        mv(&[id(heads[c]), "--tab", &tab, "--target-pane", id(heads[c - 1]), "--split", "right", "--ratio", &ratio]);
    }
    for col in &plan {
        for r in 1..col.len() {
            let ratio = format!("{:.4}", grid::keep_ratio(col.len(), r));
            mv(&[id(col[r]), "--tab", &tab, "--target-pane", id(col[r - 1]), "--split", "down", "--ratio", &ratio]);
        }
    }
    // A moved pane can keep a stale PTY size (herdr 0.9.1); a zero resize resyncs it.
    for p in &panes {
        let _ = herdr_json(&["pane", "resize", "--pane", &p.0, "--direction", "down", "--amount", "0"]);
    }
    seen
}

/// While agent panes share the tab, title the Claude pane they came from `★ main`. The title
/// expires on its own (TTL), so it is refreshed every scan and vanishes when the panes close or
/// this panel exits; nothing has to clear it.
fn mark_main(pane: &str, name: &str) {
    let Ok(bin) = std::env::var("HERDR_BIN_PATH") else { return };
    if pane.is_empty() {
        return;
    }
    let title = if name.is_empty() { "★ main".to_string() } else { format!("★ main · {name}") };
    let _ = Command::new(bin)
        .args(["pane", "report-metadata", pane, "--source", crate::PLUGIN_ID, "--title", &title, "--ttl-ms", "3000"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Closes this tab's agent panes: all of them (`c`), or the one with `only` as its key.
fn close_agent_panes(only: Option<&str>) {
    let (Some((panes, _)), Ok(bin)) = (tab_panes(), std::env::var("HERDR_BIN_PATH")) else { return };
    let chosen = panes.iter().filter(|p| is_agent_pane(p) && only.is_none_or(|k| p["label"].as_str().is_some_and(|l| data::pane_key(l) == k)));
    for id in chosen.filter_map(|p| p["pane_id"].as_str()) {
        crate::tui::close_pane(&bin, id);
    }
}

fn open_view(path: &std::path::Path) {
    crate::tui::open_overlay("view", None, Some(format!("AGENT_FILE={}", path.display())));
}

/// One worker per panel. Every FOCUS_EVERY it follows the focused Claude pane (one `pane list`),
/// refreshes the `★ main` title and sends a snapshot; elapsed times and spinners are computed at
/// render, so they need no scan. Transcripts are rescanned only when they change (watch.rs), the
/// followed session or its pane's `blocked` state changes, `r` is pressed, or SAFETY_SCAN passes
/// (agents go quiet → stopped after minutes of silence, which no file event announces).
fn spawn() -> (Receiver<Snap>, Sender<()>) {
    let (tx, rx) = mpsc::channel();
    let (kick, kicks) = mpsc::channel::<()>();
    let mut watch = super::watch::Watch::new(kick.clone());
    std::thread::spawn(move || {
        let every = if watch.is_some() { SAFETY_SCAN } else { FOCUS_EVERY };
        let mut scanner = Scanner::default();
        let mut sessions = data::Sessions::default();
        let mut notifier = super::notify::Notifier::default();
        let mut main_pane = String::new();
        let mut session: Option<String> = std::env::var("AGENTS_SESSION").ok();
        let mut followed: Option<String> = None;
        let mut dir: Option<PathBuf> = None;
        // Outside herdr (snapshots), AGENTS_BLOCKED=1 stands in for a blocked parent pane.
        let mut blocked = std::env::var("AGENTS_BLOCKED").is_ok();
        let mut scanned_blocked = blocked;
        let (mut agents, mut groups) = (Vec::new(), HashMap::new());
        let (mut name, mut since) = (String::new(), 0);
        let mut last: Option<Instant> = None;
        let mut dirty = true;
        loop {
            // One `pane list` per tick serves the session to follow, its blocked state and the
            // open agent panes.
            let panes = crate::tui::pane_list().unwrap_or_default();
            if std::env::var("AGENTS_SESSION").is_err() {
                match focused_session(&panes, session.as_deref()) {
                    Some((s, b, p)) => (session, blocked, main_pane) = (Some(s), b, p),
                    None if !in_herdr() => session = None,
                    None => {}
                }
            }
            if followed != session {
                followed = session.clone();
                dir = None;
                scanner = Scanner::default();
                (agents, groups) = (Vec::new(), HashMap::new());
                if let Some(w) = watch.as_mut() {
                    w.follow(None);
                }
                dirty = true;
            }
            if blocked != scanned_blocked {
                dirty = true;
            }
            // Entering a worktree moves the session to another project directory: drop the old
            // path so the lookup below finds the new one.
            if dir.as_deref().is_some_and(data::dir_moved) {
                dir = None;
                scanner = Scanner::default();
                (agents, groups) = (Vec::new(), HashMap::new());
                if let Some(w) = watch.as_mut() {
                    w.follow(None);
                }
                dirty = true;
            }
            // A new session has no directory until its first subagent; keep looking until then.
            if dir.is_none() {
                dir = session.as_deref().and_then(data::session_dir);
                if dir.is_some() {
                    if let Some(w) = watch.as_mut() {
                        w.follow(dir.as_deref());
                    }
                    dirty = true;
                }
            }
            if dirty || last.is_none_or(|t| t.elapsed() >= every) {
                dirty = false;
                last = Some(Instant::now());
                scanned_blocked = blocked;
                // A restart with --resume keeps the id but moves the start.
                (name, since) = session.as_deref().and_then(|s| sessions.info(s)).unwrap_or_default();
                if let Some(d) = &dir {
                    agents = scanner.scan(d, blocked, 0);
                    if in_herdr() {
                        let label = if name.is_empty() { session.as_deref().unwrap_or("").chars().take(8).collect() } else { name.clone() };
                        super::notify::check(&agents, &label, since);
                    }
                    groups = agents
                        .iter()
                        .filter_map(|a| a.group.clone())
                        .map(|g| {
                            let n = scanner.workflow_name(d, &g);
                            (g, n)
                        })
                        .collect();
                }
            }
            let open = open_agent_panes(&panes);
            if !open.is_empty() {
                mark_main(&main_pane, &name);
            }
            let snap = Snap { session: session.clone(), name: name.clone(), since, agents: agents.clone(), groups: groups.clone(), open, main: main_pane.clone() };
            if tx.send(snap).is_err() {
                return;
            }
            if in_herdr() {
                notifier.tick();
            }
            match crate::tui::wait_kick(&kicks, FOCUS_EVERY, last) {
                Some(kicked) => dirty |= kicked,
                None => return,
            }
        }
    });
    (rx, kick)
}

fn in_herdr() -> bool {
    std::env::var("HERDR_PANE_ID").is_ok()
}

/// Claude session id to follow in this panel's tab: the focused pane's if it runs Claude, else
/// the one already followed if still there, else the tab's first Claude pane. None when the tab
/// has no Claude (or herdr can't be asked), so the caller keeps its last session.
/// The flag is herdr's `blocked` state for that pane: an approval or question prompt is up.
fn focused_session(panes: &[serde_json::Value], prev: Option<&str>) -> Option<(String, bool, String)> {
    let me = std::env::var("HERDR_PANE_ID").ok()?;
    let tab = crate::tui::tab_of(panes, &me)?;
    let claude: Vec<(&serde_json::Value, &str)> = panes
        .iter()
        .filter(|p| &p["tab_id"] == tab && p["pane_id"] != me.as_str())
        .filter(|p| !p["label"].as_str().is_some_and(|l| SIDE_LABELS.contains(&l)))
        .filter(|p| p["agent_session"]["agent"] == "claude")
        .filter_map(|p| Some((p, p["agent_session"]["value"].as_str()?)))
        .collect();
    let pick = claude
        .iter()
        .find(|(p, _)| p["focused"] == true)
        .or_else(|| claude.iter().find(|(_, s)| Some(*s) == prev))
        .or(claude.first());
    pick.map(|(p, s)| (s.to_string(), p["agent_status"] == "blocked", p["pane_id"].as_str().unwrap_or("").to_string()))
}

/// herdr shows the pane running this Claude session at an approval or question prompt.
pub fn session_blocked(session: &str) -> bool {
    let panes = crate::tui::pane_list().unwrap_or_default();
    panes.iter().any(|p| p["agent_session"]["value"] == session && p["agent_status"] == "blocked")
}

fn hide_everywhere() {
    crate::tui::hide_everywhere(super::LABEL, "agents");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(i: i64, status: Status) -> Agent {
        Agent {
            id: format!("a{i}"),
            path: PathBuf::new(),
            kind: "reviewer".into(),
            desc: format!("task {i}"),
            parent: None,
            group: None,
            start_ms: i,
            last_ms: i,
            status,
            tools: 0,
            errors: 0,
            activity: String::new(),
            cwd: String::new(),
            model: String::new(),
            effort: String::new(),
        }
    }

    /// What the list shows, one token per row: S<section> header, A agent, +n more.
    fn shape(l: &List) -> Vec<String> {
        l.rows
            .iter()
            .map(|r| match r {
                Row::Section { sec, .. } => format!("S{}", *sec as u8),
                Row::More { n, .. } => format!("+{n}"),
                Row::Group { .. } => "G".into(),
                Row::Agent { .. } => "A".into(),
            })
            .collect()
    }

    fn at(l: &List, token: &str) -> usize {
        shape(l).iter().position(|t| t == token).expect(token)
    }

    #[test]
    fn finished_shows_the_newest_few_and_folds_open_and_shut() {
        let mut l = List::new();
        l.all = (0..6).map(|i| agent(i, Status::Done)).chain([agent(9, Status::Thinking)]).collect();
        l.rebuild();
        assert_eq!(shape(&l), ["S1", "A", "S2", "A", "A", "A", "+3"], "running in full, finished capped at 3");

        l.activate(at(&l, "+3"));
        assert_eq!(shape(&l).iter().filter(|t| *t == "A").count(), 7, "+N more shows every finished agent");
        assert_eq!(shape(&l).last().map(String::as_str), Some("+0"), "then offers show less");

        l.activate(at(&l, "S2"));
        assert_eq!(shape(&l), ["S1", "A", "S2"], "header click shuts the section but keeps the header");

        l.activate(at(&l, "S2"));
        assert_eq!(shape(&l), ["S1", "A", "S2", "A", "A", "A", "+3"], "reopening returns to the capped view");

        l.activate(at(&l, "S1"));
        assert_eq!(shape(&l), ["S1", "S2", "A", "A", "A", "+3"], "any section folds, not only finished");
    }

    #[test]
    fn regrid_waits_while_a_just_finished_agents_pane_is_about_to_close() {
        let mut l = List::new();
        let now = 100_000;
        let mut done = agent(1, Status::Done);
        done.last_ms = now - 1_000;
        l.all = vec![done, agent(2, Status::Thinking)];
        l.open = l.all.iter().map(Agent::pane_key).collect();
        assert!(l.closing_soon(now), "its pane closes itself in a moment: re-gridding now races that close");
        assert!(!l.closing_soon(now + CLOSE_GRACE_MS), "a pane still open after the grace stays: grid it");
        l.open.remove(&l.all[0].pane_key());
        assert!(!l.closing_soon(now), "a finished agent without a pane does not hold the grid");
    }
}
