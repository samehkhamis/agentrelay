//! The shared core both UIs render: agents, channels, the message log, read
//! marks, followed transcripts and sending. Every process works off the same
//! database, so the TUI and the web UI show the same state.

pub mod channels;
pub mod feed;
mod watcher;

use crate::agents::{Address, Agent, Kind, Presence};
use crate::db::{self, Msg};
use crate::{ipc, theme};
use crate::transcript::Tail;
use crate::uploads;
use anyhow::Result;
use channels::{Channel, Section};
use feed::Post;
use rusqlite::Connection;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

const HISTORY: i64 = 3000;
const STATUS_ASK: &str = "Quick status update please: what are you working on, what's done, \
                          and is anything blocked? 2-3 lines.";
pub const HELP: &str = "commands: /status  /new claude|codex [folder] [prompt]  /rename <name> (in a DM)  \
                        !command (in a DM)  /theme [name]  /sound  /quit";

/// Slash commands: (name, usage after the name, what it does).
const COMMANDS: [(&str, &str, &str); 6] = [
    ("/status", "", "ask everyone here for a status update"),
    ("/new", "claude|codex [folder] [prompt]", "start an agent"),
    ("/rename", "<name>", "rename this DM's conversation"),
    ("/theme", "[name]", "switch the theme for every UI"),
    ("/sound", "", "turn the reply chime on or off"),
    ("/quit", "", "leave agentchat"),
];

/// One autocomplete suggestion.
#[derive(serde::Serialize)]
pub struct Completion {
    /// The whole line once accepted.
    pub line: String,
    pub label: String,
    pub detail: String,
    /// Accepting it is a complete command: run it rather than keep typing.
    pub run: bool,
}

