//! Shared state in one SQLite file (WAL mode), so every agentchat process (the
//! TUI, the web server) sees the same messages, read marks and agents.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Msg {
    pub id: i64,
    /// Unix milliseconds.
    pub ts: i64,
    /// `all`, `claude`, `codex`, a project group like `webapp`, or `dm:<agent>`.
    pub channel: String,
    pub sender: String,
    /// `user`, `claude`, `codex` or `system`.
    pub sender_kind: String,
    /// `chat` for posted messages, `status` for an agent's latest transcript reply.
    pub kind: String,
    pub body: String,
    /// Attached files (paths in the uploads directory).
    pub files: Vec<String>,
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
}

pub fn db_path() -> PathBuf {
    match std::env::var("AGENTCHAT_DB") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => home().join(".agentchat").join("chat.db"),
    }
}

/// Where agentchat keeps its files: the database's directory.
pub fn data_dir() -> PathBuf {
    db_path().parent().map(PathBuf::from).unwrap_or_else(home)
}

pub fn open() -> Result<Connection> {
    let path = db_path();
    let dir = data_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let conn = Connection::open(&path).with_context(|| format!("opening {}", path.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ts INTEGER NOT NULL,
            channel TEXT NOT NULL,
            sender TEXT NOT NULL,
            sender_kind TEXT NOT NULL,
            kind TEXT NOT NULL DEFAULT 'chat',
            body TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS messages_channel ON messages(channel, id);
        -- Fingerprint of the last status posted per agent, to avoid reposting it.
        CREATE TABLE IF NOT EXISTS statuses (agent TEXT PRIMARY KEY, fingerprint TEXT NOT NULL);
        -- Last message id read per channel, shared by every UI.
        CREATE TABLE IF NOT EXISTS seen (channel TEXT PRIMARY KEY, last_id INTEGER NOT NULL);
        -- Preferences shared by every UI, e.g. the theme.
        CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
        -- The latest discovered agent list (JSON), and which process wrote it.
        CREATE TABLE IF NOT EXISTS agents (id INTEGER PRIMARY KEY CHECK (id = 1), pid INTEGER NOT NULL,
                                           ts INTEGER NOT NULL, json TEXT NOT NULL);",
    )?;
    // Attachments came later: one path per line.
    let has_files = conn.prepare("SELECT files FROM messages LIMIT 0").is_ok();
    if !has_files {
        conn.execute_batch("ALTER TABLE messages ADD COLUMN files TEXT NOT NULL DEFAULT ''")?;
    }
    Ok(conn)
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Logs a message from the user.
pub fn post(conn: &Connection, channel: &str, sender: &str, body: &str, files: &[String]) -> Result<i64> {
    conn.execute(
        "INSERT INTO messages (ts, channel, sender, sender_kind, kind, body, files)
         VALUES (?1, ?2, ?3, 'user', 'chat', ?4, ?5)",
        params![now_ms(), channel, sender, body, files.join("\n")],
    )?;
    Ok(conn.last_insert_rowid())
}

fn row_to_msg(r: &rusqlite::Row) -> rusqlite::Result<Msg> {
    Ok(Msg {
        id: r.get(0)?,
        ts: r.get(1)?,
        channel: r.get(2)?,
        sender: r.get(3)?,
        sender_kind: r.get(4)?,
        kind: r.get(5)?,
        body: r.get(6)?,
        files: r.get::<_, String>(7)?.lines().map(str::to_string).collect(),
    })
}

/// Messages with id > `after`, oldest first.
pub fn since(conn: &Connection, after: i64) -> Result<Vec<Msg>> {
    let mut st = conn.prepare(
        "SELECT id, ts, channel, sender, sender_kind, kind, body, files FROM messages WHERE id > ?1 ORDER BY id",
    )?;
    let rows = st.query_map([after], row_to_msg)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The most recent `limit` messages, oldest first.
pub fn tail(conn: &Connection, limit: i64) -> Result<Vec<Msg>> {
    let mut st = conn.prepare(
        "SELECT id, ts, channel, sender, sender_kind, kind, body, files FROM
           (SELECT * FROM messages ORDER BY id DESC LIMIT ?1) ORDER BY id",
    )?;
    let rows = st.query_map([limit], row_to_msg)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Posts a status message unless the same fingerprint was already posted for
/// this agent. Atomic, so two TUIs running at once don't double-post.
pub fn post_status_if_new(
    conn: &mut Connection,
    agent: &str,
    agent_kind: &str,
    fingerprint: &str,
    body: &str,
    ts: i64,
) -> Result<bool> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let prev: Option<String> = tx
        .query_row("SELECT fingerprint FROM statuses WHERE agent = ?1", [agent], |r| r.get(0))
        .optional()?;
    if prev.as_deref() == Some(fingerprint) {
        return Ok(false);
    }
    tx.execute(
        "INSERT INTO statuses (agent, fingerprint) VALUES (?1, ?2)
         ON CONFLICT(agent) DO UPDATE SET fingerprint = excluded.fingerprint",
        params![agent, fingerprint],
    )?;
    tx.execute(
        "INSERT INTO messages (ts, channel, sender, sender_kind, kind, body) VALUES (?1, ?2, ?3, ?4, 'status', ?5)",
        params![ts, format!("dm:{agent}"), agent, agent_kind, body],
    )?;
    tx.commit()?;
    Ok(true)
}

pub fn seen(conn: &Connection) -> Result<HashMap<String, i64>> {
    let mut st = conn.prepare("SELECT channel, last_id FROM seen")?;
    let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Marks a channel read up to `last_id`; never moves backwards.
pub fn mark_seen(conn: &Connection, channel: &str, last_id: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO seen (channel, last_id) VALUES (?1, ?2)
         ON CONFLICT(channel) DO UPDATE SET last_id = MAX(last_id, excluded.last_id)",
        params![channel, last_id],
    )?;
    Ok(())
}

/// The shared agent list: (writer pid, unix ms, JSON).
pub fn agents_snapshot(conn: &Connection) -> Result<Option<(u32, i64, String)>> {
    Ok(conn
        .query_row("SELECT pid, ts, json FROM agents WHERE id = 1", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()?)
}

pub fn set_agents_snapshot(conn: &Connection, json: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO agents (id, pid, ts, json) VALUES (1, ?1, ?2, ?3)
         ON CONFLICT(id) DO UPDATE SET pid = excluded.pid, ts = excluded.ts, json = excluded.json",
        params![std::process::id(), now_ms(), json],
    )?;
    Ok(())
}

pub fn setting(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0)).optional()?)
}

pub fn set_setting(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}
