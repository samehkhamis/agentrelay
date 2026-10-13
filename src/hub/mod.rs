//! The shared core both UIs render: agents, channels, the message log, read
//! marks, followed transcripts and sending. Every process works off the same
//! database, so the TUI and the web UI show the same state.

mod actions;
pub mod channels;
mod commands;
pub mod feed;
mod watcher;

pub use actions::Task;
pub use commands::{help, parse_input, theme_list, Completion, Input};

use crate::agents::{Agent, Kind, Presence};
use crate::db::{self, Msg};
use crate::files::{self, FileRef};
use crate::{ipc, theme};
use crate::transcript::Tail;
use crate::uploads;
use commands::completions;
use anyhow::Result;
use channels::{Channel, Section};
use feed::Post;
use rusqlite::Connection;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

const HISTORY: i64 = 3000;
/// A message logged and ready to deliver: run [`Delivery::run`] off the UI thread.
pub struct Delivery {
    targets: Vec<Agent>,
    text: String,
    images: Vec<PathBuf>,
}

impl Delivery {
    /// How many agents it goes to.
    pub fn len(&self) -> usize {
        self.targets.len()
    }

    pub fn run(&self) -> ipc::Report {
        ipc::deliver(&self.targets, &self.text, &self.images)
    }
}

pub struct Hub {
    conn: Connection,
    pub me: String,
    msgs: Vec<Msg>,
    last_id: i64,
    pub agents: Vec<Agent>,
    pub channels: Vec<Channel>,
    seen: HashMap<String, i64>,
    /// Transcripts followed for DMs, by agent name.
    tails: HashMap<String, Tail>,
    agents_rx: Receiver<Vec<Agent>>,
}

impl Hub {
    pub fn open() -> Result<Hub> {
        let conn = db::open()?;
        let msgs = db::tail(&conn, HISTORY)?;
        let mut hub = Hub {
            seen: db::seen(&conn)?,
            last_id: msgs.last().map_or(0, |m| m.id),
            conn,
            me: std::env::var("AGENTCHAT_USER").or_else(|_| std::env::var("USER")).unwrap_or_else(|_| "you".into()),
            msgs,
            agents: Vec::new(),
            channels: Vec::new(),
            tails: HashMap::new(),
            agents_rx: watcher::spawn(),
        };
        hub.channels = channels::build(&hub.agents, &hub.msgs);
        hub.sync_theme()?;
        Ok(hub)
    }

    /// Adopts the theme chosen in any UI.
    fn sync_theme(&self) -> Result<()> {
        if let Some(name) = db::setting(&self.conn, "theme")? {
            theme::set(&name);
        }
        Ok(())
    }

    /// Switches every UI to a theme. False if there's no such theme.
    pub fn set_theme(&mut self, name: &str) -> Result<bool> {
        if !theme::set(name) {
            return Ok(false);
        }
        db::set_setting(&self.conn, "theme", name)?;
        Ok(true)
    }

