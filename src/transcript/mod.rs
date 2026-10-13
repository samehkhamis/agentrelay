//! Reading agent transcripts: Claude Code's session JSONL and Codex rollouts,
//! both parsed into the same [`Entry`] stream.

pub mod claude;
pub mod codex;

use crate::agents::Kind;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub enum Item {
    /// A prompt; `from` is set when it came through agentchat or another session.
    Prompt { from: Option<String>, text: String },
    Reply(String),
    Tool { name: String, detail: String, error: bool },
}

pub struct Entry {
    /// Stable id within the transcript.
    pub key: String,
    /// Unix milliseconds.
    pub ts: i64,
    pub item: Item,
}

/// What one transcript line contributes.
enum Event {
    Entry(Entry, Option<String>),
    ToolResult { call: String, error: bool },
    Title(String),
}

fn parse(kind: Kind, line: &str) -> Vec<Event> {
    let Ok(v) = serde_json::from_str::<Value>(line) else { return Vec::new() };
    match kind {
        Kind::Claude => claude::parse(&v),
        Kind::Codex => codex::parse(&v),
    }
}

const INITIAL_BYTES: u64 = 3 << 20;
const MAX_ENTRIES: usize = 1500;

/// A transcript followed as it grows: the last few MB on open, then whatever
/// gets appended.
pub struct Tail {
    path: PathBuf,
    kind: Kind,
    offset: u64,
    partial: Vec<u8>,
    /// The first read starts mid-file, so its first line is dropped.
    skip_first: bool,
    pub entries: Vec<Entry>,
    /// Tool call id -> index in `entries`, to mark failed calls.
    calls: HashMap<String, usize>,
}

impl Tail {
    pub fn open(path: &Path, kind: Kind) -> Tail {
        let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let offset = len.saturating_sub(INITIAL_BYTES);
        let mut t = Tail {
            path: path.to_path_buf(),
            kind,
            offset,
            partial: Vec::new(),
            skip_first: offset > 0,
            entries: Vec::new(),
            calls: HashMap::new(),
        };
        t.poll();
        t
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads what was appended since the last poll. True if anything changed.
    pub fn poll(&mut self) -> bool {
        let Ok(mut f) = std::fs::File::open(&self.path) else { return false };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.offset {
            *self = Tail::open(&self.path.clone(), self.kind); // truncated or replaced
            return true;
        }
        if len == self.offset || f.seek(SeekFrom::Start(self.offset)).is_err() {
            return false;
        }
        let mut buf = std::mem::take(&mut self.partial);
        let Ok(n) = f.read_to_end(&mut buf) else { return false };
        self.offset += n as u64;
        let complete = buf.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        self.partial = buf.split_off(complete);
        let before = self.entries.len();
        for line in String::from_utf8_lossy(&buf).lines() {
            if std::mem::take(&mut self.skip_first) {
                continue;
            }
            for ev in parse(self.kind, line) {
                self.apply(ev);
            }
        }
        if self.entries.len() > MAX_ENTRIES {
            self.entries.drain(..self.entries.len() - MAX_ENTRIES);
            self.calls.clear(); // indices moved; old calls just lose error marks
        }
        self.entries.len() != before || n > 0
    }

    fn apply(&mut self, ev: Event) {
        match ev {
            Event::Entry(e, call) => {
                if let Some(id) = call {
                    self.calls.insert(id, self.entries.len());
                }
                self.entries.push(e);
            }
            Event::ToolResult { call, error: true } => {
                if let Some(Item::Tool { error, .. }) =
                    self.calls.get(&call).and_then(|i| self.entries.get_mut(*i)).map(|e| &mut e.item)
                {
                    *error = true;
                }
            }
            Event::ToolResult { .. } | Event::Title(_) => {}
        }
    }
}

/// An agent's latest reply and, for Claude, its session title.
pub struct Summary {
    pub reply: Option<Entry>,
    pub title: Option<String>,
}

pub fn summary(path: &Path, kind: Kind) -> Summary {
    let mut s = Summary { reply: None, title: None };
    for line in tail_lines(path, 768 << 10).iter().rev() {
        for ev in parse(kind, line) {
            match ev {
                Event::Entry(e @ Entry { item: Item::Reply(_), .. }, _) if s.reply.is_none() => s.reply = Some(e),
                Event::Title(t) if s.title.is_none() => s.title = Some(t),
                _ => {}
            }
        }
        if s.reply.is_some() && (s.title.is_some() || kind == Kind::Codex) {
            break;
        }
    }
    s
}

/// Complete lines in the last `max_bytes` of a file.
fn tail_lines(path: &Path, max_bytes: u64) -> Vec<String> {
    let Ok(mut f) = std::fs::File::open(path) else { return Vec::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(max_bytes);
    let mut buf = Vec::new();
    if f.seek(SeekFrom::Start(start)).is_err() || f.read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&buf);
    let skip = usize::from(start > 0); // probably a partial line
    text.lines().skip(skip).map(str::to_string).collect()
}

fn parse_ts(v: Option<&Value>) -> i64 {
    v.and_then(Value::as_str)
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.timestamp_millis())
        .unwrap_or_else(crate::db::now_ms)
}

/// Joins the `text` of content items of `item_type` (or a plain string).
fn text_of(content: Option<&Value>, item_type: &str) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter(|i| i.get("type").and_then(Value::as_str) == Some(item_type))
            .filter_map(|i| i.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// A prompt entry, recognising messages sent through agentchat.
fn prompt(key: String, ts: i64, from: Option<String>, text: &str) -> Event {
    let (from, text) = match crate::ipc::decompose(text) {
        Some((who, body)) => (Some(format!("{who} · via agentchat")), body),
        None => (from, text),
    };
    Event::Entry(Entry { key, ts, item: Item::Prompt { from, text: text.to_string() } }, None)
}

fn tool(key: String, ts: i64, name: String, detail: String, call: Option<String>) -> Event {
    Event::Entry(Entry { key, ts, item: Item::Tool { name, detail, error: false } }, call)
}

/// `mcp__linear__save_issue` -> `linear.save_issue`.
fn display_name(name: &str) -> String {
    match name.strip_prefix("mcp__") {
        Some(rest) => rest.replacen("__", ".", 1),
        None => name.to_string(),
    }
}

/// First line, trimmed and capped, for one-line tool summaries.
fn one_line(s: &str) -> String {
    let line = s.trim().lines().next().unwrap_or("");
    let more = if s.trim().lines().nth(1).is_some() { " …" } else { "" };
    let capped: String = line.chars().take(160).collect();
    format!("{capped}{more}")
}

/// The first string value in a JSON object, for tools without a known shape.
fn first_string(v: &Value) -> String {
    v.as_object()
        .and_then(|o| o.values().find_map(Value::as_str))
        .map(one_line)
        .unwrap_or_default()
}
