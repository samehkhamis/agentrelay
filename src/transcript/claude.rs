//! Claude Code session lines: `user`, `assistant`, `queued_command`
//! attachments (prompts sent mid-turn) and `ai-title`.

use super::{change, display_name, fenced, first_string, images, one_line, parse_ts, prompt, tool, Call, ChangeOp, Entry, Event, Item, Opts};
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
                    events.extend(user_text(uuid.clone(), ts, &texts.join("\n"), meta, files));
                }
                events.extend(edit_result(format!("{uuid}:change"), ts, v.get("toolUseResult")));
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
                            Some(tool(key, ts, describe(name, input), call))
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
/// agentrelay / another session (in a `<cross-session-message>` envelope, which
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

/// What an Edit or Write changed, from its result: the hunks it applied, or
/// a new file's content.
fn edit_result(key: String, ts: i64, result: Option<&Value>) -> Option<Event> {
    let r = result?.as_object()?;
    let path = r.get("filePath")?.as_str()?;
    let hunks = r.get("structuredPatch").and_then(Value::as_array).filter(|h| !h.is_empty());
    let (op, diff) = match (r.get("type").and_then(Value::as_str), hunks) {
        (_, Some(hunks)) => (ChangeOp::Updated, hunks.iter().map(hunk).collect::<Vec<_>>().join("\n")),
        (Some("create"), None) => {
            let content = r.get("content")?.as_str()?;
            let lines: Vec<String> = content.lines().map(|l| format!("+{l}")).collect();
            (ChangeOp::Added, format!("@@ -0,0 +1,{} @@\n{}", lines.len(), lines.join("\n")))
        }
        _ => return None,
    };
    Some(change(key, ts, path, op, diff))
}

/// One `structuredPatch` hunk as unified diff text.
fn hunk(h: &Value) -> String {
    let n = |k: &str| h.get(k).and_then(Value::as_u64).unwrap_or(0);
    let lines: Vec<&str> = h.get("lines").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
    format!("@@ -{},{} +{},{} @@\n{}", n("oldStart"), n("oldLines"), n("newStart"), n("newLines"), lines.join("\n"))
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

/// A Claude tool call: its one-liner, its full input as markdown (a command,
/// a diff for edits, the written content), and the file it works on.
fn describe(name: &str, input: &Value) -> Call {
    let field = |k: &str| input.get(k).and_then(Value::as_str);
    let path = field("file_path").or(field("notebook_path")).map(str::to_string);
    let ext = |p: &Option<String>| p.as_deref().and_then(|p| p.rsplit_once('.')).map_or("", |(_, e)| e).to_string();
    let detail = match name {
        "Bash" => field("command"),
        "Read" | "Write" | "Edit" | "NotebookEdit" => path.as_deref(),
        "Grep" | "Glob" => field("pattern"),
        "WebFetch" => field("url"),
        "WebSearch" => field("query"),
        "Agent" | "Task" => field("description"),
        _ => None,
    };
    let detail = detail.map(one_line).unwrap_or_else(|| first_string(input));
    let full = match name {
        "Bash" => fenced("bash", field("command").unwrap_or("")),
        "Edit" => edit_diff(field("old_string").unwrap_or(""), field("new_string").unwrap_or("")),
        "MultiEdit" => input
            .get("edits")
            .and_then(Value::as_array)
            .map(|edits| {
                let diffs: Vec<String> = edits
                    .iter()
                    .map(|e| edit_diff(e["old_string"].as_str().unwrap_or(""), e["new_string"].as_str().unwrap_or("")))
                    .collect();
                diffs.join("\n\n")
            })
            .unwrap_or_default(),
        "Write" => fenced(&ext(&path), field("content").unwrap_or("")),
        "Read" => String::new(), // the path says it all
        _ => fenced("json", &serde_json::to_string_pretty(input).unwrap_or_default()),
    };
    Call { name: display_name(name), detail, full, path }
}

/// An edit as a unified-looking diff block.
fn edit_diff(old: &str, new: &str) -> String {
    let lines = |text: &str, mark: char| text.lines().map(|l| format!("{mark} {l}")).collect::<Vec<_>>().join("\n");
    fenced("diff", &format!("{}\n{}", lines(old, '-'), lines(new, '+')))
}

#[cfg(test)]
mod tests {
    use super::super::{ChangeOp, Event, Item};
    use super::edit_result;
    use serde_json::json;

    fn change(ev: Option<Event>) -> (String, ChangeOp, usize, usize, String) {
        match ev {
            Some(Event::Entry(e, _)) => match e.item {
                Item::Change(c) => (c.path, c.op, c.added, c.removed, c.diff),
                _ => panic!("not a change"),
            },
            _ => panic!("no entry"),
        }
    }

    #[test]
    fn reads_edits_and_new_files() {
        let edit = json!({
            "filePath": "/p/a.rs",
            "structuredPatch": [{ "oldStart": 3, "oldLines": 2, "newStart": 3, "newLines": 3, "lines": [" x", "-y", "+y2", "+z"] }],
        });
        let (path, op, added, removed, diff) = change(edit_result("k".into(), 0, Some(&edit)));
        assert_eq!((path.as_str(), op, added, removed), ("/p/a.rs", ChangeOp::Updated, 2, 1));
        assert!(diff.starts_with("@@ -3,2 +3,3 @@\n x\n-y"));

        let new = json!({ "type": "create", "filePath": "/p/b.md", "content": "one\ntwo\n", "structuredPatch": [] });
        let (_, op, added, removed, _) = change(edit_result("k".into(), 0, Some(&new)));
        assert_eq!((op, added, removed), (ChangeOp::Added, 2, 0));

        // A failed edit's result is a string.
        assert!(edit_result("k".into(), 0, Some(&json!("Error: no match"))).is_none());
    }
}
