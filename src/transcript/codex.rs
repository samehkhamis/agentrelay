//! Codex rollout lines: `response_item` messages and tool calls.

use super::{display_name, first_string, one_line, parse_ts, prompt, text_of, tail_lines, tool, Entry, Event, Item};
use serde_json::Value;
use std::path::Path;

pub(super) fn parse(v: &Value) -> Vec<Event> {
    if v.get("type").and_then(Value::as_str) != Some("response_item") {
        return Vec::new();
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
                // Environment context and AGENTS.md come in as user messages too.
                let injected = text.starts_with('<') || text.starts_with("# AGENTS.md");
                (!injected && !text.trim().is_empty()).then(|| prompt(key, ts, None, &text))
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
            let detail = command(args.get("cmd").or(args.get("command"))).unwrap_or_else(|| first_string(&args));
            Some(tool(key, ts, name, detail, call))
        }
        "custom_tool_call" => {
            let input = field("input");
            let detail = match field("name") {
                "exec" => exec_summary(input),
                "apply_patch" => patch_files(input),
                _ => one_line(input),
            };
            Some(tool(key, ts, field("name").to_string(), detail, call))
        }
        "local_shell_call" => {
            let detail = command(p.pointer("/action/command")).unwrap_or_default();
            Some(tool(key, ts, "shell".into(), detail, call))
        }
        _ => None,
    };
    event.into_iter().collect()
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
