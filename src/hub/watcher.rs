//! Background discovery shared across processes. One process at a time
//! discovers agents and logs status updates; it publishes the agent list to the
//! database, and every other process (TUI or web server) reads that snapshot
//! instead, so they all show exactly the same agents.

use crate::agents::{self, Agent};
use crate::transcript::{self, Entry, Item};
use crate::db;
use rusqlite::Connection;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

const TICK: Duration = Duration::from_secs(2);
/// A snapshot from another process older than this means nobody is discovering.
const STALE_MS: i64 = 6_000;

pub fn spawn() -> Receiver<Vec<Agent>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let Ok(mut conn) = db::open() else { return };
        loop {
            let agents = shared(&conn).unwrap_or_else(|| discover(&mut conn));
            if tx.send(agents).is_err() {
                return; // the owner is gone
            }
            std::thread::sleep(TICK);
        }
    });
    rx
}

/// Another live process's fresh snapshot, if there is one.
fn shared(conn: &Connection) -> Option<Vec<Agent>> {
    let (pid, ts, json) = db::agents_snapshot(conn).ok()??;
    let fresh = pid != std::process::id() && db::now_ms() - ts < STALE_MS;
    fresh.then(|| serde_json::from_str(&json).ok()).flatten()
}

fn discover(conn: &mut Connection) -> Vec<Agent> {
    let mut agents = agents::discover();
    for a in agents.iter_mut() {
        let Some(path) = &a.transcript else { continue };
        let s = transcript::summary(path, a.kind);
        a.title = a.title.take().or(s.title);
        if let Some(e @ Entry { item: Item::Reply(text), .. }) = &s.reply {
            let body: String = text.trim().chars().take(4000).collect();
            let _ = db::post_status_if_new(conn, &a.name, a.kind.as_str(), &e.key, &body, e.ts);
        }
    }
    if let Ok(json) = serde_json::to_string(&agents) {
        let _ = db::set_agents_snapshot(conn, &json);
    }
    agents
}
