//! Agents: Claude Code subagents of the focused Claude pane, as a list, live transcript panes
//! and notifications when one needs the user.
//!   sidekick agents               subagent list (`agents` pane)
//!   sidekick view [FILE]          live transcript; FILE defaults to $AGENT_FILE (`view` pane)
//!   sidekick notify               notify about subagents needing the user, every Claude pane (hooks)
//! Outside herdr, set AGENTS_SESSION=<session id> to pick the session for the list.

mod data;
mod list;
mod notify;
mod view;
mod watch;

use std::path::PathBuf;
use std::time::Duration;

use ratatui::crossterm::event::Event;
use ratatui::Frame;

use crate::tui::{self, Screen};

/// sidekick.sh finds the panel by this pane label (herdr has no "list plugin panes" API).
pub const LABEL: &str = "◈ agents";

impl Screen for list::List {
    fn tick(&mut self) {
        list::List::tick(self)
    }
    fn render(&mut self, f: &mut Frame) {
        list::List::render(self, f)
    }
    fn event(&mut self, e: Event) -> bool {
        list::List::event(self, e)
    }
}

impl Screen for view::View {
    fn tick(&mut self) {
        view::View::tick(self)
    }
    fn render(&mut self, f: &mut Frame) {
        view::View::render(self, f)
    }
    fn event(&mut self, e: Event) -> bool {
        view::View::event(self, e)
    }
    fn done(&self) -> bool {
        view::View::done(self)
    }
}

// 100ms keeps the running spinner moving; drawing an unchanged frame is cheap.
const POLL: Duration = Duration::from_millis(100);

pub fn list_main(mut args: Vec<String>) -> std::io::Result<()> {
    let snapshot = tui::take_snapshot_arg(&mut args);
    let mut l = list::List::new();
    if snapshot.is_some() {
        l.wait_loaded();
    } else {
        tui::herdr(&["pane", "rename", "$PANE", LABEL]);
        tui::resync_size();
    }
    match snapshot {
        Some(size) => tui::print_snapshot(&mut l, &size, (44, 30)),
        None => tui::run(&mut l, POLL),
    }
}

pub fn view_main(mut args: Vec<String>) -> std::io::Result<()> {
    let snapshot = tui::take_snapshot_arg(&mut args);
    let Some(path) = args.first().cloned().or_else(|| std::env::var("AGENT_FILE").ok()) else {
        eprintln!("sidekick view: no transcript (pass FILE or set AGENT_FILE)");
        std::process::exit(1);
    };
    let mut v = view::View::new(PathBuf::from(path));
    if snapshot.is_none() {
        // Only split panes are renamed: an overlay may carry the id of the pane underneath.
        if std::env::var("AGENT_PANE").as_deref() == Ok("split") {
            tui::herdr(&["pane", "rename", "$PANE", &v.pane_label()]);
        }
        tui::resync_size();
    }
    match snapshot {
        Some(size) => tui::print_snapshot(&mut v, &size, (100, 45)),
        None => tui::run(&mut v, POLL),
    }
}

pub fn notify_main() -> std::io::Result<()> {
    notify::run_once();
    Ok(())
}
