//! herdr notifications when a subagent starts needing the user (approval, reported block, failure).
//!
//! Callers share one check: every open panel (each scan, its own session), one elected panel
//! (every Claude pane in every workspace, see `Notifier`), and `agents notify` from hooks. A
//! marker file per agent and kind, created with create_new, makes each notification fire once.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::data::{self, Agent, Scanner, Section, Status};

/// Only transcripts written this recently are read or reported; older trouble is history.
const RECENT_MS: i64 = 10 * 60 * 1000;
const KEEP_MARKERS_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const GLOBAL_EVERY: Duration = Duration::from_secs(5);
/// Markers live for days, so sweeping them once an hour is as good as every scan.
const PRUNE_EVERY: Duration = Duration::from_secs(60 * 60);

/// herdr fires pane.agent_status_changed too rarely to rely on (0.9.1), and plugins get no
/// daemon, so the open panels host the all-workspace scan: the one holding the lock runs it, and
/// when that panel closes the lock frees and another panel takes over.
#[derive(Default)]
pub struct Notifier {
    lock: Option<File>,
    last: Option<Instant>,
    pruned: Option<Instant>,
    scanners: HashMap<String, Scanner>,
    sessions: data::Sessions,
}

impl Notifier {
    pub fn tick(&mut self) {
        if self.last.is_some_and(|t| t.elapsed() < GLOBAL_EVERY) {
            return;
        }
        self.last = Some(Instant::now());
        if self.lock.is_none() {
            self.lock = take_lock();
        }
        if self.lock.is_some() {
            run_all(&mut self.scanners, &mut self.sessions);
            if self.pruned.is_none_or(|t| t.elapsed() >= PRUNE_EVERY) {
                self.pruned = Some(Instant::now());
                prune();
            }
        }
    }
}

fn take_lock() -> Option<File> {
    let path = crate::tui::state_file("notifier.lock")?;
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(path).ok()?;
    f.try_lock().ok()?;
    Some(f)
}

/// Notifies once for each agent of this session that needs the user and changed recently.
pub fn check(agents: &[Agent], session: &str, since: i64) {
    let Some(dir) = marker_dir() else { return };
    let now = data::now_ms();
    for a in agents {
        if a.status.section() != Section::Attention || a.start_ms < since || now - a.last_ms > RECENT_MS {
            continue;
        }
        let kind = match &a.status {
            Status::Approval { .. } => "approval",
            Status::Failed(_) => "failed",
            _ => "reported",
        };
        let marker = dir.join(format!("{}-{kind}", a.id));
        if std::fs::OpenOptions::new().write(true).create_new(true).open(&marker).is_err() {
            continue;
        }
        let title = if a.desc.is_empty() { a.kind.clone() } else { a.desc.clone() };
        let body = format!("{} · {session}", a.status.label(now));
        show(&format!("Agent: {title}"), &body);
    }
}

/// Checks every Claude session herdr knows about (`agents notify`, and the elected panel).
/// Scanners of sessions no longer in any pane are dropped.
pub fn run_all(scanners: &mut HashMap<String, Scanner>, sessions: &mut data::Sessions) {
    let Some(panes) = crate::tui::pane_list() else { return };
    let now = data::now_ms();
    let mut live = HashSet::new();
    for p in &panes {
        let Some(sid) = p["agent_session"]["value"].as_str() else { continue };
        if p["agent_session"]["agent"] != "claude" {
            continue;
        }
        live.insert(sid.to_string());
        let Some(dir) = data::session_dir(sid) else { continue };
        let (name, since) = sessions.info(sid).unwrap_or_default();
        let scanner = scanners.entry(sid.to_string()).or_default();
        let agents = scanner.scan(&dir, p["agent_status"] == "blocked", now - RECENT_MS);
        let label = if name.is_empty() { sid.chars().take(8).collect() } else { name };
        check(&agents, &label, since);
    }
    scanners.retain(|sid, _| live.contains(sid));
}

/// `agents notify` (hooks): one pass, then sweep old markers.
pub fn run_once() {
    run_all(&mut HashMap::new(), &mut data::Sessions::default());
    prune();
}

fn marker_dir() -> Option<PathBuf> {
    let dir = crate::tui::state_file("notified")?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn prune() {
    let Some(dir) = marker_dir() else { return };
    let now = data::now_ms();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if data::file_mtime(&e.path()).is_some_and(|t| now - t > KEEP_MARKERS_MS) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

fn show(title: &str, body: &str) {
    let Ok(bin) = std::env::var("HERDR_BIN_PATH") else { return };
    let _ = Command::new(bin)
        .args(["notification", "show", title, "--body", body, "--sound", "request"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}
