//! Claude Code sessions, from the supported `claude agents --json`.

use super::{group_for, Address, Agent, Kind, Presence};
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
            let cwd = live_cwd(pid, a.get("cwd").and_then(Value::as_str).unwrap_or(""));
            let presence = match a.get("status").and_then(Value::as_str) {
                Some("busy") => Presence::Busy,
                _ => Presence::Online,
            };
            let transcript = a
                .get("sessionId")
                .and_then(Value::as_str)
                .and_then(|sid| find_transcript(&projects, &cwd, sid));
            Some(Agent {
                group: group_for(&name, &cwd),
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

/// The session's folder. Claude records it at start; if it has since been
/// renamed or moved, the process's own working directory (on Linux) still
/// points at it.
fn live_cwd(pid: u32, recorded: &str) -> String {
    if Path::new(recorded).is_dir() {
        return recorded.to_string();
    }
    match std::fs::read_link(format!("/proc/{pid}/cwd")) {
        Ok(dir) if dir.is_dir() => dir.display().to_string(),
        _ => recorded.to_string(),
    }
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
