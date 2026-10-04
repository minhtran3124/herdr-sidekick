//! Claude Code transcripts on disk: which session a pane runs, and that session's subagents.
//!
//! Layout (Claude Code 2.1): `<config>/projects/<project>/<session>/subagents/agent-<id>.jsonl`
//! plus `agent-<id>.meta.json`; workflow agents sit under `subagents/workflows/<run>/`.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

/// No write for this long while it is the model's turn: most likely stopped (killed, crashed).
pub const QUIET_MS: i64 = 15 * 60 * 1000;
/// Same, while a tool call is open; test suites and builds legitimately run long.
pub const TOOL_QUIET_MS: i64 = 60 * 60 * 1000;
pub const STRUCTURED_OUTPUT: &str = "StructuredOutput";
const AGENT_TOOLS: [&str; 2] = ["Agent", "Task"];

/// Where a status sorts in the panel: what needs the user first, then live work, then history.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Section {
    Attention,
    Active,
    Finished,
}

#[derive(Clone, PartialEq)]
pub enum Status {
    /// Prompt sent, no reply yet.
    Starting,
    /// The model's turn: thinking or writing.
    Thinking,
    /// A tool call is open (`since` = when it was issued).
    Tool { name: String, since: i64 },
    /// Waiting on child agents (open Agent calls or background launches) and background
    /// commands that have not reported back, even after the agent ended its turn.
    Waiting { agents: usize, tasks: usize },
    /// The parent Claude pane shows an approval prompt and this foreground agent has an open call.
    Approval { name: String },
    Done,
    /// Finished but reported DONE_WITH_CONCERNS.
    Concerns,
    /// Finished but reported BLOCKED or NEEDS_CONTEXT.
    Reported(String),
    /// Ended on an API error (rate limit, overload, …).
    Failed(String),
    Interrupted,
    /// Failed or reported a block, then the same task was dispatched again (see `mark_retried`).
    Retried,
    /// Not finished and silent for a long time.
    Stopped,
}

impl Status {
    pub fn section(&self) -> Section {
        match self {
            Status::Approval { .. } | Status::Reported(_) | Status::Failed(_) => Section::Attention,
            Status::Starting | Status::Thinking | Status::Tool { .. } | Status::Waiting { .. } => Section::Active,
            Status::Done | Status::Concerns | Status::Interrupted | Status::Stopped | Status::Retried => Section::Finished,
        }
    }

    pub fn is_live(&self) -> bool {
        self.section() == Section::Active || matches!(self, Status::Approval { .. })
    }

    /// Short label for the panel and the transcript header.
    pub fn label(&self, now: i64) -> String {
        match self {
            Status::Starting => "starting".into(),
            Status::Thinking => "thinking".into(),
            Status::Tool { name, since } => format!("{name} {}", fmt_dur(now - since)),
            Status::Waiting { agents, tasks } => {
                let part = |n: usize, what: &str| format!("{n} {what}{}", if n == 1 { "" } else { "s" });
                match (agents, tasks) {
                    (0, t) => format!("waiting on {}", part(*t, "bg task")),
                    (a, 0) => format!("waiting on {}", part(*a, "agent")),
                    (a, t) => format!("waiting on {} + {}", part(*a, "agent"), part(*t, "bg task")),
                }
            }
            Status::Approval { name } => format!("needs approval: {name}"),
            Status::Done => "done".into(),
            Status::Concerns => "done, concerns".into(),
            Status::Reported(s) if s == "NEEDS_CONTEXT" => "needs context".into(),
            Status::Reported(s) => format!("reported {s}"),
            Status::Failed(e) => format!("failed: {e}"),
            Status::Interrupted => "interrupted".into(),
            Status::Stopped => "stopped?".into(),
            Status::Retried => "retried".into(),
        }
    }
}

#[derive(Clone)]
pub struct Agent {
    pub id: String,
    pub path: PathBuf,
    pub kind: String,
    pub desc: String,
    pub parent: Option<String>,
    /// Workflow run directory name (`wf_…`) for workflow agents.
    pub group: Option<String>,
    pub start_ms: i64,
    pub last_ms: i64,
    pub status: Status,
    pub tools: usize,
    pub errors: usize,
    pub activity: String,
    /// Working directory the agent ran in; its pane opens there.
    pub cwd: String,
}

impl Agent {
    /// Pane label of this agent's split pane; `#<id8>` lets the list find it again.
    pub fn pane_label(&self) -> String {
        pane_label(&self.desc, &self.kind, &self.id)
    }

    pub fn elapsed_ms(&self, now: i64) -> i64 {
        let end = if self.status.is_live() { now } else { self.last_ms };
        (end - self.start_ms).max(0)
    }
}

