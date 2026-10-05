//! Board state: joins the three data sources into ranked rows, tracks selection by path,
//! animations, and runs user actions off the UI thread.

use std::collections::{HashMap, HashSet};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ratatui::layout::Rect;

use super::data::{self, Agent, GitInfo, HerdrSnap, Msg, PrInfo};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Place {
    Here,
    Open,
    Closed,
    Prunable,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub path: String,
    pub branch: String,
    pub place: Place,
    pub linked: bool,
    pub open_ws: Option<String>,
    /// Most urgent first.
    pub agents: Vec<Agent>,
    pub git: Option<GitInfo>,
    pub pr: Option<PrInfo>,
}

impl Row {
    pub fn top_status(&self) -> &str {
        self.agents.first().map(|a| a.status.as_str()).unwrap_or("")
    }
    pub fn ci_failed(&self) -> bool {
        self.pr.as_ref().is_some_and(|p| p.ci == "FAILURE" || p.ci == "ERROR")
    }
    /// Attention order: needs you > CI broken > finished > busy > quiet.
    fn rank(&self) -> u8 {
        match self.top_status() {
            "blocked" => 0,
            _ if self.ci_failed() => 1,
            "done" => 2,
            "working" => 3,
            _ => 4,
        }
    }
}

fn urgency(status: &str) -> u8 {
    match status {
        "blocked" => 0,
        "working" => 1,
        "done" => 2,
        "idle" => 3,
        _ => 4,
    }
}

pub enum Mode {
    Normal,
    Filter,
    NewBranch(String),
    ConfirmDelete(String),
}

/// A button on the selected card's action row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Act {
    Open,
    Claude,
    Pr,
    Hide,
    Delete,
}

/// Vertical position of a card, eased from its old slot to its new one when rows re-sort.
struct Slide {
    from: f32,
    to: f32,
    start: Instant,
}

/// Index of the git worker's kick channel in `App::kicks` (herdr, git, pr order in main).
pub const GIT_KICK: usize = 1;

pub const SLIDE: Duration = Duration::from_millis(260);
pub const PULSE: Duration = Duration::from_millis(3000);

pub struct App {
    pub ws: String,
    pub here: String,
    pub repo: String,
    pub root: String,
    pub base: String,
    pub plugin_root: String,
    pub snap: HerdrSnap,
    pub git: HashMap<String, GitInfo>,
    pub prs: Vec<PrInfo>,
    pub pr_at: i64,
    pub rows: Vec<Row>,
    pub selected: Option<String>,
    pub hover: Option<String>,
    pub expanded: bool,
    /// Worktree paths taken off the board with `x` (kept in the state dir); `H` shows them.
    pub hidden: HashSet<String>,
    pub show_hidden: bool,
    /// `?`: the icon legend replaces the list.
    pub legend: bool,
    pub filter: String,
    pub mode: Mode,
    pub status: Option<(String, bool, Instant)>,
    pub started: Instant,
    pub scroll: u16,
    /// Card rects from the last frame, for mouse hit-testing.
    pub hits: Vec<(Rect, String)>,
    /// The selected card's action buttons from the last frame.
    pub act_hits: Vec<(Rect, Act)>,
    pub last_click: Option<(String, Instant)>,
    pub git_paths: Arc<Mutex<Vec<String>>>,
    pub tx: Sender<Msg>,
    pub kicks: Vec<Sender<()>>,
    pub quit: bool,
    slides: HashMap<String, Slide>,
    pulses: HashMap<String, Instant>,
    prev_status: HashMap<String, String>,
}

