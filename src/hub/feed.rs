//! What a channel's feed shows, independent of how it's drawn: posts with
//! their grouping already decided, so both UIs lay out the same feed.

use super::channels::{self, Channel};
use crate::agents::Kind;
use crate::db::Msg;
use crate::transcript::{Change, ChangeOp, Entry, Item, Tail};
use chrono::{Local, TimeZone};

const STATUS_NOTE: &str = "↻ latest reply";
/// Status channels show this many lines of a reply before cutting it.
pub const STATUS_LINES: usize = 8;
const LIMIT: usize = 400;
const GROUP_GAP_MS: i64 = 5 * 60 * 1000;

/// How a long markdown body is cut to [`STATUS_LINES`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Clamp {
    Off,
    /// A status update: the full reply is in the agent's DM.
    ToDm,
    /// A subagent's reply: expands in place.
    Expand,
}

impl Clamp {
    /// What the cut says, in both UIs.
    pub fn hint(self) -> Option<&'static str> {
        match self {
            Clamp::Off => None,
            Clamp::ToDm => Some("… open the DM for the full reply"),
            Clamp::Expand => Some("… show more"),
        }
    }
}

pub enum Body<'a> {
    Markdown { text: &'a str, clamp: Clamp },
    /// A tool call: one line, expandable to its `full` input (markdown), and
    /// the file it worked on.
    Tool { name: &'a str, detail: &'a str, full: &'a str, path: Option<&'a str>, error: bool },
    /// The files a turn changed, at its end.
    Changes(Vec<Changed<'a>>),
}

/// A file changed during a turn: its edits merged.
pub struct Changed<'a> {
    pub path: &'a str,
    pub op: ChangeOp,
    pub added: usize,
    pub removed: usize,
    /// Each edit's diff, in order.
    pub diffs: Vec<&'a str>,
}

impl<'a> Changed<'a> {
    /// A turn's changes, one per file in the order first touched.
    fn merge(changes: &[(i64, &'a Change)]) -> Vec<Changed<'a>> {
        let mut out: Vec<Changed> = Vec::new();
        for (_, c) in changes {
            let i = match out.iter().position(|o| o.path == c.path) {
                Some(i) => i,
                None => {
                    out.push(Changed { path: &c.path, op: c.op, added: 0, removed: 0, diffs: Vec::new() });
                    out.len() - 1
                }
            };
            let f = &mut out[i];
            // Created this turn stays "added"; otherwise the last edit says.
            f.op = if f.op == ChangeOp::Added && c.op != ChangeOp::Deleted { ChangeOp::Added } else { c.op };
            f.added += c.added;
            f.removed += c.removed;
            f.diffs.push(&c.diff);
        }
        out
    }
}

pub struct Post<'a> {
    /// Stable id, for render caches.
    pub key: String,
    /// Unix milliseconds.
    pub ts: i64,
    pub author: &'a str,
    /// `claude`, `codex`, `user`, `peer` (another session) or `subagent`: the author color.
    pub author_kind: &'a str,
    /// Shown dim after the author, e.g. `claude`.
    pub tag: Option<&'a str>,
    /// Shown dim after the time, e.g. [`STATUS_NOTE`].
    pub note: Option<&'static str>,
    pub body: Body<'a>,
    /// Attached files and images (paths).
    pub files: Vec<&'a str>,
    /// The agent's folder, against which the post's file mentions resolve.
    pub cwd: Option<&'a str>,
    /// First post of its day: draw a day divider above it.
    pub new_day: bool,
    /// Starts an author group: draw the author header above it.
    pub new_group: bool,
}

/// A status channel: messages posted there and agents' latest replies.
pub fn log_posts<'a>(msgs: &'a [Msg], chan: &Channel) -> Vec<Post<'a>> {
    let mut visible: Vec<&Msg> = msgs.iter().filter(|m| channels::visible(m, &chan.id)).collect();
    // Statuses carry their transcript time, so order by time, not arrival.
    visible.sort_by_key(|m| (m.ts, m.id));
    let posts = visible[visible.len().saturating_sub(LIMIT)..].iter().map(|m| {
        let status = m.kind == "status";
        let mut p = post(
            format!("m{}", m.id),
            m.ts,
            &m.sender,
            &m.sender_kind,
            (m.sender_kind != "user").then_some(m.sender_kind.as_str()),
            status.then_some(STATUS_NOTE),
            Body::Markdown { text: &m.body, clamp: if status { Clamp::ToDm } else { Clamp::Off } },
        );
        p.files = m.files.iter().map(String::as_str).collect();
        p
    });
    arrange(posts.collect())
}

