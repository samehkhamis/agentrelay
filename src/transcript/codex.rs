//! Codex rollout lines: `response_item` messages and tool calls.

use super::{change, display_name, fenced, first_string, images, one_line, parse_ts, prompt, text_of, tail_lines, tool, Call, ChangeOp, Entry, Event, Item, Opts};
use serde_json::Value;
use std::path::Path;

pub(super) fn parse(v: &Value, opts: Opts) -> Vec<Event> {
    match v.get("type").and_then(Value::as_str) {
        Some("response_item") => {}
        Some("event_msg") => return file_changes(v),
        _ => return Vec::new(),
    }
    let p = &v["payload"];
    let ts = parse_ts(v.get("timestamp"));
    let key = p.get("id").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| ts.to_string());
    let call = p.get("call_id").and_then(Value::as_str).map(str::to_string);
    let field = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("");
    let event = match field("type") {
        "message" => match field("role") {
            "user" => {
                let text = text_of(p.get("content"), "input_text");
                let files: Vec<String> = p
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter(|i| opts.images && i.get("type").and_then(Value::as_str) == Some("input_image"))
                    .filter_map(|i| images::store_data_url(i.get("image_url")?.as_str()?))
                    .collect();
                if let Some(report) = shell_command(&text) {
                    return vec![prompt(key, ts, None, &report, Vec::new())];
                }
                // Environment context and AGENTS.md come in as user messages too.
                let injected = text.starts_with('<') || text.starts_with("# AGENTS.md");
                let empty = text.trim().is_empty() && files.is_empty();
                (!injected && !empty).then(|| prompt(key, ts, None, &text, files))
            }
            "assistant" => {
                let text = text_of(p.get("content"), "output_text");
                (!text.trim().is_empty()).then_some(Event::Entry(Entry { key, ts, item: Item::Reply(text) }, None))
            }
            _ => None,
        },
        "function_call" => {
            let name = match field("namespace") {
                "" => field("name").to_string(),
                ns => format!("{ns}.{}", field("name")),
            };
            let args: Value = serde_json::from_str(field("arguments")).unwrap_or(Value::Null);
            let cmd = args.get("cmd").or(args.get("command"));
            let detail = command(cmd).unwrap_or_else(|| first_string(&args));
            let full = match cmd {
                Some(Value::String(c)) => fenced("bash", c),
                _ => fenced("json", &serde_json::to_string_pretty(&args).unwrap_or_default()),
            };
            Some(tool(key, ts, Call { name, detail, full, path: None }, call))
        }
        "custom_tool_call" => {
            let input = field("input");
            let (detail, full) = match field("name") {
                "exec" => (exec_summary(input), fenced("js", input)),
                "apply_patch" => (patch_files(input), fenced("diff", input)),
                _ => (one_line(input), fenced("", input)),
            };
            Some(tool(key, ts, Call { name: field("name").to_string(), detail, full, path: None }, call))
        }
        "local_shell_call" => {
            let detail = command(p.pointer("/action/command")).unwrap_or_default();
            let full = fenced("bash", &detail);
            Some(tool(key, ts, Call { name: "shell".into(), detail, full, path: None }, call))
        }
        _ => None,
    };
    event.into_iter().collect()
}

/// The files a completed edit changed: a `FileChange` item, which carries a
/// unified diff per file.
fn file_changes(v: &Value) -> Vec<Event> {
    let p = &v["payload"];
    let item = &p["item"];
    let completed = p.get("type").and_then(Value::as_str) == Some("item_completed")
        && item.get("type").and_then(Value::as_str) == Some("FileChange")
        && item.get("status").and_then(Value::as_str) == Some("completed");
    let Some(changes) = item.get("changes").and_then(Value::as_object).filter(|_| completed) else { return Vec::new() };
    let ts = parse_ts(v.get("timestamp"));
    let id = item.get("id").and_then(Value::as_str).unwrap_or("");
    changes
        .iter()
        .map(|(path, c)| {
            let op = match c.get("type").and_then(Value::as_str) {
                Some("add") => ChangeOp::Added,
                Some("delete") => ChangeOp::Deleted,
                _ => ChangeOp::Updated,
            };
            // A move ends up at its new path.
            let path = c.get("move_path").and_then(Value::as_str).unwrap_or(path);
            let diff = c.get("unified_diff").and_then(Value::as_str).unwrap_or("").to_string();
            change(format!("{id}:{path}"), ts, path, op, diff)
        })
        .collect()
}