impl App {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ws: String,
        here: String,
        repo: String,
        root: String,
        base: String,
        plugin_root: String,
        git_paths: Arc<Mutex<Vec<String>>>,
        tx: Sender<Msg>,
        kicks: Vec<Sender<()>>,
    ) -> Self {
        Self {
            ws,
            here,
            repo,
            root,
            base,
            plugin_root,
            git_paths,
            tx,
            kicks,
            snap: HerdrSnap::default(),
            git: HashMap::new(),
            prs: Vec::new(),
            pr_at: 0,
            rows: Vec::new(),
            selected: None,
            hover: None,
            expanded: false,
            hidden: load_hidden(),
            show_hidden: false,
            legend: false,
            filter: String::new(),
            mode: Mode::Normal,
            status: None,
            started: Instant::now(),
            scroll: 0,
            hits: Vec::new(),
            act_hits: Vec::new(),
            last_click: None,
            quit: false,
            slides: HashMap::new(),
            pulses: HashMap::new(),
            prev_status: HashMap::new(),
        }
    }

    pub fn apply(&mut self, msg: Msg) {
        match msg {
            Msg::Herdr(s) => {
                let paths: Vec<String> = s.worktrees.iter().map(|w| w.path.clone()).collect();
                let changed = self.git_paths.lock().map(|mut p| std::mem::replace(&mut *p, paths.clone()) != paths);
                if changed.unwrap_or(false) {
                    // New or removed checkouts: rescan git now instead of at the next 10s tick.
                    let _ = self.kicks[GIT_KICK].send(());
                }
                self.snap = s;
            }
            Msg::Git(g) => self.git = g,
            Msg::Pr(p, at) => {
                self.prs = p;
                self.pr_at = at;
            }
            Msg::Status(text, ok) => {
                self.status = Some((text, ok, Instant::now()));
                self.kick();
            }
        }
        self.rebuild();
    }

    pub fn kick(&self) {
        for k in &self.kicks {
            let _ = k.send(());
        }
    }

    pub fn rebuild(&mut self) {
        let paths: Vec<&str> = self.snap.worktrees.iter().map(|w| w.path.as_str()).collect();
        // An agent belongs to the deepest worktree containing its cwd: the main checkout
        // is a parent directory of every `.worktrees/*` checkout.
        let owner = |cwd: &str| {
            paths
                .iter()
                .filter(|p| cwd == **p || cwd.strip_prefix(**p).is_some_and(|r| r.starts_with('/')))
                .max_by_key(|p| p.len())
                .map(|p| p.to_string())
        };
        let mut by_owner: HashMap<String, Vec<Agent>> = HashMap::new();
        for (cwd, a) in &self.snap.agents {
            if let Some(o) = owner(cwd) {
                by_owner.entry(o).or_default().push(a.clone());
            }
        }
        let mut rows: Vec<Row> = self
            .snap
            .worktrees
            .iter()
            .map(|w| {
                let mut agents = by_owner.remove(&w.path).unwrap_or_default();
                agents.sort_by_key(|a| urgency(&a.status));
                let place = if w.prunable {
                    Place::Prunable
                } else if w.open_ws.as_deref() == Some(self.ws.as_str()) || w.path == self.here {
                    Place::Here
                } else if w.open_ws.is_some() {
                    Place::Open
                } else {
                    Place::Closed
                };
                Row {
                    path: w.path.clone(),
                    branch: w.branch.clone(),
                    place,
                    linked: w.linked,
                    open_ws: w.open_ws.clone(),
                    agents,
                    git: self.git.get(&w.path).cloned(),
                    pr: self.prs.iter().find(|p| p.b == w.branch).cloned(),
                }
            })
            .filter(|r| fuzzy(&self.filter, &r.branch))
            .filter(|r| self.show_hidden || !self.hidden.contains(&r.path))
            .collect();
        rows.sort_by_key(Row::rank);

        for r in &rows {
            let st = r.top_status().to_string();
            if st == "blocked" && self.prev_status.get(&r.path).map(String::as_str) != Some("blocked") {
                self.pulses.insert(r.path.clone(), Instant::now());
            }
            self.prev_status.insert(r.path.clone(), st);
        }
        self.rows = rows;
        if !self.selected.as_ref().is_some_and(|s| self.rows.iter().any(|r| &r.path == s)) {
            self.selected = self.rows.first().map(|r| r.path.clone());
        }
    }

    pub fn selected_row(&self) -> Option<&Row> {
        self.rows.iter().find(|r| Some(&r.path) == self.selected.as_ref())
    }

    pub fn move_sel(&mut self, d: i32) {
        let n = self.rows.len() as i32;
        if n == 0 {
            return;
        }
        let cur = self.rows.iter().position(|r| Some(&r.path) == self.selected.as_ref()).unwrap_or(0) as i32;
        self.selected = Some(self.rows[(cur + d).clamp(0, n - 1) as usize].path.clone());
    }

    /// Record a card's target y; returns the eased y to draw it at this frame.
    pub fn slide_y(&mut self, path: &str, target: f32) -> f32 {
        let now = Instant::now();
        let s = self.slides.entry(path.to_string()).or_insert(Slide { from: target, to: target, start: now });
        if (s.to - target).abs() > f32::EPSILON {
            let cur = ease(s, now);
            *s = Slide { from: cur, to: target, start: now };
        }
        ease(s, now)
    }

    pub fn animating(&self) -> bool {
        let now = Instant::now();
        self.slides.values().any(|s| now - s.start < SLIDE) || self.pulses.values().any(|p| now - *p < PULSE)
    }

    pub fn pulse_on(&self, path: &str) -> bool {
        self.pulses.get(path).is_some_and(|p| p.elapsed() < PULSE && (p.elapsed().as_millis() / 150) % 2 == 0)
    }

    pub fn frame(&self) -> usize {
        (self.started.elapsed().as_millis() / 80) as usize
    }

    pub fn flash(&mut self, text: impl Into<String>, ok: bool) {
        self.status = Some((text.into(), ok, Instant::now()));
    }

    // ---- actions -------------------------------------------------------------------------

    fn background(&self, job: impl FnOnce() -> Result<String, String> + Send + 'static) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let (text, ok) = match job() {
                Ok(t) => (t, true),
                Err(e) => (e, false),
            };
            let _ = tx.send(Msg::Status(text, ok));
        });
    }

    pub fn open_selected(&mut self) {
        let Some(r) = self.selected_row() else { return };
        let (ws, path, branch) = (self.ws.clone(), r.path.clone(), r.branch.clone());
        self.background(move || {
            herdr(&["worktree", "open", "--workspace", &ws, "--path", &path, "--focus"])
                .map(|_| format!("opened {branch}"))
        });
    }

    pub fn create(&mut self, branch: String) {
        let (ws, base) = (self.ws.clone(), self.base.clone());
        self.flash(format!("creating {branch}…"), true);
        self.background(move || {
            herdr(&["worktree", "create", "--workspace", &ws, "--branch", &branch, "--base", &base, "--focus"])
                .map(|_| format!("created {branch}"))
        });
    }

    /// A click on the selected card's action row: the same as its key.
    pub fn act(&mut self, a: Act) {
        match a {
            Act::Open => self.open_selected(),
            Act::Claude => self.claude(),
            Act::Pr => self.open_pr(),
            Act::Hide => self.toggle_hidden(),
            Act::Delete => {
                if let Some(r) = self.selected_row().filter(|r| r.linked) {
                    self.mode = Mode::ConfirmDelete(r.path.clone());
                }
            }
        }
    }

    /// `x`: take the selected worktree off the board, or put a hidden one back.
    pub fn toggle_hidden(&mut self) {
        let Some(r) = self.selected_row() else { return };
        let (path, branch) = (r.path.clone(), r.branch.clone());
        let text = if self.hidden.remove(&path) {
            format!("{branch} back on the board")
        } else {
            self.hidden.insert(path);
            format!("hid {branch} · H shows hidden")
        };
        save_hidden(&self.hidden);
        self.flash(text, true);
        self.rebuild();
    }

    /// `d` confirmed. `force` drops uncommitted changes; `branch` also deletes the branch with
    /// `git branch -d`, which refuses an unmerged one (that is reported, never forced).
    pub fn delete(&mut self, path: String, branch: bool, force: bool) {
        let Some(r) = self.rows.iter().find(|r| r.path == path).cloned() else { return };
        if !r.linked {
            return self.flash("main checkout can't be removed", false);
        }
        if self.hidden.remove(&path) {
            save_hidden(&self.hidden);
        }
        let root = self.root.clone();
        self.flash(format!("removing {}…", r.branch), true);
        self.background(move || {
            let mut args = vec!["-C", root.as_str(), "worktree", "remove"];
            if force {
                args.push("--force");
            }
            args.push(&r.path);
            match (&r.open_ws, r.place) {
                // The folder is gone already: only git's record of it is left.
                (_, Place::Prunable) => cmd("git", &["-C", &root, "worktree", "prune"]),
                // Let herdr close the workspace and run `git worktree remove` itself.
                (Some(ws), _) if force => herdr(&["worktree", "remove", "--workspace", ws, "--force"]),
                (Some(ws), _) => herdr(&["worktree", "remove", "--workspace", ws]),
                (None, _) => cmd("git", &args),
            }?;
            let mut text = format!("removed {}", r.branch);
            if branch && !r.branch.is_empty() {
                match cmd("git", &["-C", &root, "branch", "-d", &r.branch]) {
                    Ok(_) => text.push_str(" + branch"),
                    Err(_) => text.push_str("; branch kept (not merged)"),
                }
            }
            Ok(text)
        });
    }

    pub fn claude(&mut self) {
        let Some(r) = self.selected_row() else { return };
        let (ws, path, branch) = (self.ws.clone(), r.path.clone(), r.branch.clone());
        self.background(move || {
            let out = herdr(&["worktree", "open", "--workspace", &ws, "--path", &path, "--focus"])?;
            let v: serde_json::Value = serde_json::from_str(&out).map_err(|e| e.to_string())?;
            let target_ws = v["result"]["workspace"]["workspace_id"].as_str().unwrap_or_default().to_string();
            let pane = v["result"]["root_pane"]["pane_id"].as_str().map(String::from).or_else(|| {
                let panes = data::herdr_json(&["pane", "list"])?;
                panes["result"]["panes"].as_array()?.iter().find_map(|p| {
                    (p["workspace_id"] == target_ws.as_str() && p["label"] != super::LABEL)
                        .then(|| p["pane_id"].as_str().map(String::from))
                        .flatten()
                })
            });
            let pane = pane.ok_or("no pane to split")?;
            let split = herdr(&["pane", "split", &pane, "--direction", "down", "--focus"])?;
            let v: serde_json::Value = serde_json::from_str(&split).map_err(|e| e.to_string())?;
            let new = v["result"]["pane"]["pane_id"].as_str().ok_or("split returned no pane")?;
            herdr(&["pane", "run", new, "claude"]).map(|_| format!("claude started in {branch}"))
        });
    }

    pub fn open_pr(&mut self) {
        match self.selected_row().and_then(|r| r.pr.clone()) {
            Some(pr) if !pr.url.is_empty() => {
                let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
                let _ = Command::new(opener)
                    .arg(&pr.url)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn();
                self.flash(format!("opening #{}", pr.n), true);
            }
            _ => self.flash("no PR for this branch", false),
        }
    }

    /// OSC 52: herdr forwards clipboard writes to the outer terminal.
    pub fn yank(&mut self) {
        let Some(path) = self.selected.clone() else { return };
        use std::io::Write;
        let mut out = std::io::stdout();
        let _ = write!(out, "\x1b]52;c;{}\x07", base64(path.as_bytes()));
        let _ = out.flush();
        self.flash("path copied", true);
    }

    /// `q`: close the board in this tab only.
    pub fn close_here(&mut self) {
        crate::tui::close_here("worktrees");
        self.quit = true;
    }

    /// `Q`: hide the board in every tab and stop it auto-opening.
    pub fn hide(&mut self) {
        let _ =
            Command::new("bash").arg(format!("{}/sidekick.sh", self.plugin_root)).args(["off", "worktrees"]).stdin(Stdio::null()).status();
        self.quit = true;
    }
}