/// Everything the status needs from one transcript, fed record by record.
#[derive(Default)]
pub struct Summary {
    offset: u64,
    pub start_ms: i64,
    pub last_ms: i64,
    replied: bool,
    ended: bool,
    /// Workflow agents finish by calling StructuredOutput; its accepted result ends the run.
    structured: bool,
    interrupted: bool,
    failed: Option<String>,
    reported: Option<String>,
    /// Open tool calls by id: (name, summary, issued at).
    open: HashMap<String, (String, String, i64)>,
    /// Background work started and not yet reported by a task notification: id -> is an agent.
    background: HashMap<String, bool>,
    pub tools: usize,
    pub errors: usize,
    pub activity: String,
    pub cwd: String,
}

impl Summary {
    pub fn feed(&mut self, v: &Value) {
        let ts = v["timestamp"].as_str().and_then(parse_ts);
        if let Some(ms) = ts {
            if self.start_ms == 0 {
                self.start_ms = ms;
            }
            self.last_ms = ms;
        }
        if self.cwd.is_empty() {
            if let Some(c) = v["cwd"].as_str() {
                self.cwd = c.to_string();
            }
        }
        let msg = &v["message"];
        let blocks = || msg["content"].as_array().into_iter().flatten();
        match v["type"].as_str() {
            Some("assistant") => {
                self.replied = true;
                self.interrupted = false;
                if v["isApiErrorMessage"] == true {
                    self.failed = Some(v["error"].as_str().unwrap_or("api error").to_string());
                    return;
                }
                self.failed = None;
                for b in blocks() {
                    match b["type"].as_str() {
                        Some("tool_use") => {
                            self.tools += 1;
                            let name = b["name"].as_str().unwrap_or("tool").to_string();
                            let summary = tool_summary(&b["input"], &self.cwd);
                            self.structured = name == STRUCTURED_OUTPUT;
                            self.activity = format!("{name} {summary}");
                            if let Some(id) = b["id"].as_str() {
                                self.open.insert(id.to_string(), (name, summary, ts.unwrap_or(self.last_ms)));
                            }
                        }
                        Some("text") => {
                            let t = b["text"].as_str().unwrap_or("");
                            self.activity = format!("✎ {}", t.trim().lines().next().unwrap_or(""));
                            if let Some(s) = reported_status(t) {
                                self.reported = Some(s.to_string());
                            }
                        }
                        Some("thinking") => self.activity = "∴ thinking".into(),
                        _ => {}
                    }
                }
                self.ended = matches!(msg["stop_reason"].as_str(), Some("end_turn" | "stop_sequence"));
            }
            Some("user") => {
                let results = || blocks().filter(|b| b["type"] == "tool_result");
                self.errors += results().filter(|b| b["is_error"] == true).count();
                for r in results() {
                    if let Some(id) = r["tool_use_id"].as_str() {
                        self.open.remove(id);
                    }
                }
                let out = &v["toolUseResult"];
                if let Some(id) = out["backgroundTaskId"].as_str() {
                    self.background.insert(id.to_string(), false);
                } else if let (Some(id), "async_launched") = (out["agentId"].as_str(), out["status"].as_str().unwrap_or("")) {
                    self.background.insert(id.to_string(), true);
                }
                self.ended = self.structured && results().any(|b| b["is_error"] != true);
                self.structured = false;
                let text = msg["content"].as_str().map(String::from).unwrap_or_else(|| {
                    blocks().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n")
                });
                // Completed, failed or killed: any notification means that task stopped running.
                for chunk in text.split("<task-id>").skip(1) {
                    if let Some(id) = chunk.split("</task-id>").next() {
                        self.background.remove(id.trim());
                    }
                }
                if text.contains("[Request interrupted by user") {
                    self.interrupted = true;
                    self.open.clear();
                } else if !self.ended {
                    // A new message (e.g. SendMessage) restarts the turn; a status from before is stale.
                    self.reported = None;
                }
            }
            _ => {}
        }
    }

    /// `mtime` = the transcript's last write; `can_ask` = a foreground agent whose permission
    /// prompts surface in the parent pane, which herdr reports as blocked.
    /// `told`: the parent's record of this agent finishing (see `Parents`).
    pub fn status(&self, now: i64, mtime: i64, parent_blocked: bool, can_ask: bool, told: Option<&Told>) -> Status {
        if let Some(e) = &self.failed {
            return Status::Failed(e.clone());
        }
        // Heard after the agent's last record, so it is about this run, not one resumed since.
        // About 2% of finished transcripts never get an end_turn record; this covers them.
        let told = told.filter(|t| t.ts + 2000 >= self.last_ms);
        match told.map(|t| t.status.as_str()) {
            Some("failed") => return Status::Failed("failed".into()),
            Some("killed") => return Status::Interrupted,
            _ => {}
        }
        let ended = self.ended || told.is_some();
        let silent = now - mtime.max(self.last_ms);
        let bg_agents = self.background.values().filter(|a| **a).count();
        let bg_tasks = self.background.len() - bg_agents;
        if ended && !self.background.is_empty() && !self.interrupted && silent <= TOOL_QUIET_MS {
            return Status::Waiting { agents: bg_agents, tasks: bg_tasks };
        }
        if ended {
            return match self.reported.as_deref() {
                Some(s @ ("BLOCKED" | "NEEDS_CONTEXT")) => Status::Reported(s.to_string()),
                Some("DONE_WITH_CONCERNS") => Status::Concerns,
                _ => Status::Done,
            };
        }
        if self.interrupted {
            return Status::Interrupted;
        }
        if let Some((name, _, since)) = self.open.values().max_by_key(|(_, _, t)| *t) {
            if parent_blocked && can_ask {
                return Status::Approval { name: name.clone() };
            }
            if silent > TOOL_QUIET_MS {
                return Status::Stopped;
            }
            let n = self.open.values().filter(|(n, _, _)| AGENT_TOOLS.contains(&n.as_str())).count();
            if n > 0 {
                return Status::Waiting { agents: n, tasks: 0 };
            }
            return Status::Tool { name: name.clone(), since: *since };
        }
        if silent > QUIET_MS {
            return Status::Stopped;
        }
        if self.replied { Status::Thinking } else { Status::Starting }
    }

