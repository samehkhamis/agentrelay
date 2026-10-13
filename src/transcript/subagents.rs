//! Finding an agent's subagents. Claude Code writes each one to
//! `<session>/subagents/agent-<id>.jsonl` with a `.meta.json` (type and task
//! description); Codex records spawned threads in `thread_spawn_edges`.

use crate::agents::Kind;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// (transcript, label) of subagents whose transcript changed since `since_ms`.
pub fn find(main: &Path, kind: Kind, since_ms: i64) -> Vec<(PathBuf, String)> {
    let found = match kind {
        Kind::Claude => claude(main),
        Kind::Codex => codex(main),
    };
    found.into_iter().filter(|(p, _)| modified_ms(p) >= since_ms).collect()
}

fn modified_ms(p: &Path) -> i64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as i64)
}

fn claude(main: &Path) -> Vec<(PathBuf, String)> {
    let dir = main.with_extension("").join("subagents");
    let Ok(read) = std::fs::read_dir(&dir) else { return Vec::new() };
    read.flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .map(|p| {
            let meta: Value = std::fs::read_to_string(p.with_extension("meta.json"))
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or(Value::Null);
            let field = |k: &str| meta.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            let label = match (field("agentType"), field("description")) {
                (t, d) if !t.is_empty() && !d.is_empty() => format!("{t} · {d}"),
                (t, d) if t.is_empty() && d.is_empty() => "subagent".into(),
                (t, d) => format!("{t}{d}"),
            };
            (p, label)
        })
        .collect()
}

/// Child threads of the rollout's thread (its id ends the file name).
fn codex(main: &Path) -> Vec<(PathBuf, String)> {
    let stem = main.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let parent = &stem[stem.len().saturating_sub(36)..];
    let query = || -> rusqlite::Result<Vec<(PathBuf, String)>> {
        let conn = crate::agents::codex::state_db()?;
        let mut st = conn.prepare(
            "SELECT t.rollout_path, COALESCE(t.agent_nickname, t.name, t.title) FROM thread_spawn_edges e
             JOIN threads t ON t.id = e.child_thread_id WHERE e.parent_thread_id = ?1",
        )?;
        let rows = st.query_map([parent], |r| Ok((PathBuf::from(r.get::<_, String>(0)?), r.get::<_, String>(1)?)))?;
        rows.map(|r| r.map(|(p, t)| (p, t.lines().next().unwrap_or("subagent").chars().take(60).collect())))
            .collect()
    };
    query().unwrap_or_default()
}