fn load_hidden() -> HashSet<String> {
    crate::tui::state_file("hidden-worktrees")
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| t.lines().filter(|l| !l.is_empty()).map(String::from).collect())
        .unwrap_or_default()
}

fn save_hidden(hidden: &HashSet<String>) {
    if let Some(p) = crate::tui::state_file("hidden-worktrees") {
        let mut lines: Vec<&str> = hidden.iter().map(String::as_str).collect();
        lines.sort_unstable();
        let _ = std::fs::write(p, lines.join("\n"));
    }
}

fn ease(s: &Slide, now: Instant) -> f32 {
    let t = ((now - s.start).as_secs_f32() / SLIDE.as_secs_f32()).min(1.0);
    let e = 1.0 - (1.0 - t).powi(3); // ease-out cubic
    s.from + (s.to - s.from) * e
}

/// Case-insensitive subsequence match, like most fuzzy finders' first pass.
pub fn fuzzy(needle: &str, hay: &str) -> bool {
    let mut it = hay.chars().flat_map(char::to_lowercase);
    needle.chars().flat_map(char::to_lowercase).all(|c| it.any(|h| h == c))
}

fn cmd(bin: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(bin).args(args).stdin(Stdio::null()).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let out_text = String::from_utf8_lossy(&out.stdout);
        let msg = if err.trim().is_empty() { out_text } else { err };
        Err(msg.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("failed").trim().to_string())
    }
}

