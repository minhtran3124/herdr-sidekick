//! Git reads. Everything is compared against HEAD, so staged and unstaged edits show as one diff.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// `git hash-object -t tree /dev/null`: lets a repo with no commits diff against "nothing".
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Staged {
    No,
    Partly,
    All,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    /// Repo-relative, `/`-separated.
    pub path: String,
    /// `M` modified, `A` added, `D` deleted, `?` untracked, `U` conflicted.
    pub status: char,
    pub staged: Staged,
    pub ins: u32,
    pub del: u32,
    pub binary: bool,
}

/// Read-only git. GIT_OPTIONAL_LOCKS=0 stops `git status` from refreshing the index: that takes
/// .git/index.lock (racing the user's and agents' own git commands) and, with the file watcher,
/// would retrigger a scan on every scan.
fn git(root: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let out = Command::new("git").arg("-C").arg(root).args(args).env("GIT_OPTIONAL_LOCKS", "0").stderr(Stdio::null()).output().ok()?;
    out.status.success().then_some(out.stdout)
}

pub fn toplevel(dir: &Path) -> Option<PathBuf> {
    let out = git(dir, &["rev-parse", "--show-toplevel"])?;
    Some(PathBuf::from(String::from_utf8_lossy(&out).trim()))
}

pub fn branch(root: &Path) -> String {
    git(root, &["branch", "--show-current"]).map(|b| String::from_utf8_lossy(&b).trim().to_string()).unwrap_or_default()
}

fn base(root: &Path) -> &'static str {
    if git(root, &["rev-parse", "-q", "--verify", "HEAD"]).is_some() {
        "HEAD"
    } else {
        EMPTY_TREE
    }
}

/// HEAD's commit id; empty before the first commit.
pub fn head(root: &Path) -> String {
    git(root, &["rev-parse", "-q", "--verify", "HEAD"]).map(|b| String::from_utf8_lossy(&b).trim().to_string()).unwrap_or_default()
}

/// The file as committed in HEAD; empty when it is new.
pub fn head_blob(root: &Path, path: &str) -> Vec<u8> {
    git(root, &["show", &format!("HEAD:{path}")]).unwrap_or_default()
}

/// Tracked files plus untracked ones that are not ignored, repo-relative.
pub fn ls_files(root: &Path) -> Vec<String> {
    let out = git(root, &["ls-files", "-co", "--exclude-standard", "-z"]).unwrap_or_default();
    out.split(|&b| b == 0).filter(|p| !p.is_empty()).map(|p| String::from_utf8_lossy(p).into_owned()).collect()
}

/// The checkout's git directory: `root/.git`, or `<main>/.git/worktrees/<name>` in a linked
/// worktree (where `root/.git` is a file).
pub fn git_dir(root: &Path) -> Option<PathBuf> {
    let out = git(root, &["rev-parse", "--absolute-git-dir"])?;
    Some(PathBuf::from(String::from_utf8_lossy(&out).trim()))
}

pub fn is_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(8000)].contains(&0)
}

/// Renames are off on purpose: a rename shows as a delete plus an add, which keeps paths 1:1.
pub fn changes(root: &Path) -> Vec<Change> {
    let Some(status) = git(root, &["status", "--porcelain=v1", "-z", "--untracked-files=all", "--no-renames"]) else {
        return Vec::new();
    };
    let mut stats = HashMap::new();
    if let Some(numstat) = git(root, &["diff", base(root), "--numstat", "-z", "--no-renames"]) {
        for rec in numstat.split(|b| *b == 0) {
            let rec = String::from_utf8_lossy(rec);
            let mut it = rec.splitn(3, '\t');
            if let (Some(a), Some(d), Some(p)) = (it.next(), it.next(), it.next()) {
                stats.insert(p.to_string(), (a.parse().unwrap_or(0), d.parse().unwrap_or(0), a == "-"));
            }
        }
    }

    let mut out: Vec<Change> = status
        .split(|b| *b == 0)
        .filter(|rec| rec.len() > 3)
        .map(|rec| {
            let (x, y) = (rec[0] as char, rec[1] as char);
            let path = String::from_utf8_lossy(&rec[3..]).into_owned();
            let status = match (x, y) {
                ('?', _) => '?',
                ('U', _) | (_, 'U') | ('A', 'A') | ('D', 'D') => 'U',
                ('A', _) => 'A',
                ('D', _) | (_, 'D') => 'D',
                _ => 'M',
            };
            let staged = match (x, y) {
                ('?', _) | (' ', _) => Staged::No,
                (_, ' ') => Staged::All,
                _ => Staged::Partly,
            };
            let (ins, del, binary) =
                if status == '?' { count_lines(&root.join(&path)) } else { stats.get(&path).copied().unwrap_or_default() };
            Change { path, status, staged, ins, del, binary }
        })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// numstat does not cover untracked files: every line of one is an insertion.
fn count_lines(path: &Path) -> (u32, u32, bool) {
    let Ok(bytes) = std::fs::read(path) else { return (0, 0, false) };
    if is_binary(&bytes) {
        return (0, 0, true);
    }
    let n = bytes.iter().filter(|b| **b == b'\n').count() + usize::from(bytes.last().is_some_and(|b| *b != b'\n'));
    (n as u32, 0, false)
}
