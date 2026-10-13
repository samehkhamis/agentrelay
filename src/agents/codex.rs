//! Codex threads: live ones are those loaded in the app-server daemon, other
//! threads touched recently are listed as away.

use super::{basename, group_of, Address, Agent, Kind, Presence};
use crate::db::home;
use crate::{ipc, transcript};
use anyhow::Result;
use std::path::PathBuf;

struct Thread {
    id: String,
    cwd: String,
    title: String,
    rollout: PathBuf,
    updated: i64,
}

pub fn list() -> Vec<Agent> {
    let loaded = ipc::codex::AppServer::connect().and_then(|mut c| c.loaded_threads()).unwrap_or_default();
    let hours: i64 = std::env::var("AGENTCHAT_CODEX_HOURS").ok().and_then(|h| h.parse().ok()).unwrap_or(24);
    let since = chrono::Utc::now().timestamp() - hours * 3600;
    threads()
        .unwrap_or_default()
        .into_iter()
        .filter(|t| loaded.contains(&t.id) || t.updated > since)
        .map(|t| {
            let presence = if !loaded.contains(&t.id) {
                Presence::Away
            } else if transcript::codex::turn_open(&t.rollout) {
                Presence::Busy
            } else {
                Presence::Online
            };
            let name = name(&t.cwd, &t.id);
            Agent {
                group: group_of(&name),
                name,
                kind: Kind::Codex,
                presence,
                title: Some(t.title.lines().next().unwrap_or("").chars().take(80).collect()),
                cwd: t.cwd,
                transcript: Some(t.rollout),
                address: Address::Codex(t.id),
            }
        })
        .collect()
}

/// `<cwd basename>-<last 4 hex of the thread id>`, matching Claude's naming.
fn name(cwd: &str, thread_id: &str) -> String {
    let id: String = thread_id.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    format!("{}-{}", basename(cwd), &id[id.len().saturating_sub(4)..])
}

fn threads() -> Result<Vec<Thread>> {
    let db = home().join(".codex/state_5.sqlite");
    let conn = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut st = conn.prepare(
        "SELECT id, cwd, COALESCE(name, title), rollout_path, updated_at FROM threads
         WHERE archived = 0 ORDER BY updated_at DESC LIMIT 200",
    )?;
    let rows = st.query_map([], |r| {
        Ok(Thread {
            id: r.get(0)?,
            cwd: r.get(1)?,
            title: r.get(2)?,
            rollout: PathBuf::from(r.get::<_, String>(3)?),
            updated: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}