fn herdr(args: &[&str]) -> Result<String, String> {
    cmd(&data::herdr_bin(), args)
}

fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            out.push(if i <= chunk.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::data::WtRaw;

    const MAIN: &str = "/repo";
    const FEAT: &str = "/repo/.worktrees/feat";

    fn app() -> App {
        let (tx, _rx) = std::sync::mpsc::channel();
        let kicks = (0..3).map(|_| std::sync::mpsc::channel().0).collect();
        App::new(
            "w1".into(),
            String::new(),
            "repo".into(),
            MAIN.into(),
            "origin/main".into(),
            String::new(),
            Arc::new(Mutex::new(Vec::new())),
            tx,
            kicks,
        )
    }

    fn wt(path: &str, branch: &str) -> WtRaw {
        WtRaw { path: path.into(), branch: branch.into(), prunable: false, linked: path != MAIN, open_ws: None }
    }

    fn agent(status: &str) -> Agent {
        Agent { status: status.into(), name: "claude".into(), title: String::new() }
    }

    fn snap(agents: Vec<(&str, &str)>) -> Msg {
        Msg::Herdr(HerdrSnap {
            worktrees: vec![wt(MAIN, "main"), wt(FEAT, "feat")],
            agents: agents.into_iter().map(|(cwd, st)| (cwd.to_string(), agent(st))).collect(),
            error: None,
        })
    }

    fn row<'a>(a: &'a App, path: &str) -> &'a Row {
        a.rows.iter().find(|r| r.path == path).expect("row")
    }

    #[test]
    fn hidden_worktree_leaves_the_board_until_shown_or_unhidden() {
        let mut a = app();
        a.apply(snap(vec![]));
        a.selected = Some(FEAT.into());
        a.toggle_hidden();
        assert!(a.rows.iter().all(|r| r.path != FEAT), "x takes it off the board");
        a.show_hidden = true;
        a.rebuild();
        assert!(a.rows.iter().any(|r| r.path == FEAT), "H shows it again");
        a.selected = Some(FEAT.into());
        a.toggle_hidden();
        a.show_hidden = false;
        a.rebuild();
        assert!(a.rows.iter().any(|r| r.path == FEAT), "x on a hidden one puts it back");
    }

    /// A real repo with one linked worktree on branch `feat`; returns (root, worktree path).
    fn git_repo(name: &str) -> (String, String) {
        let root = std::env::temp_dir().join(format!("sidekick-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("tmp dir");
        let root = root.to_string_lossy().into_owned();
        let wt = format!("{root}/.worktrees/feat");
        let git = |args: &[&str]| cmd("git", &[&["-C", &root, "-c", "user.name=t", "-c", "user.email=t@t"], args].concat()).expect("git");
        git(&["init", "-q", "-b", "main"]);
        git(&["commit", "-q", "--allow-empty", "-m", "init"]);
        git(&["worktree", "add", "-q", "-b", "feat", &wt]);
        (root, wt)
    }

    fn delete_and_wait(root: &str, wt: &str, branch: bool, force: bool) -> (String, bool) {
        let (tx, rx) = std::sync::mpsc::channel();
        let kicks = (0..3).map(|_| std::sync::mpsc::channel().0).collect();
        let mut a = App::new("w1".into(), String::new(), "repo".into(), root.into(), "main".into(), String::new(), Arc::new(Mutex::new(Vec::new())), tx, kicks);
        a.apply(Msg::Herdr(HerdrSnap {
            worktrees: vec![
                WtRaw { path: root.into(), branch: "main".into(), prunable: false, linked: false, open_ws: None },
                WtRaw { path: wt.into(), branch: "feat".into(), prunable: false, linked: true, open_ws: None },
            ],
            agents: Vec::new(),
            error: None,
        }));
        a.delete(wt.into(), branch, force);
        match rx.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(Msg::Status(text, ok)) => (text, ok),
            _ => panic!("no status from delete"),
        }
    }

    fn has_branch(root: &str, b: &str) -> bool {
        cmd("git", &["-C", root, "rev-parse", "--verify", "-q", &format!("refs/heads/{b}")]).is_ok()
    }

    #[test]
    fn delete_with_branch_removes_a_merged_branch_but_keeps_an_unmerged_one() {
        let (root, wt) = git_repo("merged");
        let (text, ok) = delete_and_wait(&root, &wt, true, false);
        assert!(ok && text.ends_with("+ branch"), "{text}");
        assert!(!std::path::Path::new(&wt).exists() && !has_branch(&root, "feat"));
        let _ = std::fs::remove_dir_all(&root);

        let (root, wt) = git_repo("unmerged");
        cmd("git", &["-C", &wt, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "work"]).expect("commit");
        let (text, ok) = delete_and_wait(&root, &wt, true, false);
        assert!(ok && text.contains("branch kept"), "{text}");
        assert!(has_branch(&root, "feat"), "an unmerged branch is never force-deleted");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn uncommitted_work_survives_a_plain_delete_and_goes_only_with_force() {
        let (root, wt) = git_repo("dirty");
        std::fs::write(format!("{wt}/notes.txt"), "keep me").expect("write");
        let (_, ok) = delete_and_wait(&root, &wt, false, false);
        assert!(!ok && std::path::Path::new(&format!("{wt}/notes.txt")).exists(), "git refuses without force");
        let (text, ok) = delete_and_wait(&root, &wt, false, true);
        assert!(ok && !std::path::Path::new(&wt).exists(), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn agent_in_nested_checkout_belongs_to_that_worktree_not_the_parent() {
        let mut a = app();
        a.apply(snap(vec![("/repo/.worktrees/feat/apps/api", "working"), ("/repo/apps/web", "idle")]));
        assert_eq!(row(&a, FEAT).top_status(), "working");
        assert_eq!(row(&a, MAIN).top_status(), "idle");
    }

    #[test]
    fn sibling_path_with_same_prefix_is_not_owned() {
        let mut a = app();
        a.apply(snap(vec![("/repo2", "working")]));
        assert!(a.rows.iter().all(|r| r.agents.is_empty()));
    }

    #[test]
    fn worktree_needing_you_sorts_above_failed_ci_and_busy_ones() {
        let mut a = app();
        a.apply(Msg::Pr(vec![PrInfo { b: "main".into(), n: 1, ci: "FAILURE".into(), ..Default::default() }], 1));
        a.apply(snap(vec![(FEAT, "blocked")]));
        assert_eq!(a.rows[0].path, FEAT, "blocked agent first");
        assert_eq!(a.rows[1].path, MAIN, "failed CI next");
    }

    #[test]
    fn pulse_starts_only_on_transition_into_blocked() {
        let mut a = app();
        a.apply(snap(vec![(FEAT, "working")]));
        assert!(!a.pulses.contains_key(FEAT));
        a.apply(snap(vec![(FEAT, "blocked")]));
        let first = *a.pulses.get(FEAT).expect("pulse on transition");
        a.apply(snap(vec![(FEAT, "blocked")]));
        assert_eq!(a.pulses[FEAT], first, "staying blocked must not restart the pulse");
    }

    #[test]
    fn selection_follows_path_across_resort() {
        let mut a = app();
        a.apply(snap(vec![]));
        a.selected = Some(MAIN.into());
        a.apply(snap(vec![(FEAT, "blocked")]));
        assert_eq!(a.rows[0].path, FEAT);
        assert_eq!(a.selected.as_deref(), Some(MAIN));
    }

    #[test]
    fn reordered_card_eases_from_old_slot_instead_of_jumping() {
        let mut a = app();
        assert_eq!(a.slide_y(FEAT, 8.0), 8.0, "first placement is immediate");
        let y = a.slide_y(FEAT, 0.0);
        assert!(y > 0.0 && y <= 8.0, "starts near the old slot, got {y}");
        std::thread::sleep(SLIDE);
        assert_eq!(a.slide_y(FEAT, 0.0), 0.0, "settles on the new slot");
    }

    #[test]
    fn fuzzy_is_case_insensitive_subsequence() {
        assert!(fuzzy("TrAd", "fix/tradingview-username-validation"));
        assert!(fuzzy("fxtv", "fix/tradingview"));
        assert!(!fuzzy("vt", "tv"));
    }

    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(base64(b"/a/b"), "L2EvYg==");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(b"fo"), "Zm8=");
    }
}
