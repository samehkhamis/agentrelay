//! Things the hub does for a UI beyond messaging: starting agents, renaming
//! conversations and running `!` commands. Slow work comes back as a [`Task`]
//! to run off the UI thread.

use super::channels::{Channel, Section};
use super::Hub;
use crate::agents::{Address, Agent, Kind, Presence};
use crate::{db, ipc};
use anyhow::Result;
use std::path::PathBuf;

/// Work to run off the UI thread (starting an agent, running a command);
/// returns a one-line note for the UI.
pub struct Task(Box<dyn FnOnce() -> String + Send>);

impl Task {
    pub fn run(self) -> String {
        (self.0)()
    }
}

impl Hub {
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

    /// A channel's own folder: its DM agent's, or its project's.
    pub fn default_folder(&self, chan: &Channel) -> Option<String> {
        (chan.section != Section::Top).then(|| self.members(chan).first().map(|a| a.cwd.clone())).flatten()
    }

    /// The folder for a new agent: the given one, else the channel's own.
    fn folder(&self, chan: &Channel, given: Option<&str>) -> Result<PathBuf> {
        let path = match (given, self.default_folder(chan)) {
            (Some(f), _) => db::expand_tilde(f),
            (None, Some(f)) => PathBuf::from(f),
            (None, None) => anyhow::bail!("say which folder: /new claude ~/code/project [prompt]"),
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
            let shown = db::tilde(&cwd.display().to_string());
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
    /// session takes the new name (so its DM moves to the returned channel), a
    /// Codex thread gets it as its title.
    pub fn rename(&self, chan_id: &str, name: &str) -> Result<(Task, String)> {
        let agent = self.dm_agent(chan_id, "/rename")?;
        let follow = match agent.address {
            Address::Claude(_) => format!("dm:{name}"),
            Address::Codex(_) => chan_id.to_string(),
        };
        let name = name.to_string();
        let task = Task(Box::new(move || {
            let done = match &agent.address {
                Address::Claude(pid) => ipc::claude::rename(*pid, &name),
                Address::Codex(thread) => ipc::codex::AppServer::connect().and_then(|mut s| s.rename(thread, &name)),
            };
            match done {
                Ok(()) => format!("renamed {} to {name}", agent.name),
                Err(e) => format!("couldn't rename: {e}"),
            }
        }));
        Ok((task, follow))
    }

    /// Runs a command for a DM's agent, like `!` in its own terminal. Codex
    /// runs it in the thread's shell and records it there; for Claude,
    /// agentrelay runs it in the agent's folder and sends the output to it.
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
/// code; None if it didn't finish within the timeout, when its whole process
/// group is killed).
fn run_shell(command: &str, cwd: &str) -> (String, Option<i32>) {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let child = std::process::Command::new(shell)
        .args(["-lc", command])
        .current_dir(cwd)
        .process_group(0)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return (e.to_string(), None),
    };
    // Drain both pipes on their own threads so a chatty command can't block.
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut out = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut out);
            }
            out
        })
    };
    let stdout = drain(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let stderr = drain(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let deadline = std::time::Instant::now() + SHELL_TIMEOUT;
    let exit = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(50)),
            _ => {
                if let Some(pid) = rustix::process::Pid::from_raw(child.id() as i32) {
                    let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::Kill);
                }
                let _ = child.wait();
                break None;
            }
        }
    };
    let mut text = String::from_utf8_lossy(&stdout.join().unwrap_or_default()).to_string();
    text.push_str(&String::from_utf8_lossy(&stderr.join().unwrap_or_default()));
    if exit.is_none() {
        text.push_str(&format!("\n(killed after {}s)", SHELL_TIMEOUT.as_secs()));
    }
    let start = text.char_indices().rev().nth(SHELL_OUTPUT).map_or(0, |(i, _)| i);
    (text[start..].to_string(), exit)
}