    /// Steps to the next theme, for every UI; returns its name.
    pub fn cycle_theme(&mut self) -> Result<&'static str> {
        let names = theme::names();
        let i = names.iter().position(|n| *n == theme::t().name).unwrap_or(0);
        let next = names[(i + 1) % names.len()];
        self.set_theme(next)?;
        Ok(next)
    }

    /// Picks up new messages, agents and read marks. Returns the agents that
    /// posted a new reply since the last poll.
    pub fn poll(&mut self) -> Result<Vec<String>> {
        // Agents first: the watcher logs a batch of statuses before publishing
        // the list, so reading the log after it sees the whole batch.
        let mut refreshed = None;
        while let Ok(a) = self.agents_rx.try_recv() {
            refreshed = Some(a);
        }
        let refreshed_any = refreshed.is_some();
        let first_batch = refreshed_any && self.agents.is_empty();

        let new = db::since(&self.conn, self.last_id)?;
        if let Some(m) = new.last() {
            self.last_id = m.id;
        }
        let replied = new.iter().filter(|m| m.kind == "status").map(|m| m.sender.clone()).collect();
        let new_channel = new.iter().any(|m| !self.channels.iter().any(|c| c.id == m.channel));
        self.msgs.extend(new);
        if self.msgs.len() > 2 * HISTORY as usize {
            self.msgs.drain(..self.msgs.len() - HISTORY as usize); // a long-running server
        }
        if let Some(agents) = refreshed {
            self.agents = agents;
        }
        if refreshed_any || new_channel {
            self.channels = channels::build(&self.agents, &self.msgs);
        }
        self.seen = db::seen(&self.conn)?;
        self.sync_theme()?;
        if first_batch {
            // Channels never opened anywhere start out read.
            for c in &self.channels {
                if !self.seen.contains_key(&c.id) {
                    db::mark_seen(&self.conn, &c.id, self.last_id)?;
                }
            }
            self.seen = db::seen(&self.conn)?;
        }
        Ok(replied)
    }

    pub fn channel(&self, id: &str) -> Option<&Channel> {
        self.channels.iter().find(|c| c.id == id)
    }

    pub fn members(&self, chan: &Channel) -> Vec<&Agent> {
        channels::members(&self.agents, chan)
    }

    pub fn online(&self) -> usize {
        self.agents.iter().filter(|a| a.presence != Presence::Away).count()
    }

    pub fn unread(&self, chan: &Channel) -> usize {
        channels::unread(&self.msgs, &chan.id, self.seen.get(&chan.id).copied().unwrap_or(0), &self.me)
    }

    /// Marks a channel read up to now, for every UI.
    pub fn mark_seen(&mut self, chan: &str) {
        if self.seen.get(chan).copied().unwrap_or(0) < self.last_id && db::mark_seen(&self.conn, chan, self.last_id).is_ok() {
            self.seen.insert(chan.to_string(), self.last_id);
        }
    }

    /// The id of each agent's newest status, so a UI can tell when one replies.
    pub fn last_statuses(&self) -> HashMap<&str, i64> {
        let mut out = HashMap::new();
        for m in self.msgs.iter().filter(|m| m.kind == "status") {
            out.insert(m.sender.as_str(), m.id);
        }
        out
    }

    /// Starts or refreshes the transcript behind a DM.
    pub fn follow(&mut self, chan: &str) {
        let Some(name) = chan.strip_prefix("dm:") else { return };
        let Some(agent) = self.agents.iter().find(|a| a.name == name) else { return };
        let Some(path) = agent.transcript.clone() else { return };
        let kind = agent.kind;
        match self.tails.get_mut(name) {
            Some(t) if t.path() == path => {
                t.poll();
            }
            _ => {
                self.tails.insert(name.to_string(), Tail::open(&path, kind));
            }
        }
    }

    /// The feed for a channel: the transcript for a followed DM, else the log.
    pub fn posts(&self, chan: &Channel) -> Vec<Post<'_>> {
        let mut posts = match chan.agent().and_then(|n| self.tails.get_key_value(n)) {
            Some((name, tail)) => feed::transcript_posts(tail, name, chan.kind.unwrap_or(Kind::Claude), &self.me),
            None => feed::log_posts(&self.msgs, chan),
        };
        // Mentions resolve against the DM agent's folder, or the author's.
        let cwd_of = |name: &str| self.agents.iter().find(|a| a.name == name).map(|a| a.cwd.as_str());
        let dm = chan.agent().and_then(cwd_of);
        for p in &mut posts {
            p.cwd = dm.or_else(|| cwd_of(p.author));
        }
        posts
    }

    /// Files a feed mentions (tool calls and inline code), newest first.
    pub fn mentioned_files(&self, chan: &Channel) -> Vec<(String, FileRef)> {
        let mut out: Vec<(String, FileRef)> = Vec::new();
        for p in self.posts(chan).iter().rev() {
            let cwd = p.cwd.unwrap_or("");
            let raws: Vec<String> = match &p.body {
                feed::Body::Tool { path: Some(path), .. } => vec![path.to_string()],
                feed::Body::Markdown { text, .. } => crate::highlight::markdown(text)
                    .filter_map(|e| match e {
                        pulldown_cmark::Event::Code(c) => Some(c.to_string()),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            for raw in raws {
                // Images already show inline; the file views are for text.
                let new = |f: &FileRef| !uploads::is_image(&f.path) && !out.iter().any(|(_, o)| o.path == f.path);
                if let Some(f) = files::resolve(cwd, &raw).filter(new) {
                    out.push((raw, f));
                }
            }
        }
        out
    }

    /// Opens a file mentioned in a channel for viewing: inside one of its
    /// agents' folders, or one the feed mentions. Returns it with its line.
    pub fn open_file(&self, chan: &Channel, raw: &str) -> Result<(FileRef, String)> {
        let cwds: Vec<&str> = self.posts(chan).iter().filter_map(|p| p.cwd).collect();
        let file = cwds
            .iter()
            .find_map(|cwd| files::resolve(cwd, raw))
            .ok_or_else(|| anyhow::anyhow!("no such file: {raw}"))?;
        let inside = |cwd: &&str| file.path.canonicalize().ok().is_some_and(|p| p.starts_with(cwd));
        let mentioned = || self.mentioned_files(chan).iter().any(|(_, f)| f.path == file.path);
        anyhow::ensure!(cwds.iter().any(inside) || mentioned(), "{raw} isn't in this agent's folder");
        let text = files::read_text(&file.path).ok_or_else(|| anyhow::anyhow!("{raw} isn't a text file"))?;
        Ok((file, text))
    }

    /// A tool call's full input (markdown), by post key, for expanding it.
    pub fn tool_full(&self, chan: &Channel, key: &str) -> Option<String> {
        self.posts(chan).into_iter().find(|p| p.key == key).and_then(|p| match p.body {
            feed::Body::Tool { full, .. } if !full.is_empty() => Some(full.to_string()),
            _ => None,
        })
    }

    /// Changes whenever the channel's feed does, so a UI can skip redrawing.
    pub fn version(&self, chan: &Channel) -> String {
        match chan.agent().and_then(|n| self.tails.get(n)) {
            Some(t) => format!("{}:t{}", theme::t().name, t.version()),
            None => format!("{}:m{}", theme::t().name, self.last_id),
        }
    }

    /// A DM's conversation title, shown beside the agent's name.
    pub fn headline(&self, chan: &Channel) -> Option<String> {
        (chan.section == Section::Direct).then(|| self.members(chan).first()?.title.clone()).flatten()
    }

    /// What an empty channel says.
    pub fn empty_hint(&self, chan: &Channel) -> String {
        match chan.section {
            Section::Direct => format!("Nothing from {} yet.", chan.name()),
            _ => format!("This is the start of {}. Type /status to ask everyone here for an update.", chan.title()),
        }
    }

    /// The composer's hint, in message or shell mode.
    pub fn placeholder(&self, chan: &Channel, shell: bool) -> String {
        match (shell, chan.agent()) {
            (false, _) => format!("Message {} · ! for shell", chan.title()),
            (true, Some(agent)) => format!("Shell command for {agent} · Backspace to leave"),
            (true, None) => "Shell commands run in a DM · Backspace to leave".into(),
        }
    }

    /// The line under the channel title.
    pub fn detail(&self, chan: &Channel) -> String {
        let members = self.members(chan);
        if chan.section == Section::Direct {
            return match members.first() {
                Some(a) => {
                    format!("{} · {} · {}", a.kind.as_str(), a.presence.label(), db::tilde(&a.cwd))
                }
                None => "offline".into(),
            };
        }
        let count = |p: Presence| members.iter().filter(|a| a.presence == p).count();
        let kinds: Vec<String> = [Kind::Claude, Kind::Codex]
            .iter()
            .map(|k| (k, members.iter().filter(|a| a.kind == *k).count()))
            .filter(|(_, n)| *n > 0)
            .map(|(k, n)| format!("{n} {}", k.as_str()))
            .collect();
        format!(
            "{} agents · {} · {} busy · {} online · {} away",
            members.len(),
            kinds.join(" · "),
            count(Presence::Busy),
            count(Presence::Online),
            count(Presence::Away)
        )
    }

    /// Logs a message (and files already in the uploads directory) in a
    /// channel and returns its delivery to everyone live there (None if nobody is).
    pub fn send(&mut self, chan_id: &str, body: &str, files: &[PathBuf]) -> Result<Option<Delivery>> {
        let Some(chan) = self.channel(chan_id).cloned() else { return Ok(None) };
        let paths: Vec<String> = files.iter().map(|f| f.display().to_string()).collect();
        db::post(&self.conn, &chan.id, &self.me, body, &paths)?;
        let targets: Vec<Agent> =
            self.members(&chan).into_iter().filter(|a| a.presence != Presence::Away).cloned().collect();
        let channel = (chan.section != Section::Direct).then(|| chan.name().to_string());
        let text = ipc::compose(&self.me, channel.as_deref(), body, files);
        let images = files.iter().filter(|f| uploads::is_image(f)).cloned().collect();
        self.poll()?;
        Ok((!targets.is_empty()).then_some(Delivery { targets, text, images }))
    }

    /// Autocomplete for a composer line (folders are only read for `/new`).
    pub fn complete(&self, line: &str) -> Vec<Completion> {
        let folders = if line.starts_with("/new ") { self.folders() } else { Vec::new() };
        completions(line, &folders)
    }


}

