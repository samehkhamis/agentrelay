//! Delivering messages to agents over their own sockets.

pub mod claude;
pub mod codex;

use crate::agents::{Address, Agent};
use std::path::PathBuf;

const PREFIX: &str = "[agentchat] ";

/// The text an agent receives: `[agentchat] alex in #webapp: …`, or
/// `[agentchat] alex: …` for a DM, followed by any attached files' paths.
pub fn compose(from: &str, channel: Option<&str>, body: &str, files: &[PathBuf]) -> String {
    let mut text = match channel {
        Some(c) => format!("{PREFIX}{from} in #{c}: {body}"),
        None => format!("{PREFIX}{from}: {body}"),
    };
    if !files.is_empty() {
        text.push_str("\n\nAttached files:");
        for f in files {
            text.push_str(&format!("\n- `{}`", f.display()));
        }
    }
    text
}

/// How a shell command and its output read, both in what agents receive and
/// in DMs: the command, the output in a code block, and the exit code.
pub fn shell_report(command: &str, output: &str, exit: Option<i32>) -> String {
    let exit = exit.map_or("did not finish".into(), |c| format!("exit {c}"));
    let output = output.trim_end();
    let fence = if output.contains("```") { "````" } else { "```" };
    format!("ran `$ {}`\n{fence}\n{output}\n{fence}\n{exit}", command.trim())
}

/// A message as [`compose`] wrote it.
pub struct Incoming<'a> {
    /// `alex in #webapp`, or `alex` for a DM.
    pub from: &'a str,
    pub body: &'a str,
    pub files: Vec<String>,
}

/// Inverse of [`compose`].
pub fn decompose(text: &str) -> Option<Incoming<'_>> {
    let (from, rest) = text.strip_prefix(PREFIX)?.split_once(": ")?;
    let (body, list) = rest.split_once("\n\nAttached files:").unwrap_or((rest, ""));
    let files = list
        .lines()
        .filter_map(|l| l.trim().strip_prefix("- `")?.strip_suffix('`'))
        .map(str::to_string)
        .collect();
    Some(Incoming { from, body, files })
}

/// Sends `text` to each agent: Claude sessions directly, Codex threads through
/// one shared app-server connection, with `images` as image inputs (Claude
/// reads them from the paths in the text). Returns (delivered, failed) names.
pub fn deliver(targets: &[Agent], text: &str, images: &[PathBuf]) -> (Vec<String>, Vec<String>) {
    let mut ok = Vec::new();
    let mut failed = Vec::new();
    let mut server: Option<Result<codex::AppServer, String>> = None;
    for a in targets {
        let res = match &a.address {
            Address::Claude(pid) => claude::send(*pid, text).map_err(|e| e.to_string()),
            Address::Codex(thread) => match server.get_or_insert_with(|| codex::AppServer::connect().map_err(|e| e.to_string())) {
                Ok(s) => s.start_turn(thread, text, images).map_err(|e| e.to_string()),
                Err(e) => Err(e.clone()),
            },
        };
        match res {
            Ok(()) => ok.push(a.name.clone()),
            Err(_) => failed.push(a.name.clone()),
        }
    }
    (ok, failed)
}

#[cfg(test)]
mod tests {
    use super::{compose, decompose};
    use std::path::PathBuf;

    #[test]
    fn decompose_inverts_compose() {
        let text = compose("alex", Some("infra"), "look: here", &[PathBuf::from("/u/1-a.png")]);
        let m = decompose(&text).unwrap();
        assert_eq!((m.from, m.body, m.files), ("alex in #infra", "look: here", vec!["/u/1-a.png".to_string()]));
    }
}
