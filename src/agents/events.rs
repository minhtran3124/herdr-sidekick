//! Live feed of herdr's `pane.agent_status_changed` events for the notifier, so it scans when a
//! Claude pane changes state instead of on a timer.
//!
//! herdr only accepts that subscription per pane, so the feed subscribes to every Claude pane plus
//! the pane lifecycle events, and starts over when a pane appears, goes or turns out to run an
//! agent. Events are invalidation signals (a lagging reader gets `events_lost`, nothing is
//! replayed): any line that ends the stream makes the next pass resubscribe and report a change.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

const RETRY: Duration = Duration::from_secs(5);
const STATUS: &str = "pane.agent_status_changed";
const LIFECYCLE: [&str; 3] = ["pane.created", "pane.closed", "pane.agent_detected"];

pub struct Feed {
    changed: Arc<AtomicBool>,
    live: Arc<AtomicBool>,
}

impl Feed {
    /// None outside herdr. The thread ends with the process.
    pub fn start() -> Option<Feed> {
        let socket = std::env::var_os("HERDR_SOCKET_PATH")?;
        let feed = Feed { changed: Arc::default(), live: Arc::default() };
        let (changed, live) = (feed.changed.clone(), feed.live.clone());
        std::thread::spawn(move || loop {
            follow(&socket, &changed, &live);
            live.store(false, Ordering::Relaxed);
            std::thread::sleep(RETRY);
        });
        Some(feed)
    }

    /// A Claude pane changed state (or the feed just connected) since the last call.
    pub fn take(&self) -> bool {
        self.changed.swap(false, Ordering::Relaxed)
    }

    /// Subscribed right now; otherwise the caller has to poll.
    pub fn live(&self) -> bool {
        self.live.load(Ordering::Relaxed)
    }
}

fn follow(socket: &OsString, changed: &AtomicBool, live: &AtomicBool) -> Option<()> {
    let mut subs: Vec<Value> = crate::tui::pane_list()?
        .iter()
        .filter(|p| p["agent"] == "claude" || p["agent_session"]["agent"] == "claude")
        .filter_map(|p| p["pane_id"].as_str())
        .map(|id| json!({"type": STATUS, "pane_id": id}))
        .collect();
    subs.extend(LIFECYCLE.iter().map(|t| json!({"type": t})));
    let mut stream = UnixStream::connect(socket).ok()?;
    let req = json!({"id": "sidekick", "method": "events.subscribe", "params": {"subscriptions": subs}});
    writeln!(stream, "{req}").ok()?;
    let mut lines = BufReader::new(stream).lines();
    let ack: Value = serde_json::from_str(&lines.next()?.ok()?).ok()?;
    // A pane closed since `pane list` rejects the whole request: the retry lists panes again.
    if ack["result"]["type"] != "subscription_started" {
        return None;
    }
    live.store(true, Ordering::Relaxed);
    // Nothing is replayed, so whatever changed before this point is unseen: scan once.
    changed.store(true, Ordering::Relaxed);
    for line in lines {
        let Ok(v) = serde_json::from_str::<Value>(&line.ok()?) else { continue };
        changed.store(true, Ordering::Relaxed);
        if v["event"] != STATUS {
            return None;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs a running herdr: `HERDR_BIN_PATH=$(which herdr) cargo test -- --ignored feed_connects`.
    #[test]
    #[ignore]
    fn feed_connects_and_reports_the_first_scan() {
        let feed = Feed::start().expect("HERDR_SOCKET_PATH");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !feed.live() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(feed.live(), "no subscription_started from herdr");
        assert!(feed.take());
        assert!(!feed.take());
    }
}
