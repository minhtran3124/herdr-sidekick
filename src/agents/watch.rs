//! File watching for the agents panel, so transcripts are rescanned when they change instead of
//! every second. Watches the session's `subagents/` (recursive: nested and workflow agents add
//! directories) and the session's own transcript `<sid>.jsonl`, where the parent records that a
//! background agent finished.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

pub struct Watch {
    watcher: RecommendedWatcher,
    watched: Vec<PathBuf>,
}

impl Watch {
    /// Changes send `()` on `wake`. None when the platform watcher cannot start; the caller
    /// then polls.
    pub fn new(wake: Sender<()>) -> Option<Self> {
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            // Errors (queue overflow, a dropped watch) wake too: rescan rather than miss a change.
            if !res.is_ok_and(|ev| matches!(ev.kind, EventKind::Access(_))) {
                let _ = wake.send(());
            }
        })
        .ok()?;
        Some(Watch { watcher, watched: Vec::new() })
    }

    /// Watches session directory `dir` (None: nothing).
    pub fn follow(&mut self, dir: Option<&Path>) {
        for p in self.watched.drain(..) {
            let _ = self.watcher.unwatch(&p);
        }
        let Some(dir) = dir else { return };
        let targets = [(dir.join("subagents"), RecursiveMode::Recursive), (dir.with_extension("jsonl"), RecursiveMode::NonRecursive)];
        for (p, mode) in targets {
            if self.watcher.watch(&p, mode).is_ok() {
                self.watched.push(p);
            }
        }
    }
}
