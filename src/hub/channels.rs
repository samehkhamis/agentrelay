//! The channel model: #all, one per agent kind, one per project group, and a
//! DM per agent. Big channels carry status updates; DMs show the transcript.

use crate::agents::{group_of, Agent, Kind, Presence};
use crate::db::{self, Msg};
use std::collections::BTreeSet;

const TOP: [&str; 3] = ["all", "claude", "codex"];

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Section {
    Top,
    Projects,
    Direct,
}

#[derive(Clone)]
pub struct Channel {
    pub id: String,
    pub section: Section,
    /// DMs: the agent's kind and presence (None once it's gone).
    pub kind: Option<Kind>,
    pub presence: Option<Presence>,
}

impl Channel {
    pub fn name(&self) -> &str {
        self.id.strip_prefix("dm:").unwrap_or(&self.id)
    }

    /// The agent behind a DM.
    pub fn agent(&self) -> Option<&str> {
        self.id.strip_prefix("dm:")
    }

    /// `#webapp`, or the agent's name for a DM.
    pub fn title(&self) -> String {
        match self.section {
            Section::Direct => self.name().to_string(),
            _ => format!("#{}", self.name()),
        }
    }
}

pub fn build(agents: &[Agent], msgs: &[Msg]) -> Vec<Channel> {
    let top = TOP.iter().map(|id| Channel { id: id.to_string(), section: Section::Top, kind: None, presence: None });

    // Projects: groups of current agents, plus group channels with history.
    let mut groups: BTreeSet<String> = agents.iter().map(|a| a.group.clone()).collect();
    let recent = db::now_ms() - 24 * 3600 * 1000;
    let mut gone: BTreeSet<&str> = BTreeSet::new();
    for m in msgs {
        match m.channel.strip_prefix("dm:") {
            Some(n) if m.ts > recent && !agents.iter().any(|a| a.name == n) => {
                gone.insert(n);
            }
            Some(_) => {}
            None if !TOP.contains(&m.channel.as_str()) => {
                groups.insert(m.channel.clone());
            }
            None => {}
        }
    }
    let projects = groups.into_iter().map(|g| Channel { id: g, section: Section::Projects, kind: None, presence: None });

    // Direct: live agents busy-first, then ones recently heard from but gone.
    let mut dms: Vec<Channel> = agents
        .iter()
        .map(|a| Channel { id: format!("dm:{}", a.name), section: Section::Direct, kind: Some(a.kind), presence: Some(a.presence) })
        .collect();
    dms.extend(gone.into_iter().map(|n| {
        let kind = msgs.iter().rev().find(|m| m.sender == n).map(|m| Kind::parse(&m.sender_kind));
        Channel { id: format!("dm:{n}"), section: Section::Direct, kind, presence: None }
    }));
    let rank = |c: &Channel| match c.presence {
        Some(Presence::Busy) => 0,
        Some(Presence::Online) => 1,
        _ => 2,
    };
    dms.sort_by(|a, b| rank(a).cmp(&rank(b)).then(a.id.cmp(&b.id)));

    top.chain(projects).chain(dms).collect()
}

/// Whether a logged message shows in a channel: messages posted there, plus
/// agents' status updates in every channel the agent belongs to.
pub fn visible(m: &Msg, chan: &str) -> bool {
    m.channel == chan
        || (m.kind == "status" && (chan == "all" || chan == m.sender_kind || chan == group_of(&m.sender)))
}

pub fn members<'a>(agents: &'a [Agent], chan: &Channel) -> Vec<&'a Agent> {
    agents
        .iter()
        .filter(|a| match chan.section {
            Section::Top => chan.id == "all" || chan.id == a.kind.as_str(),
            Section::Projects => a.group == chan.id,
            Section::Direct => chan.agent() == Some(a.name.as_str()),
        })
        .collect()
}

/// Messages from others in a channel after `seen`.
pub fn unread(msgs: &[Msg], chan: &str, seen: i64, me: &str) -> usize {
    msgs.iter().rev().take_while(|m| m.id > seen).filter(|m| m.sender != me && visible(m, chan)).count()
}
