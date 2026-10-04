//! Changes: files changed vs HEAD in the focused pane's checkout, with a full-file diff.
//!   sidekick changes              changed-files panel (`changes` pane)
//!   sidekick diff [PATH]          full-file diff overlay; PATH defaults to $CHANGES_FILE (`diff` pane)

mod diff;
pub(crate) mod git;
mod list;
mod watch;

use std::time::Duration;

use ratatui::crossterm::event::Event;
use ratatui::Frame;

use crate::tui::{self, Screen};

/// sidekick.sh finds the panel by this pane label (herdr has no "list plugin panes" API).
pub const LABEL: &str = "± changes";

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

impl Screen for diff::Diff {
    fn tick(&mut self) {
        diff::Diff::tick(self)
    }
    fn render(&mut self, f: &mut Frame) {
        diff::Diff::render(self, f)
    }
    fn event(&mut self, e: Event) -> bool {
        diff::Diff::event(self, e)
    }
}

pub fn list_main(mut args: Vec<String>) -> std::io::Result<()> {
    let snapshot = tui::take_snapshot_arg(&mut args);
    let mut l = list::List::new(std::env::current_dir()?);
    if snapshot.is_some() {
        l.wait_loaded();
    } else {
        tui::herdr(&["pane", "rename", "$PANE", LABEL]);
        tui::resync_size();
    }
    match snapshot {
        Some(size) => tui::print_snapshot(&mut l, &size, (42, 30)),
        None => tui::run(&mut l, Duration::from_millis(250)),
    }
}

pub fn diff_main(mut args: Vec<String>) -> std::io::Result<()> {
    let snapshot = tui::take_snapshot_arg(&mut args);
    let path = args.first().cloned().or_else(|| std::env::var("CHANGES_FILE").ok()).unwrap_or_default();
    let Some(root) = git::toplevel(&std::env::current_dir()?) else {
        eprintln!("sidekick diff: not inside a git repository");
        std::process::exit(1);
    };
    let mut d = diff::Diff::new(root, path);
    if snapshot.is_none() {
        tui::resync_size();
    }
    match snapshot {
        Some(size) => tui::print_snapshot(&mut d, &size, (110, 45)),
        None => tui::run(&mut d, Duration::from_millis(250)),
    }
}