    /// The open call's summary while one runs, else the last thing the agent did.
    pub fn current(&self) -> String {
        match self.open.values().max_by_key(|(_, _, t)| *t) {
            Some((_, s, _)) => s.clone(),
            None => self.activity.clone(),
        }
    }
}

pub const PANE_PREFIX: &str = "◇ ";

pub fn pane_label(desc: &str, kind: &str, id: &str) -> String {
    let title = if desc.is_empty() { kind } else { desc };
    let short: String = title.chars().take(28).collect();
    format!("{PANE_PREFIX}{short} #{}", id.get(..8).unwrap_or(id))
}

/// A failed or blocked agent stops needing the user once the same task (same type and
/// description) was dispatched again after it, which is how the parent retries.
pub fn mark_retried(agents: &mut [Agent]) {
    let retried: Vec<bool> = agents
        .iter()
        .map(|a| {
            matches!(a.status, Status::Failed(_) | Status::Reported(_))
                && !a.desc.is_empty()
                && agents.iter().any(|b| b.desc == a.desc && b.kind == a.kind && b.start_ms > a.start_ms)
        })
        .collect();
    for (a, r) in agents.iter_mut().zip(retried) {
        if r {
            a.status = Status::Retried;
        }
    }
}

/// The SDD-style status line an agent ends its report with: `**Status:** BLOCKED — …`.
fn reported_status(text: &str) -> Option<&'static str> {
    const WORDS: [&str; 4] = ["DONE_WITH_CONCERNS", "NEEDS_CONTEXT", "BLOCKED", "DONE"];
    text.lines().find_map(|line| {
        let at = line.to_ascii_lowercase().find("status")?;
        let rest = &line[at + 6..];
        let word_at = rest.find(|c: char| !matches!(c, '*' | ':' | ' ' | '`' | '_'))?;
        if !rest[..word_at].contains(':') {
            return None;
        }
        WORDS.into_iter().find(|w| rest[word_at..].starts_with(w))
    })
}

/// A parent transcript saying a child stopped: `<task-notification>` for background agents
/// (status completed, failed or killed), or the Agent call's tool_result for foreground ones.
#[derive(Clone)]
pub struct Told {
    pub ts: i64,
    pub status: String,
}

/// Completion records from parent transcripts (the session's main transcript for top-level
/// agents, the parent agent's for nested ones), read incrementally. Only lines carrying a task
/// id or a tool result are parsed, so a multi-MB main transcript stays cheap.
#[derive(Default)]
pub struct Parents {
    logs: HashMap<PathBuf, ParentLog>,
    /// Scan counter (`new_scan`): a parent shared by many agents is read once per scan.
    /// 0 = no scans, read on every call.
    scan: u64,
}

#[derive(Default)]
struct ParentLog {
    read_in_scan: u64,
    offset: u64,
    /// task id -> last notification about it.
    notes: HashMap<String, Told>,
    /// tool_use_id -> when its result came back (async launches excluded).
    results: HashMap<String, i64>,
}

impl Parents {
    pub fn new_scan(&mut self) {
        self.scan += 1;
    }

    /// `agent_path` = `…/<session>/subagents/[workflows/<run>/]agent-<id>.jsonl`.
    pub fn told(&mut self, agent_path: &Path, meta: &Value) -> Option<Told> {
        let id = agent_path.file_stem()?.to_str()?.strip_prefix("agent-")?;
        let parent = match meta["parentAgentId"].as_str() {
            Some(p) => agent_path.with_file_name(format!("agent-{p}.jsonl")),
            None => {
                let subagents = agent_path.ancestors().find(|a| a.file_name().is_some_and(|n| n == "subagents"))?;
                subagents.parent()?.with_extension("jsonl")
            }
        };
        let log = self.logs.entry(parent.clone()).or_default();
        if self.scan == 0 || log.read_in_scan != self.scan {
            log.read_in_scan = self.scan;
            log.advance(&parent);
        }
        let by_result = || {
            let ts = *log.results.get(meta["toolUseId"].as_str()?)?;
            Some(Told { ts, status: "completed".into() })
        };
        log.notes.get(id).cloned().or_else(by_result)
    }
}

