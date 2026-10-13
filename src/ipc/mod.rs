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

/// Inverse of [`compose`]: (`alex in #webapp`, rest of the text).
pub fn decompose(text: &str) -> Option<(&str, &str)> {
    text.strip_prefix(PREFIX)?.split_once(": ")
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
