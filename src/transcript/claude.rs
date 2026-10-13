//! Claude Code session lines: `user`, `assistant`, `queued_command`
//! attachments (prompts sent mid-turn) and `ai-title`.

use super::{display_name, first_string, one_line, parse_ts, prompt, tool, Entry, Event, Item};
use serde_json::Value;

pub(super) fn parse(v: &Value) -> Vec<Event> {
    if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return Vec::new(); // subagent traffic
    }
    let ts = parse_ts(v.get("timestamp"));
    let uuid = v.get("uuid").and_then(Value::as_str).unwrap_or("").to_string();
    let meta = v.get("isMeta").and_then(Value::as_bool) == Some(true);
    match v.get("type").and_then(Value::as_str) {
        Some("ai-title") => v.get("aiTitle").and_then(Value::as_str).map(|t| Event::Title(t.into())).into_iter().collect(),
        Some("attachment") => {
            let a = &v["attachment"];
            match (a.get("type").and_then(Value::as_str), a.get("prompt").and_then(Value::as_str)) {
                (Some("queued_command"), Some(p)) => user_text(uuid, ts, p).into_iter().collect(),
                _ => Vec::new(),
            }
        }
        Some("user") => match v.pointer("/message/content") {
            Some(Value::String(s)) if !meta => user_text(uuid, ts, s).into_iter().collect(),
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(|i| match i.get("type").and_then(Value::as_str) {
                    Some("tool_result") => Some(Event::ToolResult {
                        call: i.get("tool_use_id").and_then(Value::as_str).unwrap_or("").to_string(),
                        error: i.get("is_error").and_then(Value::as_bool) == Some(true),
                    }),
                    Some("text") if !meta => user_text(uuid.clone(), ts, i.get("text")?.as_str()?),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        },
        Some("assistant") => {
            let Some(Value::Array(items)) = v.pointer("/message/content") else { return Vec::new() };
            items
                .iter()
                .enumerate()
                .filter_map(|(n, i)| {
                    let key = format!("{uuid}:{n}");
                    match i.get("type").and_then(Value::as_str)? {
                        "text" => {
                            let t = i.get("text")?.as_str()?;
                            (!t.trim().is_empty()).then(|| Event::Entry(Entry { key, ts, item: Item::Reply(t.into()) }, None))
                        }
                        "tool_use" => {
                            let name = i.get("name")?.as_str()?;
                            let input = i.get("input").unwrap_or(&Value::Null);
                            let call = i.get("id").and_then(Value::as_str).map(str::to_string);
                            Some(tool(key, ts, display_name(name), summarize(name, input), call))
                        }
                        _ => None,
                    }
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

/// A user-side text: a typed prompt, a slash command, or a message from
/// agentchat / another session (in a `<cross-session-message>` envelope).
/// Harness-generated wrappers are skipped.
fn user_text(key: String, ts: i64, text: &str) -> Option<Event> {
    if let Some((from, body)) = crate::ipc::claude::parse_envelope(text) {
        return Some(prompt(key, ts, from, body));
    }
    if let Some(rest) = text.strip_prefix("<command-name>") {
        let cmd = rest.split("</command-name>").next().unwrap_or(rest);
        return Some(prompt(key, ts, None, cmd));
    }
    if text.trim_start().starts_with('<') || text.trim().is_empty() {
        return None; // local command output, reminders, notifications
    }
    Some(prompt(key, ts, None, text))
}

fn summarize(name: &str, input: &Value) -> String {
    let field = |k: &str| input.get(k).and_then(Value::as_str);
    let detail = match name {
        "Bash" => field("command"),
        "Read" | "Write" | "Edit" | "NotebookEdit" => field("file_path").or(field("notebook_path")),
        "Grep" | "Glob" => field("pattern"),
        "WebFetch" => field("url"),
        "WebSearch" => field("query"),
        "Agent" | "Task" => field("description"),
        _ => None,
    };
    detail.map(one_line).unwrap_or_else(|| first_string(input))
}