/// A `!command` run in the thread (`thread/shellCommand` or Codex's `!`),
/// recorded as a `<user_shell_command>` block, in agentrelay's report form.
fn shell_command(text: &str) -> Option<String> {
    let inner = text.strip_prefix("<user_shell_command>")?;
    let between = |open: &str, close: &str| inner.split_once(open).and_then(|(_, r)| r.split_once(close)).map(|(v, _)| v);
    let command = between("<command>", "</command>")?.trim();
    let result = between("<result>", "</result>").unwrap_or("");
    let exit = result.lines().find_map(|l| l.strip_prefix("Exit code: ")?.trim().parse().ok());
    let output = result.split_once("Output:\n").map_or("", |(_, o)| o);
    Some(crate::ipc::shell_report(command, output, exit))
}

/// A command given as a string or an argv array.
fn command(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) => Some(one_line(s)),
        Value::Array(a) => Some(one_line(&a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "))),
        _ => None,
    }
}

/// Code-mode `exec` calls are JS like `tools.exec_command({cmd:"…"})`: show
/// the shell commands, or else the tool names called.
fn exec_summary(js: &str) -> String {
    let cmds: Vec<String> = js.split("cmd:\"").skip(1).map(unescape_until_quote).collect();
    if let Some(first) = cmds.first() {
        let more = if cmds.len() > 1 { format!("  (+{} more)", cmds.len() - 1) } else { String::new() };
        return format!("{}{more}", one_line(first));
    }
    // Tool names in call order, repeats counted: `chrome_devtools.evaluate_script ×2`.
    let mut tools: Vec<(String, usize)> = Vec::new();
    let names = js.split("tools.").skip(1).filter_map(|s| s.split('(').next());
    for name in names.filter(|n| n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')) {
        let name = display_name(name);
        match tools.iter_mut().find(|(n, _)| *n == name) {
            Some((_, count)) => *count += 1,
            None => tools.push((name, 1)),
        }
    }
    if tools.is_empty() {
        return one_line(js);
    }
    let parts: Vec<String> =
        tools.into_iter().map(|(n, c)| if c > 1 { format!("{n} ×{c}") } else { n }).collect();
    parts.join(", ")
}

fn unescape_until_quote(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => break,
            '\\' => match chars.next() {
                Some('n') => out.push(' '),
                Some(e) => out.push(e),
                None => break,
            },
            c => out.push(c),
        }
    }
    out
}

/// `*** Update File: src/x.rs` lines of an apply_patch.
fn patch_files(patch: &str) -> String {
    let files: Vec<&str> = patch
        .lines()
        .filter_map(|l| l.strip_prefix("*** ").and_then(|r| r.split_once(": ")).map(|(_, f)| f))
        .collect();
    files.join(", ")
}

/// True when the rollout's last task event is a start without a completion.
pub fn turn_open(path: &Path) -> bool {
    for line in tail_lines(path, 64 << 10).iter().rev() {
        if line.contains("\"task_complete\"") || line.contains("\"turn_aborted\"") {
            return false;
        }
        if line.contains("\"task_started\"") {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::super::{ChangeOp, Event, Item};
    use super::file_changes;
    use serde_json::json;

    #[test]
    fn reads_file_changes() {
        let line = json!({
            "timestamp": "2026-10-12T10:00:00Z",
            "type": "event_msg",
            "payload": { "type": "item_completed", "item": {
                "type": "FileChange", "id": "x", "status": "completed",
                "changes": {
                    "/p/a.rs": { "type": "update", "unified_diff": "@@ -1 +1 @@\n-a\n+b\n", "move_path": null },
                    "/p/old.rs": { "type": "update", "unified_diff": "", "move_path": "/p/new.rs" },
                }
            }}
        });
        let got: Vec<(String, ChangeOp, usize, usize)> = file_changes(&line)
            .into_iter()
            .filter_map(|ev| match ev {
                Event::Entry(e, _) => match e.item {
                    Item::Change(c) => Some((c.path, c.op, c.added, c.removed)),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        assert_eq!(got, [("/p/a.rs".into(), ChangeOp::Updated, 1, 1), ("/p/new.rs".into(), ChangeOp::Updated, 0, 0)]);
    }
}