impl ParentLog {
    fn advance(&mut self, path: &Path) {
        let Ok(mut f) = File::open(path) else { return };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.offset {
            *self = ParentLog::default();
        }
        if len == self.offset || f.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut buf = Vec::new();
        if f.read_to_end(&mut buf).is_err() {
            return;
        }
        let Some(end) = buf.iter().rposition(|&b| b == b'\n') else { return };
        for line in buf[..end].split(|&b| b == b'\n') {
            let has = |needle: &[u8]| line.windows(needle.len()).any(|w| w == needle);
            if !has(b"<task-id>") && !has(b"\"tool_result\"") {
                continue;
            }
            let Ok(v) = serde_json::from_slice::<Value>(line) else { continue };
            let ts = v["timestamp"].as_str().and_then(parse_ts).unwrap_or(0);
            let text = String::from_utf8_lossy(line);
            for chunk in text.split("<task-id>").skip(1) {
                let Some(id) = chunk.split("</task-id>").next() else { continue };
                let status = chunk.split("<status>").nth(1).and_then(|r| r.split("</status>").next()).unwrap_or("completed");
                self.notes.insert(id.trim().to_string(), Told { ts, status: status.trim().to_string() });
            }
            if v["toolUseResult"]["status"] == "async_launched" {
                continue;
            }
            for b in v["message"]["content"].as_array().into_iter().flatten() {
                if let (Some("tool_result"), Some(id)) = (b["type"].as_str(), b["tool_use_id"].as_str()) {
                    self.results.insert(id.to_string(), ts);
                }
            }
        }
        self.offset += end as u64 + 1;
    }
}

/// Caches per-transcript summaries so each refresh only reads what was appended since the last one.
#[derive(Default)]
pub struct Scanner {
    sums: HashMap<PathBuf, Summary>,
    metas: HashMap<PathBuf, Value>,
    parents: Parents,
    workflow_names: HashMap<String, String>,
}

impl Scanner {
    /// `parent_blocked`: herdr shows the session's Claude pane waiting at an approval prompt.
    /// `min_mtime` skips transcripts not written since then without reading them (0 = all).
    pub fn scan(&mut self, session_dir: &Path, parent_blocked: bool, min_mtime: i64) -> Vec<Agent> {
        self.parents.new_scan();
        let mut metas = Vec::new();
        collect_metas(&session_dir.join("subagents"), None, &mut metas);
        let now = now_ms();
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for (meta_path, group) in metas {
            let id = meta_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let id = id.trim_start_matches("agent-").trim_end_matches(".meta.json").to_string();
            let path = meta_path.with_file_name(format!("agent-{id}.jsonl"));
            if min_mtime > 0 && file_mtime(&path).unwrap_or(0) < min_mtime {
                continue;
            }
            let meta = self
                .metas
                .entry(meta_path.clone())
                .or_insert_with(|| std::fs::read(&meta_path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default());
            let kind = meta["agentType"].as_str().unwrap_or("agent").to_string();
            let desc = meta["description"].as_str().unwrap_or("").to_string();
            let parent = meta["parentAgentId"].as_str().map(String::from);
            let can_ask = meta["requestNonInteractive"] != true;
            let sum = self.sums.entry(path.clone()).or_default();
            advance(sum, &path);
            seen.insert(path.clone());
            let mtime = file_mtime(&path).unwrap_or(sum.last_ms);
            let told = self.parents.told(&path, meta);
            out.push(Agent {
                id,
                kind,
                desc,
                parent,
                group,
                start_ms: if sum.start_ms == 0 { mtime } else { sum.start_ms },
                last_ms: sum.last_ms.max(if sum.start_ms == 0 { mtime } else { 0 }),
                status: sum.status(now, mtime, parent_blocked, can_ask, told.as_ref()),
                tools: sum.tools,
                errors: sum.errors,
                activity: sum.current(),
                cwd: sum.cwd.clone(),
                path,
            });
        }
        self.sums.retain(|p, _| seen.contains(p));
        mark_retried(&mut out);
        out
    }

    /// `workflowName` from `<session>/workflows/<run>.json`, read once per run.
    pub fn workflow_name(&mut self, session_dir: &Path, run: &str) -> String {
        self.workflow_names
            .entry(run.to_string())
            .or_insert_with(|| {
                let p = session_dir.join("workflows").join(format!("{run}.json"));
                let v: Value = std::fs::read(p).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
                v["workflowName"].as_str().unwrap_or(run).to_string()
            })
            .clone()
    }
}

fn collect_metas(dir: &Path, group: Option<String>, out: &mut Vec<(PathBuf, Option<String>)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if p.is_dir() {
            // `workflows/` itself holds one directory per run; the run id is the group.
            let g = if name == "workflows" { None } else { Some(name) };
            collect_metas(&p, g.or_else(|| group.clone()), out);
        } else if name.starts_with("agent-") && name.ends_with(".meta.json") {
            out.push((p, group.clone()));
        }
    }
}