/// A DM: the agent's transcript, prompts and replies in full, tools as
/// one-liners, with its subagents' activity merged in by time, and each
/// turn's file changes gathered at its end.
pub fn transcript_posts<'a>(tail: &'a Tail, agent: &'a str, kind: Kind, me: &'a str) -> Vec<Post<'a>> {
    let main = tail.entries();
    let main = &main[main.len().saturating_sub(LIMIT * 2)..];
    let since = main.first().map_or(0, |e| e.ts);
    // (entry, author, author kind, from a subagent)
    let mut entries: Vec<(&Entry, &str, &str, bool)> = main.iter().map(|e| (e, agent, kind.as_str(), false)).collect();
    for sub in &tail.subs {
        entries.extend(sub.entries().filter(|e| e.ts >= since).map(|e| (e, sub.label.as_str(), "subagent", true)));
    }
    entries.sort_by_key(|(e, ..)| e.ts);

    let mut posts = Vec::new();
    let mut turn: Vec<(i64, &Change)> = Vec::new();
    let changes_post = |turn: &[(i64, &'a Change)]| {
        let key = format!("{agent}:changes:{}", turn[0].0);
        let ts = turn.last().map_or(0, |(ts, _)| *ts);
        post(key, ts, agent, kind.as_str(), Some(kind.as_str()), None, Body::Changes(Changed::merge(turn)))
    };
    for (e, author, author_kind, sub) in entries {
        match &e.item {
            Item::Change(c) => turn.push((e.ts, c)),
            // A prompt to the agent starts a new turn.
            Item::Prompt { .. } if !sub && !turn.is_empty() => {
                posts.push(changes_post(&turn));
                turn.clear();
                posts.push(entry_post(e, author, author_kind, me, sub));
            }
            _ => posts.push(entry_post(e, author, author_kind, me, sub)),
        }
    }
    if !turn.is_empty() {
        posts.push(changes_post(&turn));
    }
    arrange(posts)
}

/// A post for a transcript entry other than a change.
fn entry_post<'a>(e: &'a Entry, author: &'a str, kind: &'a str, me: &'a str, sub: bool) -> Post<'a> {
    let key = format!("{author}:{}", e.key);
    let tag = Some(if sub { "subagent" } else { kind });
    match &e.item {
        Item::Prompt { from, text, files } => {
            let author_kind = match from {
                Some(f) if !f.ends_with("via agentrelay") => "peer",
                _ => "user",
            };
            let body = Body::Markdown { text, clamp: Clamp::Off };
            let mut p = post(key, e.ts, from.as_deref().unwrap_or(me), author_kind, None, None, body);
            p.files = files.iter().map(String::as_str).collect();
            p
        }
        Item::Reply(text) => {
            let clamp = if sub { Clamp::Expand } else { Clamp::Off };
            post(key, e.ts, author, kind, tag, None, Body::Markdown { text, clamp })
        }
        Item::Tool { name, detail, full, path, error, files } => {
            let body = Body::Tool { name, detail, full, path: path.as_deref(), error: *error };
            let mut p = post(key, e.ts, author, kind, tag, None, body);
            p.files = files.iter().map(String::as_str).collect();
            p
        }
        Item::Change(_) => unreachable!("changes are gathered per turn"),
    }
}

impl Post<'_> {
    /// Activity from one of the agent's subagents rather than the agent itself.
    pub fn is_subagent(&self) -> bool {
        self.author_kind == "subagent"
    }
}

fn post<'a>(
    key: String,
    ts: i64,
    author: &'a str,
    author_kind: &'a str,
    tag: Option<&'a str>,
    note: Option<&'static str>,
    body: Body<'a>,
) -> Post<'a> {
    Post { key, ts, author, author_kind, tag, note, body, files: Vec::new(), cwd: None, new_day: false, new_group: false }
}

/// Sets the day and group breaks: a header for each author run, broken by
/// noted posts, a new day, or five quiet minutes.
fn arrange(mut posts: Vec<Post>) -> Vec<Post> {
    for i in 0..posts.len() {
        let (before, rest) = posts.split_at_mut(i);
        let p = &mut rest[0];
        let prev = before.last();
        p.new_day = prev.is_none_or(|q| day(q.ts) != day(p.ts));
        p.new_group = p.new_day
            || prev.is_none_or(|q| {
                q.author != p.author || q.note.is_some() || p.note.is_some() || p.ts - q.ts >= GROUP_GAP_MS
            });
    }
    posts
}

fn local(ts: i64) -> chrono::DateTime<Local> {
    Local.timestamp_millis_opt(ts).single().unwrap_or_else(Local::now)
}

fn day(ts: i64) -> chrono::NaiveDate {
    local(ts).date_naive()
}

/// `Today`, or `Monday, October 12`.
pub fn day_label(ts: i64) -> String {
    if day(ts) == Local::now().date_naive() {
        "Today".into()
    } else {
        local(ts).format("%A, %B %-d").to_string()
    }
}

/// `6:24 PM`.
pub fn clock(ts: i64) -> String {
    local(ts).format("%-I:%M %p").to_string()
}
