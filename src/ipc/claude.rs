//! Claude Code's session socket. Each session listens on the
//! `messagingSocketPath` in `~/.claude/sessions/<pid>.json` and publishes a
//! peer token in `<pid>.<hash>.key` beside it. A peer sends newline-delimited
//! JSON: an `auth` line with that token, then a `user` message whose content
//! is a `<cross-session-message>` envelope.

use crate::db::home;
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

const TAG: &str = "cross-session-message";

fn read_json(p: &Path) -> Result<Value> {
    let s = std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
    Ok(serde_json::from_str(&s)?)
}

pub fn send(pid: u32, text: &str) -> Result<()> {
    let dir = home().join(".claude/sessions");
    let session = read_json(&dir.join(format!("{pid}.json")))?;
    let sock = session
        .get("messagingSocketPath")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("session {pid} has no messaging socket"))?;
    let token = peer_token(&dir, pid)?;

    let mut s = UnixStream::connect(sock).with_context(|| format!("connecting to {sock}"))?;
    s.set_write_timeout(Some(Duration::from_secs(5)))?;
    let auth = json!({ "type": "auth", "token": token });
    let msg = json!({ "type": "user", "from": "agentchat", "message": { "role": "user", "content": envelope(text) } });
    s.write_all(format!("{auth}\n{msg}\n").as_bytes())?;
    s.shutdown(std::net::Shutdown::Write)?;
    Ok(())
}

/// Starts a background Claude Code session in `cwd` with a first prompt, in
/// bypass-permissions mode like the user's other sessions (supported
/// `claude --bg`; there is no socket call for creating a session). Returns
/// its short id. Claude refuses folders whose trust prompt wasn't accepted,
/// and says so; that message is the error.
pub fn start(cwd: &Path, prompt: &str) -> Result<String> {
    let out = Command::new("claude")
        .args(["--bg", "--permission-mode", "bypassPermissions", prompt])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .context("running claude")?;
    let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        return Err(anyhow!("{}", text.trim()));
    }
    // "backgrounded · ecb5563e"
    let id = text.lines().find_map(|l| l.split_once("backgrounded · ").map(|(_, id)| id.trim().to_string()));
    Ok(id.unwrap_or_default())
}

/// The newest `<pid>.<hash>.key` holds the token for the socket in use.
fn peer_token(dir: &Path, pid: u32) -> Result<String> {
    let prefix = format!("{pid}.");
    let key = std::fs::read_dir(dir)?
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with(&prefix) && n.ends_with(".key")
        })
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .ok_or_else(|| anyhow!("session {pid} has no peer key"))?;
    read_json(&key.path())?
        .get("peerToken")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("peer key without token"))
}

/// Wraps text in the envelope Claude sessions use with each other.
/// `from-mode` declares the sender's permission class: messages here are only
/// ever typed by the user, who runs these sessions in bypass mode, so a
/// bypass-mode recipient takes them without holding them for approval, as it
/// does for a peer session in the same mode.
fn envelope(text: &str) -> String {
    // The receiver rejects an envelope whose body could close the tag early.
    let body = text.replace("</", "< /");
    let mode = std::env::var("AGENTCHAT_FROM_MODE").unwrap_or_else(|_| "bypass".into());
    format!("<{TAG} from-name=\"agentchat\" from-mode=\"{mode}\">\n{body}\n</{TAG}>")
}

/// Finds an envelope in received text (Claude may wrap it in a notice) and
/// splits it into (from-name, body).
pub fn parse_envelope(text: &str) -> Option<(Option<String>, &str)> {
    let open = format!("<{TAG}");
    let rest = &text[text.find(&open)? + open.len()..];
    let (attrs, body) = rest.split_once(">\n")?;
    let body = body.split(&format!("\n</{TAG}>")).next().unwrap_or(body);
    let name = attrs
        .split_once("from-name=\"")
        .and_then(|(_, r)| r.split_once('"'))
        .map(|(n, _)| n.to_string());
    Some((name, body))
}

#[cfg(test)]
mod tests {
    use super::{envelope, parse_envelope};

    #[test]
    fn parses_envelopes_inside_notices() {
        let wrapped = format!("Another Claude session sent a message:\n{}\n\nThis came from…", envelope("hi there"));
        assert_eq!(parse_envelope(&wrapped), Some((Some("agentchat".into()), "hi there")));
    }
}