/// Suggestions for a composer line starting with `/`: commands, then their
/// arguments (agent kinds, theme names, folders).
pub fn completions(line: &str, folders: &[String]) -> Vec<Completion> {
    let item = |line: String, label: &str, detail: &str, run: bool| Completion {
        line,
        label: label.to_string(),
        detail: detail.to_string(),
        run,
    };
    let Some(rest) = line.strip_prefix('/') else { return Vec::new() };
    let Some((cmd, arg)) = rest.split_once(' ') else {
        return COMMANDS
            .iter()
            .filter(|(name, ..)| name[1..].starts_with(rest))
            .map(|(name, usage, what)| {
                let label = if usage.is_empty() { name.to_string() } else { format!("{name} {usage}") };
                let line = if usage.is_empty() { name.to_string() } else { format!("{name} ") };
                item(line, &label, what, usage.is_empty())
            })
            .collect();
    };
    match cmd {
        "theme" => theme::names()
            .into_iter()
            .filter(|n| n.starts_with(arg.trim()) && *n != arg.trim())
            .map(|n| item(format!("/theme {n}"), n, if n == theme::t().name { "current" } else { "" }, true))
            .collect(),
        "new" => match arg.split_once(' ') {
            None => ["claude", "codex"]
                .into_iter()
                .filter(|k| k.starts_with(arg) && *k != arg)
                .map(|k| item(format!("/new {k} "), k, "then a folder and a first message, both optional", false))
                .collect(),
            Some((kind, folder)) if folder.starts_with(['~', '/', '.']) && !folder.contains(' ') => {
                let home = db::home().display().to_string();
                folders
                    .iter()
                    .map(|f| f.replacen(&home, "~", 1))
                    .filter(|f| f.starts_with(folder) && f != folder)
                    .take(12)
                    .map(|f| item(format!("/new {kind} {f} "), &f, "", false))
                    .collect()
            }
            Some(_) => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// What the user typed in a composer.
pub enum Input {
    Send(String),
    /// `/new claude|codex [folder] <prompt>`: start an agent.
    New { kind: Kind, folder: Option<String>, prompt: String },
    /// `!command`: run it in a DM's agent's folder.
    Shell(String),
    /// `/rename <name>`: rename a DM's conversation.
    Rename(String),
    /// `/theme name`, or `/theme` alone to list them.
    Theme(Option<String>),
    ToggleSound,
    Quit,
    Help,
}

pub fn parse_input(text: &str) -> Option<Input> {
    let text = text.trim();
    if let Some(cmd) = text.strip_prefix('!') {
        return (!cmd.trim().is_empty()).then(|| Input::Shell(cmd.trim().to_string()));
    }
    if let Some(name) = text.strip_prefix("/rename") {
        let name = name.trim();
        return Some(if name.is_empty() { Input::Help } else { Input::Rename(name.to_string()) });
    }
    if let Some(rest) = text.strip_prefix("/new ") {
        let (kind, rest) = rest.trim().split_once(' ').unwrap_or((rest.trim(), ""));
        let kind = match kind {
            "claude" => Kind::Claude,
            "codex" => Kind::Codex,
            _ => return Some(Input::Help),
        };
        let rest = rest.trim();
        let (folder, prompt) = match rest.split_once(' ') {
            Some((f, p)) if f.starts_with(['/', '~', '.']) => (Some(f.to_string()), p.trim()),
            _ if rest.starts_with(['/', '~', '.']) => (Some(rest.to_string()), ""),
            _ => (None, rest),
        };
        return Some(Input::New { kind, folder, prompt: prompt.to_string() });
    }
    if let Some(rest) = text.strip_prefix("/theme") {
        let name = rest.trim();
        return Some(Input::Theme((!name.is_empty()).then(|| name.to_string())));
    }
    Some(match text {
        "" => return None,
        "/quit" | "/q" => Input::Quit,
        "/sound" => Input::ToggleSound,
        "/status" => Input::Send(STATUS_ASK.into()),
        t if t.starts_with('/') => Input::Help,
        t => Input::Send(t.into()),
    })
}

/// The `/theme` reply: the themes, the active one marked.
pub fn theme_list() -> String {
    let names: Vec<String> = theme::names()
        .into_iter()
        .map(|n| if n == theme::t().name { format!("[{n}]") } else { n.to_string() })
        .collect();
    format!("themes: {}", names.join(" "))
}

/// A message logged and ready to deliver: run [`Delivery::run`] off the UI thread.
pub struct Delivery {
    pub targets: Vec<Agent>,
    text: String,
    images: Vec<PathBuf>,
}

impl Delivery {
    /// (delivered, failed) agent names.
    pub fn run(&self) -> (Vec<String>, Vec<String>) {
        ipc::deliver(&self.targets, &self.text, &self.images)
    }
}

/// Work to run off the UI thread (starting an agent, running a command);
/// returns a one-line note for the UI.
pub struct Task(Box<dyn FnOnce() -> String + Send>);

impl Task {
    pub fn run(self) -> String {
        (self.0)()
    }
}

pub struct Hub {
    conn: Connection,
    pub me: String,
    pub msgs: Vec<Msg>,
    pub last_id: i64,
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
        match chan.agent().and_then(|n| self.tails.get_key_value(n)) {
            Some((name, tail)) => feed::transcript_posts(tail, name, chan.kind.unwrap_or(Kind::Claude), &self.me),
            None => feed::log_posts(&self.msgs, chan),
        }
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

    /// The line under the channel title.
    pub fn detail(&self, chan: &Channel) -> String {
        let members = self.members(chan);
        if chan.section == Section::Direct {
            return match members.first() {
                Some(a) => {
                    let home = db::home().display().to_string();
                    format!("{} · {} · {}", a.kind.as_str(), a.presence.label(), a.cwd.replacen(&home, "~", 1))
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

    /// Folders to offer for new agents: the agents' own, plus the projects
    /// Claude has been trusted in (read from its config, never written).
    pub fn folders(&self) -> Vec<String> {
        let mut out: Vec<String> = self.agents.iter().map(|a| a.cwd.clone()).collect();
        let config = std::fs::read_to_string(db::home().join(".claude.json")).unwrap_or_default();
        let config: serde_json::Value = serde_json::from_str(&config).unwrap_or_default();
        if let Some(projects) = config.get("projects").and_then(serde_json::Value::as_object) {
            let trusted = projects.iter().filter(|(_, p)| p.get("hasTrustDialogAccepted") == Some(&true.into()));
            out.extend(trusted.map(|(path, _)| path.clone()));
        }
        out.retain(|f| !f.is_empty() && std::path::Path::new(f).is_dir());
        out.sort();
        out.dedup();
        out
    }

    /// The folder for a new agent: the given one, else the DM agent's or the
    /// project channel's folder.
    fn folder(&self, chan: &Channel, given: Option<&str>) -> Result<PathBuf> {
        let path = match given {
            Some(f) => match f.strip_prefix('~') {
                Some(rest) => PathBuf::from(format!("{}{rest}", db::home().display())),
                None => PathBuf::from(f),
            },
            None => match self.members(chan).first() {
                Some(a) if chan.section != Section::Top => PathBuf::from(&a.cwd),
                _ => anyhow::bail!("say which folder: /new claude ~/code/project <prompt>"),
            },
        };
        anyhow::ensure!(path.is_dir(), "no folder {}", path.display());
        Ok(path)
    }

    /// Starts a Claude (background session) or Codex (app-server thread)
    /// agent, with a first prompt or idle. Its DM appears once discovery finds it.
    pub fn start_agent(&self, chan_id: &str, kind: Kind, folder: Option<&str>, prompt: &str) -> Result<Task> {
        let chan = self.channel(chan_id).ok_or_else(|| anyhow::anyhow!("no channel"))?;
        let cwd = self.folder(chan, folder)?;
        let prompt = prompt.trim().to_string();
        Ok(Task(Box::new(move || {
            let shown = cwd.display().to_string().replace(&db::home().display().to_string(), "~");
            let started = match kind {
                Kind::Claude => ipc::claude::start(&cwd, &prompt).map(|id| format!("claude {id}")),
                Kind::Codex => ipc::codex::AppServer::connect().and_then(|mut s| {
                    let thread = s.start_thread(&cwd)?;
                    if !prompt.is_empty() {
                        s.start_turn(&thread, &prompt, &[])?;
                    }
                    Ok("codex".to_string())
                }),
            };
            match started {
                Ok(what) => format!("started {what} in {shown}; it'll appear under Direct messages"),
                Err(e) => format!("couldn't start {}: {e}", kind.as_str()),
            }
        })))
    }

    /// The online agent behind a DM, for commands aimed at one agent.
    fn dm_agent(&self, chan_id: &str, what: &str) -> Result<Agent> {
        let chan = self.channel(chan_id).ok_or_else(|| anyhow::anyhow!("no channel"))?;
        match (chan.section, self.members(chan).first()) {
            (Section::Direct, Some(a)) if a.presence != Presence::Away => Ok((*a).clone()),
            _ => anyhow::bail!("{what} works in a DM with an online agent"),
        }
    }

    /// Renames a DM's conversation, as `/rename` does in the agent: a Claude
    /// session takes the new name (so its DM moves), a Codex thread gets it
    /// as its title. Returns the note and the channel to follow.
    pub fn rename(&self, chan_id: &str, name: &str) -> Result<(String, String)> {
        let agent = self.dm_agent(chan_id, "/rename")?;
        match &agent.address {
            Address::Claude(pid) => {
                ipc::claude::rename(*pid, name)?;
                Ok((format!("renamed {} to {name}", agent.name), format!("dm:{name}")))
            }
            Address::Codex(thread) => {
                ipc::codex::AppServer::connect()?.rename(thread, name)?;
                Ok((format!("renamed {}'s thread to {name}", agent.name), chan_id.to_string()))
            }
        }
    }

    /// Runs a command for a DM's agent, like `!` in its own terminal. Codex
    /// runs it in the thread's shell and records it there; for Claude,
    /// agentchat runs it in the agent's folder and sends the output to it.
    pub fn shell(&self, chan_id: &str, command: &str) -> Result<Task> {
        let agent = self.dm_agent(chan_id, "!command")?;
        let (command, me) = (command.to_string(), self.me.clone());
        Ok(Task(Box::new(move || {
            let done = match &agent.address {
                Address::Codex(thread) => {
                    ipc::codex::AppServer::connect().and_then(|mut s| s.shell_command(thread, &command))
                }
                Address::Claude(pid) => {
                    let (output, exit) = run_shell(&command, &agent.cwd);
                    let report = ipc::shell_report(&command, &output, exit);
                    ipc::claude::send(*pid, &ipc::compose(&me, None, &report, &[]))
                }
            };
            match done {
                Ok(()) => format!("ran in {}", agent.name),
                Err(e) => format!("couldn't run: {e}"),
            }
        })))
    }
}

/// Output cap for `!` commands, from the end.
const SHELL_OUTPUT: usize = 16_000;
const SHELL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Runs a command with the user's shell in `cwd`: (stdout and stderr, exit
/// code; None if it didn't finish within the timeout).
fn run_shell(command: &str, cwd: &str) -> (String, Option<i32>) {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let child = std::process::Command::new(shell)
        .args(["-lc", command])
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn();
    let child = match child {
        Ok(c) => c,
        Err(e) => return (e.to_string(), None),
    };
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(SHELL_TIMEOUT) {
        Ok(Ok(out)) => {
            let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
            let start = text.char_indices().rev().nth(SHELL_OUTPUT).map_or(0, |(i, _)| i);
            (text[start..].to_string(), out.status.code())
        }
        Ok(Err(e)) => (e.to_string(), None),
        Err(_) => (format!("still running after {}s", SHELL_TIMEOUT.as_secs()), None),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_input, Input};
    use crate::agents::Kind;

    #[test]
    fn completes_commands_and_arguments() {
        use super::completions;
        let labels = |line: &str| completions(line, &["/home/x/code/infra".into()]).into_iter().map(|c| c.line).collect::<Vec<_>>();
        assert_eq!(labels("/s"), ["/status", "/sound"]);
        assert_eq!(labels("/re"), ["/rename "]);
        assert_eq!(labels("/new c"), ["/new claude ", "/new codex "]);
        assert_eq!(labels("/theme dra"), ["/theme dracula"]);
        assert!(labels("hello").is_empty());
    }

    #[test]
    fn parses_new_and_shell() {
        match parse_input("/new codex ~/code/infra fix the CI") {
            Some(Input::New { kind: Kind::Codex, folder: Some(f), prompt }) => {
                assert_eq!((f.as_str(), prompt.as_str()), ("~/code/infra", "fix the CI"))
            }
            _ => panic!("expected /new with a folder"),
        }
        assert!(matches!(parse_input("/new claude look around"), Some(Input::New { folder: None, .. })));
        assert!(matches!(parse_input("!git status"), Some(Input::Shell(c)) if c == "git status"));
    }
}
