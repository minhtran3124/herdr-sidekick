//! Shared by the changes and agents panes: herdr CLI calls, the draw/event loop, and
//! `--snapshot WxH` (render one frame as text, for checking layout without a pane).

use std::io::stdout;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event};
use ratatui::crossterm::execute;
use ratatui::text::{Line, Span};
use ratatui::{Frame, Terminal};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub trait Screen {
    fn tick(&mut self);
    fn render(&mut self, f: &mut Frame);
    /// Returns false to quit.
    fn event(&mut self, e: Event) -> bool;
    /// True when the screen wants to close on its own (checked after every tick).
    fn done(&self) -> bool {
        false
    }
}

/// Side panels and overlays, by pane label: never split, followed, or counted as work panes.
/// ("Diff" and "Agent" are the manifest titles of the overlays.)
pub const SIDE_LABELS: [&str; 5] = [crate::worktrees::LABEL, crate::changes::LABEL, crate::agents::LABEL, "Diff", "Agent"];

pub fn plugin_id() -> String {
    std::env::var("HERDR_PLUGIN_ID").unwrap_or_else(|_| crate::PLUGIN_ID.into())
}

pub fn state_file(name: &str) -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("HERDR_PLUGIN_STATE_DIR")?).join(name))
}

/// Every pane herdr knows (`pane list`). None outside herdr or when herdr cannot be asked.
pub fn pane_list() -> Option<Vec<serde_json::Value>> {
    let bin = std::env::var("HERDR_BIN_PATH").ok()?;
    let out = Command::new(bin).args(["pane", "list"]).stderr(Stdio::null()).output().ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    v["result"]["panes"].as_array().cloned()
}

/// `tab_id` of pane `me` in `panes`.
pub fn tab_of<'a>(panes: &'a [serde_json::Value], me: &str) -> Option<&'a serde_json::Value> {
    Some(&panes.iter().find(|p| p["pane_id"] == me)?["tab_id"])
}

/// Closes a plugin pane. Re-linking the plugin drops herdr's ownership record, after which only
/// a plain `pane close` works.
pub fn close_pane(bin: &str, id: &str) {
    let closed = Command::new(bin).args(["plugin", "pane", "close", id]).output().is_ok_and(|o| o.status.success());
    if !closed {
        let _ = Command::new(bin).args(["pane", "close", id]).output();
    }
}

/// A panel's `q`: close it in every tab and stop auto-opening it until its toggle action. Done
/// in the pane, not via `sidekick.sh off`, because that script would close this pane while it
/// is still running.
pub fn hide_everywhere(label: &str, panel: &str) {
    if let Some(flag) = state_file(&format!("disabled-{panel}")) {
        let _ = std::fs::write(flag, "");
    }
    let (Ok(bin), Ok(me)) = (std::env::var("HERDR_BIN_PATH"), std::env::var("HERDR_PANE_ID")) else { return };
    let panes = pane_list().unwrap_or_default();
    let others = panes.iter().filter(|p| p["label"] == label && p["pane_id"] != me.as_str());
    for id in others.filter_map(|p| p["pane_id"].as_str()) {
        close_pane(&bin, id);
    }
}

/// Opens one of this plugin's overlay panes (it takes focus), without waiting for it.
pub fn open_overlay(entry: &str, cwd: Option<&Path>, env: Option<String>) {
    let Ok(bin) = std::env::var("HERDR_BIN_PATH") else { return };
    let mut cmd = Command::new(bin);
    cmd.args(["plugin", "pane", "open", "--plugin", &plugin_id(), "--entrypoint", entry, "--placement", "overlay", "--focus"]);
    if let Some(cwd) = cwd {
        cmd.arg("--cwd").arg(cwd);
    }
    if let Some(env) = env {
        cmd.args(["--env", &env]);
    }
    if let Ok(mut child) = cmd.stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
        std::thread::spawn(move || child.wait());
    }
}

