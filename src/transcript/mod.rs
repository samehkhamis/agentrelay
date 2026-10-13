//! Reading agent transcripts: Claude Code's session JSONL and Codex rollouts,
//! both parsed into the same [`Entry`] stream, plus their subagents'.

pub mod claude;
pub mod codex;
mod images;
mod subagents;

use crate::agents::Kind;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub enum Item {
    /// A prompt; `from` is set when it came through agentchat or another
    /// session; `files` are attached files and images (paths).
    Prompt { from: Option<String>, text: String, files: Vec<String> },
    Reply(String),
    /// `files` are images the tool returned, e.g. screenshots.
    Tool { name: String, detail: String, error: bool, files: Vec<String> },
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
    ToolResult { call: String, error: bool, files: Vec<String> },
    Title(String),
}

/// How a line is read.
#[derive(Clone, Copy)]
struct Opts {
    /// Keep lines Claude marks as subagent traffic (for subagent files).
    sidechain: bool,
    /// Write out inline images; off for quick scans like [`summary`].
    images: bool,
}

fn parse(kind: Kind, line: &str, opts: Opts) -> Vec<Event> {
    let Ok(v) = serde_json::from_str::<Value>(line) else { return Vec::new() };
    match kind {
        Kind::Claude => claude::parse(&v, opts),
        Kind::Codex => codex::parse(&v, opts),
    }
}

const MAIN_BYTES: u64 = 3 << 20;
const SUB_BYTES: u64 = 512 << 10;
const MAX_ENTRIES: usize = 1500;
const SUB_SCAN: Duration = Duration::from_secs(3);

/// One transcript file followed as it grows: the last `window` bytes on open,
/// then whatever gets appended.
struct Log {
    path: PathBuf,
    kind: Kind,
    window: u64,
    opts: Opts,
    offset: u64,
    partial: Vec<u8>,
    /// The first read starts mid-file, so its first line is dropped.
    skip_first: bool,
    entries: Vec<Entry>,
    /// Tool call id -> index in `entries`, to mark failed calls.
    calls: HashMap<String, usize>,
}

impl Log {
    fn open(path: &Path, kind: Kind, window: u64, opts: Opts) -> Log {
        let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let offset = len.saturating_sub(window);
        let mut log = Log {
            path: path.to_path_buf(),
            kind,
            window,
            opts,
            offset,
            partial: Vec::new(),
            skip_first: offset > 0,
            entries: Vec::new(),
            calls: HashMap::new(),
        };
        log.poll();
        log
    }

    /// Reads what was appended since the last poll. True if anything changed.
    fn poll(&mut self) -> bool {
        let Ok(mut f) = std::fs::File::open(&self.path) else { return false };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.offset {
            *self = Log::open(&self.path.clone(), self.kind, self.window, self.opts); // replaced
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
        for line in String::from_utf8_lossy(&buf).lines() {
            if std::mem::take(&mut self.skip_first) {
                continue;
            }
            for ev in parse(self.kind, line, self.opts) {
                self.apply(ev);
            }
        }
        if self.entries.len() > MAX_ENTRIES {
            self.entries.drain(..self.entries.len() - MAX_ENTRIES);
            self.calls.clear(); // indices moved; old calls just lose error marks
        }
        n > 0
    }

    fn apply(&mut self, ev: Event) {
        match ev {
            Event::Entry(e, call) => {
                if let Some(id) = call {
                    self.calls.insert(id, self.entries.len());
                }
                self.entries.push(e);
            }
            Event::ToolResult { call, error, files } => {
                let tool = self.calls.get(&call).and_then(|i| self.entries.get_mut(*i)).map(|e| &mut e.item);
                if let Some(Item::Tool { error: failed, files: shown, .. }) = tool {
                    *failed |= error;
                    shown.extend(files);
                }
            }
            Event::Title(_) => {}
        }
    }
}

/// A subagent's transcript, labeled like `Explore · Map the API`.
pub struct Sub {
    pub label: String,
    log: Log,
}

impl Sub {
    /// Its tool calls and replies; its task prompt shows as the parent's tool call.
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.log.entries.iter().filter(|e| !matches!(e.item, Item::Prompt { .. }))
    }
}

/// An agent's transcript and its subagents', followed live.
pub struct Tail {
    main: Log,
    pub subs: Vec<Sub>,
    scanned: Instant,
}

impl Tail {
    pub fn open(path: &Path, kind: Kind) -> Tail {
        let opts = Opts { sidechain: false, images: true };
        let mut t = Tail { main: Log::open(path, kind, MAIN_BYTES, opts), subs: Vec::new(), scanned: Instant::now() };
        t.scan_subagents();
        t
    }

    pub fn path(&self) -> &Path {
        &self.main.path
    }

    pub fn entries(&self) -> &[Entry] {
        &self.main.entries
    }

    /// Picks up appended lines, and new subagents every few seconds.
    pub fn poll(&mut self) {
        self.main.poll();
        for s in &mut self.subs {
            s.log.poll();
        }
        if self.scanned.elapsed() >= SUB_SCAN {
            self.scan_subagents();
        }
    }

    /// Starts following subagents active since the oldest entry shown.
    fn scan_subagents(&mut self) {
        self.scanned = Instant::now();
        let since = self.main.entries.first().map_or(0, |e| e.ts);
        for (path, label) in subagents::find(&self.main.path, self.main.kind, since) {
            if !self.subs.iter().any(|s| s.log.path == path) {
                let log = Log::open(&path, self.main.kind, SUB_BYTES, Opts { sidechain: true, images: true });
                self.subs.push(Sub { label, log });
            }
        }
    }

    /// Changes whenever anything shown does.
    pub fn version(&self) -> String {
        let mut v = format!("{}:{}", self.main.entries.len(), self.main.offset);
        for s in &self.subs {
            v.push_str(&format!(",{}", s.log.offset));
        }
        v
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
        for ev in parse(kind, line, Opts { sidechain: false, images: false }) {
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

/// A prompt entry, recognising messages sent through agentchat (and their
/// attached files) on top of any `files` the transcript carried inline.
fn prompt(key: String, ts: i64, from: Option<String>, text: &str, mut files: Vec<String>) -> Event {
    let (from, text) = match crate::ipc::decompose(text) {
        Some(m) => {
            files.extend(m.files);
            (Some(format!("{} · via agentchat", m.from)), m.body)
        }
        None => (from, text),
    };
    Event::Entry(Entry { key, ts, item: Item::Prompt { from, text: text.to_string(), files } }, None)
}

fn tool(key: String, ts: i64, name: String, detail: String, call: Option<String>) -> Event {
    Event::Entry(Entry { key, ts, item: Item::Tool { name, detail, error: false, files: Vec::new() } }, call)
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