/// Feeds complete lines appended since `acc.offset`; a partial last line waits for the next scan.
fn advance(acc: &mut Summary, path: &Path) {
    let Ok(mut f) = File::open(path) else { return };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len < acc.offset {
        *acc = Summary::default();
    }
    if len == acc.offset || f.seek(SeekFrom::Start(acc.offset)).is_err() {
        return;
    }
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return;
    }
    let Some(end) = buf.iter().rposition(|&b| b == b'\n') else { return };
    for line in buf[..end].split(|&b| b == b'\n') {
        if let Ok(v) = serde_json::from_slice::<Value>(line) {
            acc.feed(&v);
        }
    }
    acc.offset += end as u64 + 1;
}

/// Reads every record of a transcript (for the detail view).
pub fn read_records(path: &Path) -> Vec<Value> {
    let Ok(bytes) = std::fs::read(path) else { return Vec::new() };
    bytes.split(|&b| b == b'\n').filter_map(|l| serde_json::from_slice(l).ok()).collect()
}

pub fn config_dir() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".claude"))
}

/// `<config>/projects/*/<session>` that holds `subagents/`. The same id can also appear under
/// another project (a workflow run from a different cwd leaves only `workflows/` there).
pub fn session_dir(session: &str) -> Option<PathBuf> {
    std::fs::read_dir(config_dir().join("projects")).ok()?.flatten().map(|e| e.path().join(session)).find(|p| p.join("subagents").is_dir())
}

/// Display name and process start of the Claude running a session, from
/// `<config>/sessions/<pid>.json`. A resumed session keeps its id, so its transcript also holds
/// subagents from earlier runs; `started` tells them apart. The newest run wins when an exited
/// one left its file behind. That directory only grows, so each file is parsed again only when
/// its mtime changes.
#[derive(Default)]
pub struct Sessions {
    /// path -> (mtime, (session id, name, startedAt) if the file parsed)
    files: HashMap<PathBuf, (SystemTime, Option<(String, String, i64)>)>,
}

impl Sessions {
    pub fn info(&mut self, session: &str) -> Option<(String, i64)> {
        let mut seen = HashSet::new();
        let mut best: Option<(String, i64)> = None;
        for e in std::fs::read_dir(config_dir().join("sessions")).ok()?.flatten() {
            let p = e.path();
            if p.extension().is_none_or(|x| x != "json") {
                continue;
            }
            let Ok(mtime) = e.metadata().and_then(|m| m.modified()) else { continue };
            seen.insert(p.clone());
            let entry = self.files.entry(p.clone()).or_insert((UNIX_EPOCH, None));
            if entry.0 != mtime {
                *entry = (mtime, parse_session_file(&p));
            }
            if let Some((sid, name, started)) = &entry.1 {
                // `>=`: on equal starts the later file wins, like max_by_key did.
                if sid == session && best.as_ref().is_none_or(|(_, b)| *started >= *b) {
                    best = Some((name.clone(), *started));
                }
            }
        }
        self.files.retain(|p, _| seen.contains(p));
        best
    }
}

fn parse_session_file(p: &Path) -> Option<(String, String, i64)> {
    let v: Value = serde_json::from_slice(&std::fs::read(p).ok()?).ok()?;
    Some((v["sessionId"].as_str()?.to_string(), v["name"].as_str().unwrap_or("").to_string(), v["startedAt"].as_i64().unwrap_or(0)))
}

/// One line that says what a tool call is about: its command, path, pattern, …
pub fn tool_summary(input: &Value, cwd: &str) -> String {
    const KEYS: [&str; 10] =
        ["command", "file_path", "pattern", "path", "url", "query", "skill", "description", "prompt", "subject"];
    let s = KEYS
        .iter()
        .find_map(|k| input[*k].as_str())
        .or_else(|| input.as_object()?.values().find_map(Value::as_str))
        .unwrap_or("");
    let s = s.trim().lines().next().unwrap_or("");
    let prefix = format!("{}/", cwd.trim_end_matches('/'));
    if cwd.is_empty() { s.to_string() } else { s.replace(&prefix, "") }
}

pub fn now_ms() -> i64 {
    sys_ms(SystemTime::now())
}

pub fn file_mtime(p: &Path) -> Option<i64> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok().map(sys_ms)
}

