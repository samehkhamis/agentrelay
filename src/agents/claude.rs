//! Claude Code sessions, from the supported `claude agents --json`.

use super::{group_of, Address, Agent, Kind, Presence};
use crate::db::home;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn list() -> Vec<Agent> {
    let out = match Command::new("claude").args(["agents", "--json"]).output() {
        Ok(o) if o.status.success() => o.stdout,
        _ => return Vec::new(),
    };
    let list: Vec<Value> = serde_json::from_slice(&out).unwrap_or_default();
    let projects = home().join(".claude/projects");
    list.iter()
        .filter_map(|a| {
            let name = a.get("name")?.as_str()?.to_string();
            let pid = a.get("pid")?.as_u64()? as u32;
            let cwd = a.get("cwd").and_then(Value::as_str).unwrap_or("").to_string();
            let presence = match a.get("status").and_then(Value::as_str) {
                Some("busy") => Presence::Busy,
                _ => Presence::Online,
            };
            let transcript = a
                .get("sessionId")
                .and_then(Value::as_str)
                .and_then(|sid| find_transcript(&projects, &cwd, sid));
            Some(Agent {
                group: group_of(&name),
                name,
                kind: Kind::Claude,
                presence,
                cwd,
                title: None,
                transcript,
                address: Address::Claude(pid),
            })
        })
        .collect()
}

/// `~/.claude/projects/<cwd as slug>/<session>.jsonl`, or a search if the slug differs.
fn find_transcript(projects: &Path, cwd: &str, session_id: &str) -> Option<PathBuf> {
    let file = format!("{session_id}.jsonl");
    let slug: String = cwd.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let direct = projects.join(slug).join(&file);
    if direct.exists() {
        return Some(direct);
    }
    std::fs::read_dir(projects).ok()?.flatten().map(|d| d.path().join(&file)).find(|p| p.exists())
}
