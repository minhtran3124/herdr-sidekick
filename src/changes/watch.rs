//! File-system watching for the changes panel, so `git status` runs when something changed
//! instead of every second.
//!
//! Linux (inotify) costs one watch per directory and a recursive watch would add every ignored
//! directory too (edgeful: 54k with node_modules and .venv), so it watches only the directories
//! git knows about (~800). macOS (FSEvents) watches the root recursively, one cheap stream.
//! Both also watch the git directory (index, HEAD: staging, commits, branch switches), which a
//! linked worktree keeps outside its checkout, in `<main>/.git/worktrees/<name>`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, RwLock};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use super::git;

/// Directory names whose contents never show up in the panel: build output and caches.
const NOISE: [&str; 9] = ["node_modules", "__pycache__", ".pytest_cache", ".mypy_cache", ".ruff_cache", ".next", ".turbo", ".venv", "target"];

pub struct Watch {
    watcher: RecommendedWatcher,
    /// Shared with the event callback, which judges paths relative to it.
    root: Arc<RwLock<Option<PathBuf>>>,
    /// Set by the callback when something was created or removed: the directories to watch may
    /// have changed, so the next sync lists them again (otherwise it skips `git ls-files`).
    relist: Arc<AtomicBool>,
    dirs: HashSet<PathBuf>,
}

impl Watch {
    /// File events send `()` on `wake`. None when the platform watcher cannot start; the
    /// caller then falls back to polling.
    pub fn new(wake: Sender<()>) -> Option<Self> {
        let root: Arc<RwLock<Option<PathBuf>>> = Arc::default();
        let relist = Arc::new(AtomicBool::new(true));
        let (cb_root, cb_relist) = (root.clone(), relist.clone());
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(ev) = res else {
                // Overflow or a dropped watch: rescan rather than miss a change.
                cb_relist.store(true, Ordering::Relaxed);
                let _ = wake.send(());
                return;
            };
            if matches!(ev.kind, EventKind::Access(_)) {
                return;
            }
            let root = cb_root.read().ok().and_then(|r| r.clone());
            if ev.paths.iter().any(|p| relevant(p, root.as_deref())) {
                if matches!(ev.kind, EventKind::Create(_) | EventKind::Remove(_)) {
                    cb_relist.store(true, Ordering::Relaxed);
                }
                let _ = wake.send(());
            }
        })
        .ok()?;
        Some(Watch { watcher, root, relist, dirs: HashSet::new() })
    }

    /// Follows checkout `root` (None: watch nothing). `full` (the safety scan) re-lists the
    /// directories even without a create/remove event.
    pub fn sync(&mut self, root: Option<&Path>, full: bool) {
        let changed = self.root.read().map(|r| r.as_deref() != root).unwrap_or(true);
        if changed {
            for d in self.dirs.drain() {
                let _ = self.watcher.unwatch(&d);
            }
            if let Ok(mut r) = self.root.write() {
                *r = root.map(Path::to_path_buf);
            }
        }
        let Some(root) = root else { return };
        if !(changed || full || self.relist.swap(false, Ordering::Relaxed)) {
            return;
        }
        let want = watch_set(root);
        for d in self.dirs.difference(&want).cloned().collect::<Vec<_>>() {
            let _ = self.watcher.unwatch(&d);
            self.dirs.remove(&d);
        }
        for d in want {
            let mode = if cfg!(target_os = "macos") && d == root { RecursiveMode::Recursive } else { RecursiveMode::NonRecursive };
            // A directory that vanished since `ls-files` just fails to watch.
            if !self.dirs.contains(&d) && self.watcher.watch(&d, mode).is_ok() {
                self.dirs.insert(d);
            }
        }
    }
}

/// The git directory, plus the root and (Linux) every directory holding a tracked or
/// untracked-but-not-ignored file, with its ancestors so a new subdirectory is seen too.
fn watch_set(root: &Path) -> HashSet<PathBuf> {
    let mut dirs = HashSet::from([root.to_path_buf()]);
    dirs.extend(git::git_dir(root));
    if cfg!(target_os = "macos") {
        return dirs;
    }
    for file in git::ls_files(root) {
        let mut p = Path::new(&file).parent();
        while let Some(d) = p.filter(|d| !d.as_os_str().is_empty()) {
            if !dirs.insert(root.join(d)) {
                break;
            }
            p = d.parent();
        }
    }
    dirs
}

/// Changes worth a rescan: worktree files outside build/cache directories, and inside a git
/// directory only the index, HEAD and refs (a worktree's gitdir is `.git/worktrees/<name>/`).
fn relevant(p: &Path, root: Option<&Path>) -> bool {
    let s = p.to_string_lossy();
    if s.contains("/.git/") {
        let name = p.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
        return !name.ends_with(".lock") && (name == "index" || name == "HEAD" || s.contains("/refs/"));
    }
    // Relative to the checkout: a repo that itself lives under, say, `target/` still counts.
    let rel = root.and_then(|r| p.strip_prefix(r).ok()).unwrap_or(p);
    !rel.components().any(|c| NOISE.contains(&c.as_os_str().to_string_lossy().as_ref()))
}

#[cfg(test)]
mod tests {
    use super::relevant;
    use std::path::Path;

    #[test]
    fn only_worktree_files_and_git_index_head_refs_trigger_a_rescan() {
        let root = Some(Path::new("/repo"));
        let yes = |p: &str| relevant(Path::new(p), root);
        assert!(yes("/repo/apps/web/page.tsx"));
        assert!(yes("/repo/.git/index"));
        assert!(yes("/repo/.git/HEAD"));
        assert!(yes("/repo/.git/refs/heads/main"));
        assert!(!yes("/repo/.git/index.lock"));
        assert!(!yes("/repo/.git/objects/ab/cdef"));
        assert!(!yes("/repo/.git/logs/HEAD.tmp"));
        assert!(!yes("/repo/apps/api/app/__pycache__/main.cpython-312.pyc"));
        assert!(!yes("/repo/apps/web/node_modules/x/index.js"));
        // Real code in directories that only sound like noise.
        assert!(yes("/repo/apps/api/app/logs/handler.py"));
    }

    #[test]
    fn a_linked_worktree_gitdir_counts_and_noise_is_judged_inside_the_repo() {
        let wt = Some(Path::new("/home/me/target/repo/.worktrees/fix"));
        assert!(relevant(Path::new("/home/me/target/repo/.git/worktrees/fix/index"), wt));
        assert!(relevant(Path::new("/home/me/target/repo/.git/worktrees/fix/HEAD"), wt));
        // The checkout sits under a directory named `target`; its files still count.
        assert!(relevant(Path::new("/home/me/target/repo/.worktrees/fix/src/main.rs"), wt));
    }
}