fn sys_ms(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// `2026-10-02T10:21:53.361Z` → epoch ms (UTC only, which is what Claude Code writes).
pub fn parse_ts(s: &str) -> Option<i64> {
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (hh, mm, ss) = (n(11..13)?, n(14..16)?, n(17..19)?);
    let frac = s.get(19..).and_then(|r| r.strip_prefix('.')).map(|r| {
        let digits: String = r.chars().take_while(char::is_ascii_digit).take(3).collect();
        format!("{digits:0<3}").parse::<i64>().unwrap_or(0)
    });
    // Days from civil (Howard Hinnant).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(((days * 24 + hh) * 60 + mm) * 60_000 + ss * 1000 + frac.unwrap_or(0))
}

pub fn fmt_dur(ms: i64) -> String {
    let s = ms / 1000;
    match s {
        _ if s < 60 => format!("{s}s"),
        _ if s < 3600 => format!("{}m{:02}s", s / 60, s % 60),
        _ => format!("{}h{:02}m", s / 3600, s % 3600 / 60),
    }
}

pub fn fmt_tokens(n: u64) -> String {
    if n >= 1000 { format!("{:.1}k", n as f64 / 1000.0) } else { n.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_claude_timestamps() {
        assert_eq!(parse_ts("1970-01-01T00:00:01.5Z"), Some(1500));
        assert_eq!(parse_ts("2026-10-02T10:21:53.361Z"), Some(1_790_936_513_361));
    }

    fn ts(sec: i64) -> String {
        format!("2026-10-02T10:{:02}:{:02}.000Z", sec / 60, sec % 60)
    }
    fn assistant(sec: i64, content: Value, stop: Option<&str>) -> Value {
        serde_json::json!({"type": "assistant", "timestamp": ts(sec), "message": {"content": content, "stop_reason": stop}})
    }
    fn user(sec: i64, content: Value) -> Value {
        serde_json::json!({"type": "user", "timestamp": ts(sec), "message": {"content": content}})
    }
    fn tool_use(id: &str, name: &str) -> Value {
        serde_json::json!([{"type": "tool_use", "id": id, "name": name, "input": {"command": "make test"}}])
    }
    fn result(id: &str, err: bool) -> Value {
        serde_json::json!([{"type": "tool_result", "tool_use_id": id, "content": "ok", "is_error": err}])
    }
    fn run(records: &[Value]) -> Summary {
        let mut s = Summary::default();
        records.iter().for_each(|r| s.feed(r));
        s
    }
    /// Status one second after the last record, file written then too.
    fn status(s: &Summary, blocked: bool, can_ask: bool) -> Status {
        s.status(s.last_ms + 1000, s.last_ms, blocked, can_ask, None)
    }

    #[test]
    fn prompt_only_is_starting() {
        let s = run(&[user(0, "do it".into())]);
        assert!(status(&s, false, true) == Status::Starting);
    }

    #[test]
    fn open_tool_call_is_running_that_tool_until_its_result() {
        let s = run(&[user(0, "go".into()), assistant(5, tool_use("t1", "Bash"), Some("tool_use"))]);
        assert!(matches!(status(&s, false, true), Status::Tool { ref name, .. } if name == "Bash"));
        let s = run(&[user(0, "go".into()), assistant(5, tool_use("t1", "Bash"), None), user(9, result("t1", false))]);
        assert!(status(&s, false, true) == Status::Thinking);
    }

    #[test]
    fn open_agent_call_is_waiting_on_children() {
        let s = run(&[user(0, "go".into()), assistant(5, tool_use("t1", "Agent"), None)]);
        assert!(status(&s, false, true) == Status::Waiting { agents: 1, tasks: 0 });
    }

    #[test]
    fn ended_turn_with_unreported_background_work_is_waiting_not_done() {
        let txt = serde_json::json!([{"type": "text", "text": "Waiting for background task to complete..."}]);
        let mut launched = user(6, result("t1", false));
        launched["toolUseResult"] = serde_json::json!({"backgroundTaskId": "bq1"});
        let mut spawned = user(7, result("t2", false));
        spawned["toolUseResult"] = serde_json::json!({"status": "async_launched", "agentId": "a9"});
        let base = vec![
            user(0, "go".into()),
            assistant(5, tool_use("t1", "Bash"), None),
            launched,
            assistant(6, tool_use("t2", "Agent"), None),
            spawned,
            assistant(8, txt.clone(), Some("end_turn")),
        ];
        assert!(status(&run(&base), false, true) == Status::Waiting { agents: 1, tasks: 1 });
        let note = |id: &str, sec| user(sec, format!("<task-notification>\n<task-id>{id}</task-id>\n<status>completed</status>").into());
        let mut all = base.clone();
        all.extend([note("bq1", 40), note("a9", 41), assistant(42, txt, Some("end_turn"))]);
        assert!(status(&run(&all), false, true) == Status::Done);
    }

    #[test]
    fn blocked_parent_means_approval_only_for_foreground_agents_with_an_open_call() {
        let s = run(&[user(0, "go".into()), assistant(5, tool_use("t1", "Bash"), None)]);
        assert!(matches!(status(&s, true, true), Status::Approval { .. }));
        // Background agents cannot ask (permission prompts are auto-denied), so not theirs.
        assert!(matches!(status(&s, true, false), Status::Tool { .. }));
        let thinking = run(&[user(0, "go".into())]);
        assert!(status(&thinking, true, true) == Status::Starting);
    }

    #[test]
    fn end_turn_is_done_and_reported_status_lines_are_read() {
        let txt = |t: &str| serde_json::json!([{"type": "text", "text": t}]);
        let done = run(&[user(0, "go".into()), assistant(5, txt("All good."), Some("end_turn"))]);
        assert!(status(&done, false, true) == Status::Done);
        let blocked = run(&[user(0, "go".into()), assistant(5, txt("Report\n**Status:** BLOCKED — no DB"), Some("end_turn"))]);
        assert!(status(&blocked, false, true) == Status::Reported("BLOCKED".into()));
        let concerns = run(&[user(0, "go".into()), assistant(5, txt("**Status: DONE_WITH_CONCERNS.** x"), Some("end_turn"))]);
        assert!(status(&concerns, false, true) == Status::Concerns);
        // A word in prose is not a status line.
        let prose = run(&[user(0, "go".into()), assistant(5, txt("Stop early only for a BLOCKED condition"), Some("end_turn"))]);
        assert!(status(&prose, false, true) == Status::Done);
    }

    #[test]
    fn parent_hearing_the_agent_finish_counts_as_done_without_end_turn() {
        let txt = serde_json::json!([{"type": "text", "text": "**Status:** BLOCKED — x"}]);
        // The last record is the text block with stop_reason null: no end_turn was written.
        let s = run(&[user(0, "go".into()), assistant(5, txt, None)]);
        let at = |sec: i64, status: &str| Told { ts: s.start_ms + sec * 1000, status: status.into() };
        let now = s.last_ms + 1000;
        assert!(s.status(now, s.last_ms, false, true, None) == Status::Thinking);
        assert!(s.status(now, s.last_ms, false, true, Some(&at(6, "completed"))) == Status::Reported("BLOCKED".into()));
        assert!(s.status(now, s.last_ms, false, true, Some(&at(6, "killed"))) == Status::Interrupted);
        // A notice older than the agent's latest record is about an earlier run (resumed since).
        assert!(s.status(now, s.last_ms, false, true, Some(&at(1, "completed"))) == Status::Thinking);
    }

    #[test]
    fn parent_log_reads_notifications_and_skips_async_launch_results() {
        let dir = std::env::temp_dir().join(format!("agents-test-{}", std::process::id()));
        let subs = dir.join("sess").join("subagents");
        std::fs::create_dir_all(&subs).unwrap();
        let note = serde_json::json!({"type": "attachment", "timestamp": ts(30), "attachment": {"type": "queued_command",
            "prompt": "<task-notification>\n<task-id>abg</task-id>\n<status>completed</status>"}});
        let launch = serde_json::json!({"type": "user", "timestamp": ts(2), "toolUseResult": {"status": "async_launched", "agentId": "abg"},
            "message": {"content": [{"type": "tool_result", "tool_use_id": "tu_bg", "content": "launched"}]}});
        let fg = serde_json::json!({"type": "user", "timestamp": ts(40),
            "message": {"content": [{"type": "tool_result", "tool_use_id": "tu_fg", "content": "report"}]}});
        let main = [launch, note, fg].iter().map(Value::to_string).collect::<Vec<_>>().join("\n") + "\n";
        std::fs::write(dir.join("sess.jsonl"), main).unwrap();
        let mut p = Parents::default();
        let bg = p.told(&subs.join("agent-abg.jsonl"), &serde_json::json!({"toolUseId": "tu_bg"})).unwrap();
        assert_eq!((bg.status.as_str(), bg.ts), ("completed", parse_ts(&ts(30)).unwrap()));
        assert!(p.told(&subs.join("agent-afg.jsonl"), &serde_json::json!({"toolUseId": "tu_fg"})).is_some());
        // The launch result of a background agent is not its completion.
        assert!(p.told(&subs.join("agent-axx.jsonl"), &serde_json::json!({"toolUseId": "tu_bg"})).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn parents_reads_a_shared_parent_once_per_scan_but_every_call_without_scans() {
        let dir = std::env::temp_dir().join(format!("agents-scan-test-{}", std::process::id()));
        let subs = dir.join("sess").join("subagents");
        std::fs::create_dir_all(&subs).unwrap();
        let main = dir.join("sess.jsonl");
        let note = |id: &str| {
            serde_json::json!({"type": "attachment", "timestamp": ts(30), "attachment": {"type": "queued_command",
                "prompt": format!("<task-notification>\n<task-id>{id}</task-id>\n<status>completed</status>")}})
            .to_string()
                + "\n"
        };
        std::fs::write(&main, note("a1")).unwrap();
        let meta = serde_json::json!({});
        let mut p = Parents::default();
        p.new_scan();
        assert!(p.told(&subs.join("agent-a1.jsonl"), &meta).is_some());
        // Appended within the same scan: not read until the next one.
        std::fs::write(&main, note("a1") + &note("a2")).unwrap();
        assert!(p.told(&subs.join("agent-a2.jsonl"), &meta).is_none());
        p.new_scan();
        assert!(p.told(&subs.join("agent-a2.jsonl"), &meta).is_some());
        // Without scans (the transcript view) every call reads what was appended.
        let mut view = Parents::default();
        assert!(view.told(&subs.join("agent-a2.jsonl"), &meta).is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sessions_pick_the_newest_run_and_reparse_only_changed_files() {
        let dir = std::env::temp_dir().join(format!("agents-sessions-test-{}", std::process::id()));
        let sessions = dir.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let write = |file: &str, name: &str, started: i64, mtime_s: u64| {
            let p = sessions.join(file);
            std::fs::write(&p, serde_json::json!({"sessionId": "s1", "name": name, "startedAt": started}).to_string()).unwrap();
            let t = UNIX_EPOCH + std::time::Duration::from_secs(mtime_s);
            File::options().write(true).open(&p).unwrap().set_modified(t).unwrap();
        };
        write("1.json", "old-run", 100, 1_000);
        write("2.json", "new-run", 200, 1_000);
        // Only test reading CLAUDE_CONFIG_DIR, so setting it here races nothing.
        std::env::set_var("CLAUDE_CONFIG_DIR", &dir);
        let mut s = Sessions::default();
        assert_eq!(s.info("s1"), Some(("new-run".into(), 200)));
        // Same mtime: the cached parse is used even though the content changed.
        write("2.json", "renamed", 200, 1_000);
        assert_eq!(s.info("s1"), Some(("new-run".into(), 200)));
        // A new mtime: read again.
        write("2.json", "renamed", 200, 2_000);
        assert_eq!(s.info("s1"), Some(("renamed".into(), 200)));
        assert_eq!(s.info("other"), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn workflow_agent_is_done_once_structured_output_is_accepted() {
        let s = run(&[user(0, "go".into()), assistant(5, tool_use("t1", STRUCTURED_OUTPUT), Some("tool_use"))]);
        assert!(matches!(status(&s, false, true), Status::Tool { .. }));
        let rejected = run(&[user(0, "go".into()), assistant(5, tool_use("t1", STRUCTURED_OUTPUT), None), user(6, result("t1", true))]);
        assert!(status(&rejected, false, true) == Status::Thinking);
        let accepted = run(&[user(0, "go".into()), assistant(5, tool_use("t1", STRUCTURED_OUTPUT), None), user(6, result("t1", false))]);
        assert!(status(&accepted, false, true) == Status::Done);
    }

    #[test]
    fn api_error_is_failed_and_interrupt_is_interrupted() {
        let mut err = assistant(5, serde_json::json!([{"type": "text", "text": "limit"}]), Some("stop_sequence"));
        err["isApiErrorMessage"] = true.into();
        err["error"] = "rate_limit".into();
        let s = run(&[user(0, "go".into()), err]);
        assert!(status(&s, false, true) == Status::Failed("rate_limit".into()));
        let s = run(&[user(0, "go".into()), assistant(5, tool_use("t1", "Bash"), None), user(6, "[Request interrupted by user]".into())]);
        assert!(status(&s, false, true) == Status::Interrupted);
    }

    #[test]
    fn silence_means_stopped_with_more_patience_for_open_tools() {
        let thinking = run(&[user(0, "go".into())]);
        assert!(thinking.status(thinking.last_ms + QUIET_MS + 1, 0, false, true, None) == Status::Stopped);
        let tool = run(&[user(0, "go".into()), assistant(5, tool_use("t1", "Bash"), None)]);
        assert!(matches!(tool.status(tool.last_ms + QUIET_MS + 1, 0, false, true, None), Status::Tool { .. }));
        assert!(tool.status(tool.last_ms + TOOL_QUIET_MS + 1, 0, false, true, None) == Status::Stopped);
    }

    fn agent(desc: &str, kind: &str, start: i64, status: Status) -> Agent {
        Agent {
            id: format!("{desc}{start}"),
            path: PathBuf::new(),
            kind: kind.into(),
            desc: desc.into(),
            parent: None,
            group: None,
            start_ms: start,
            last_ms: start,
            status,
            tools: 0,
            errors: 0,
            activity: String::new(),
            cwd: String::new(),
        }
    }

    #[test]
    fn failure_redispatched_with_same_task_is_retried_not_needing_the_user() {
        let failed = || Status::Failed("rate_limit".into());
        let mut a = vec![
            agent("Task 1.1 backend", "coding", 100, failed()),
            agent("Task 1.1 backend", "coding", 200, Status::Done),
            // Same description but a later agent of another type is a different job.
            agent("Review", "reviewer", 100, Status::Reported("BLOCKED".into())),
            agent("Review", "coding", 200, Status::Done),
            // The newest attempt failing still needs the user.
            agent("Task 1.2", "coding", 100, Status::Done),
            agent("Task 1.2", "coding", 200, failed()),
        ];
        mark_retried(&mut a);
        assert!(a[0].status == Status::Retried);
        assert!(a[2].status == Status::Reported("BLOCKED".into()));
        assert!(a[5].status == failed());
    }

    #[test]
    fn summary_strips_cwd() {
        let v: Value = serde_json::json!({"file_path": "/repo/apps/web/x.ts"});
        assert_eq!(tool_summary(&v, "/repo"), "apps/web/x.ts");
    }
}
