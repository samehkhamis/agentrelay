//! Discovering the running agents.

mod claude;
pub mod codex;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Claude,
    Codex,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Claude => "claude",
            Kind::Codex => "codex",
        }
    }

    pub fn parse(s: &str) -> Kind {
        if s == "codex" { Kind::Codex } else { Kind::Claude }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Presence {
    /// Live, mid-turn.
    Busy,
    /// Live, waiting for input.
    Online,
    /// A recent Codex thread not open in any session.
    Away,
}

impl Presence {
    pub fn label(self) -> &'static str {
        match self {
            Presence::Busy => "busy",
            Presence::Online => "online",
            Presence::Away => "away",
        }
    }
}

/// Where to deliver a message to an agent.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Address {
    /// A Claude Code session's pid; its socket and token are looked up from it.
    Claude(u32),
    /// A Codex thread id on the app-server daemon.
    Codex(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Agent {
    pub name: String,
    pub kind: Kind,
    pub group: String,
    pub presence: Presence,
    pub cwd: String,
    pub title: Option<String>,
    pub transcript: Option<PathBuf>,
    pub address: Address,
}

/// All live agents, plus recent Codex threads as away, sorted by group.
pub fn discover() -> Vec<Agent> {
    let mut agents = claude::list();
    agents.extend(codex::list());
    agents.sort_by(|a, b| a.group.cmp(&b.group).then(a.name.cmp(&b.name)));
    agents.dedup_by(|a, b| a.name == b.name);
    agents
}

/// `webapp-74` -> `webapp`, `api-server-6e` -> `api-server`.
/// Names without a short hex suffix are their own group.
pub fn group_of(name: &str) -> String {
    match name.rsplit_once('-') {
        Some((prefix, suffix))
            if !prefix.is_empty()
                && (2..=6).contains(&suffix.len())
                && suffix.chars().all(|c| c.is_ascii_hexdigit()) =>
        {
            prefix.to_string()
        }
        _ => name.to_string(),
    }
}

/// The project an agent belongs to: from its name (`webapp-74`), or for
/// names that aren't `folder-hex` (background sessions are named after their
/// first prompt), from its folder.
pub fn group_for(name: &str, cwd: &str) -> String {
    let group = group_of(name);
    if group == name && !cwd.is_empty() { basename(cwd) } else { group }
}

fn basename(p: &str) -> String {
    Path::new(p).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_string())
}
