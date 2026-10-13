//! Claude Code session lines: `user`, `assistant`, `queued_command`
//! attachments (prompts sent mid-turn) and `ai-title`.

use super::{display_name, first_string, images, one_line, parse_ts, prompt, tool, Entry, Event, Item, Opts};
use serde_json::Value;

/// One line; subagent (`isSidechain`) lines only when reading a subagent file.
pub(super) fn parse(v: &Value, opts: Opts) -> Vec<Event> {
    if !opts.sidechain && v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return Vec::new();
    }
    let ts = parse_ts(v.get("timestamp"));
    let uuid = v.get("uuid").and_then(Value::as_str).unwrap_or("").to_string();
    let meta = v.get("isMeta").and_then(Value::as_bool) == Some(true);
    match v.get("type").and_then(Value::as_str) {
        Some("ai-title") => v.get("aiTitle").and_then(Value::as_str).map(|t| Event::Title(t.into())).into_iter().collect(),
        Some("attachment") => {
            let a = &v["attachment"];
            match (a.get("type").and_then(Value::as_str), a.get("prompt").and_then(Value::as_str)) {
                (Some("queued_command"), Some(p)) => user_text(uuid, ts, p, false, Vec::new()).into_iter().collect(),
                _ => Vec::new(),
            }
        }
        Some("user") => match v.pointer("/message/content") {
            Some(Value::String(s)) => user_text(uuid, ts, s, meta, Vec::new()).into_iter().collect(),
            Some(Value::Array(items)) => {
                let mut events = Vec::new();
                let mut texts = Vec::new();
                let mut files = Vec::new();
                for i in items {
                    match i.get("type").and_then(Value::as_str) {
                        Some("tool_result") => {
                            // Tool results can carry images, e.g. screenshots.
                            let shots = match i.get("content") {
                                Some(Value::Array(c)) if opts.images => c.iter().filter_map(image).collect(),
                                _ => Vec::new(),
                            };
                            events.push(Event::ToolResult {
                                call: i.get("tool_use_id").and_then(Value::as_str).unwrap_or("").to_string(),
                                error: i.get("is_error").and_then(Value::as_bool) == Some(true),
                                files: shots,
                            })
                        }
                        Some("text") => texts.extend(i.get("text").and_then(Value::as_str)),
                        Some("image") if opts.images => files.extend(image(i)),
                        _ => {}
                    }
                }
                if !texts.is_empty() || !files.is_empty() {
                    events.extend(user_text(uuid, ts, &texts.join("\n"), meta, files));
                }
                events
            }
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
/// agentchat / another session (in a `<cross-session-message>` envelope, which
/// Claude logs as a meta entry when it starts a turn). Other meta entries and
/// harness wrappers are skipped.
fn user_text(key: String, ts: i64, text: &str, meta: bool, files: Vec<String>) -> Option<Event> {
    if let Some((from, body)) = crate::ipc::claude::parse_envelope(text) {
        return Some(prompt(key, ts, from, body, files));
    }
    if meta {
        return None;
    }
    if let Some(rest) = text.strip_prefix("<command-name>") {
        let cmd = rest.split("</command-name>").next().unwrap_or(rest);
        return Some(prompt(key, ts, None, cmd, files));
    }
    if text.trim_start().starts_with('<') || (text.trim().is_empty() && files.is_empty()) {
        return None; // local command output, reminders, notifications
    }
    Some(prompt(key, ts, None, text, files))
}

/// An inline `{type: image, source: {type: base64, media_type, data}}` block.
fn image(item: &Value) -> Option<String> {
    if item.get("type").and_then(Value::as_str) != Some("image") {
        return None;
    }
    let src = item.get("source")?;
    let media = src.get("media_type").and_then(Value::as_str).unwrap_or("image/png");
    images::store(media, src.get("data")?.as_str()?)
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