/// One file save is several events: let them settle into one scan.
const SETTLE: Duration = Duration::from_millis(250);
/// A file written non-stop (an agent streaming its transcript) rescans once a second, not on
/// every settle.
const MIN_SCAN_GAP: Duration = Duration::from_secs(1);

/// A worker's wait between ticks. Some(false): `tick` passed with no kick. Some(true): kicked (a
/// file event or `r`); the burst has settled and MIN_SCAN_GAP since `last_scan` has passed.
/// None: the panel is gone.
pub fn wait_kick(kicks: &Receiver<()>, tick: Duration, last_scan: Option<Instant>) -> Option<bool> {
    match kicks.recv_timeout(tick) {
        Ok(()) => {
            let gap = last_scan.map_or(Duration::ZERO, |t| MIN_SCAN_GAP.saturating_sub(t.elapsed()));
            std::thread::sleep(SETTLE.max(gap));
            kicks.try_iter().for_each(drop);
            Some(true)
        }
        Err(RecvTimeoutError::Timeout) => Some(false),
        Err(RecvTimeoutError::Disconnected) => None,
    }
}

/// Runs a herdr CLI call against this pane (`$PANE` = HERDR_PANE_ID); a no-op outside herdr.
pub fn herdr(args: &[&str]) {
    let (Ok(bin), Ok(pane)) = (std::env::var("HERDR_BIN_PATH"), std::env::var("HERDR_PANE_ID")) else { return };
    let args: Vec<&str> = args.iter().map(|a| if *a == "$PANE" { pane.as_str() } else { a }).collect();
    let _ = std::process::Command::new(bin).args(args).output();
}

/// herdr 0.9.1 can spawn a split plugin pane with a stale PTY size (e.g. 131 cols in a 39-col
/// pane), which pushes right-aligned text off screen. A no-op resize resyncs it.
pub fn resync_size() {
    herdr(&["pane", "resize", "--pane", "$PANE", "--direction", "down", "--amount", "0"]);
}

/// Removes `--snapshot WxH` from `args` and returns the size, if given.
pub fn take_snapshot_arg(args: &mut Vec<String>) -> Option<String> {
    let i = args.iter().position(|a| a == "--snapshot")?;
    let size = args.get(i + 1).cloned().unwrap_or_default();
    args.drain(i..(i + 2).min(args.len()));
    Some(size)
}

pub fn run(screen: &mut dyn Screen, poll: Duration) -> std::io::Result<()> {
    let mut term = ratatui::init();
    execute!(stdout(), EnableMouseCapture)?;
    let result = (|| loop {
        screen.tick();
        if screen.done() {
            return Ok(());
        }
        term.draw(|f| screen.render(f))?;
        if event::poll(poll)? && !screen.event(event::read()?) {
            return Ok(());
        }
    })();
    execute!(stdout(), DisableMouseCapture)?;
    ratatui::restore();
    result
}

pub fn print_snapshot(screen: &mut dyn Screen, size: &str, default: (u16, u16)) -> std::io::Result<()> {
    let (w, h) = size.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))).unwrap_or(default);
    screen.tick();
    // TestBackend never fails (its error type is Infallible).
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| screen.render(f)).unwrap();
    let buf = term.backend().buffer();
    for y in 0..h {
        let row: String = (0..w).map(|x| buf[(x, y)].symbol()).collect();
        println!("{}", row.trim_end());
    }
    Ok(())
}

/// Cuts `s` to `max` display columns, ending in `…` when cut (CJK and emoji count as two).
pub fn truncate(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if used + cw + 1 > max {
            break;
        }
        used += cw;
        out.push(c);
    }
    out.push('…');
    out
}

/// Left spans, padding, right spans: right-aligns a stats column.
pub fn spread(mut left: Vec<Span<'static>>, right: Vec<Span<'static>>, w: usize) -> Line<'static> {
    let used: usize = left.iter().chain(&right).map(|s| s.width()).sum();
    left.push(" ".repeat(w.saturating_sub(used)).into());
    left.extend(right);
    Line::from(left)
}
