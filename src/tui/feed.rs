//! Drawing a hub feed as terminal lines: day dividers, author headers, bodies.

use super::images::Images;
use super::markdown;
use crate::hub::feed::{self, Body, Post};
use crate::{files, theme, uploads};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::collections::{HashMap, HashSet};
use unicode_width::UnicodeWidthStr;

/// Rendered post bodies by (key, width), since markdown and highlighting are
/// too slow to redo every frame.
#[derive(Default)]
pub struct Cache(HashMap<(String, usize), Vec<Line<'static>>>);

impl Cache {
    fn get(&mut self, key: &str, width: usize, render: impl FnOnce() -> Vec<Line<'static>>) -> &[Line<'static>] {
        if self.0.len() > 20_000 {
            self.0.clear();
        }
        self.0.entry((key.to_string(), width)).or_insert_with(render)
    }
}

/// Rows reserved in the laid-out lines for an image, drawn after the text.
pub struct Slot {
    /// Index of the first reserved line.
    pub line: usize,
    pub cols: u16,
    pub rows: u16,
    pub path: String,
}

/// The column where post bodies (and images) start: after the gutter.
pub const BODY_X: u16 = 2;

/// A tool call's line, for clicks: expand it, or open its file.
#[derive(Clone)]
pub struct ToolRow {
    pub key: String,
    pub file: Option<String>,
    pub expandable: bool,
}

/// A laid-out feed: its lines, rows reserved for images, the tool call
/// lines (by line index), and each post's line range (for keyboard selection).
pub struct Layout {
    pub lines: Vec<Line<'static>>,
    pub slots: Vec<Slot>,
    pub tools: Vec<(usize, ToolRow)>,
    pub items: Vec<(std::ops::Range<usize>, String)>,
}

pub fn layout(
    posts: &[Post],
    width: usize,
    cache: &mut Cache,
    mut images: Option<&mut Images>,
    expanded: &HashSet<String>,
) -> Layout {
    let body_width = width.saturating_sub(3).max(16);
    let mut lines = Vec::new();
    let mut slots = Vec::new();
    let mut tools = Vec::new();
    let mut items = Vec::new();
    for p in posts {
        if p.new_day {
            lines.push(Line::default());
            lines.push(day_divider(&feed::day_label(p.ts), width));
        }
        if p.new_group {
            lines.push(Line::default());
            lines.push(gutter(p, header(p)));
        }
        let open = expanded.contains(&p.key);
        let item_start = lines.len();
        if let Body::Tool { full, path, .. } = &p.body {
            let file = path.filter(|f| files::resolve(p.cwd.unwrap_or(""), f).is_some()).map(str::to_string);
            tools.push((lines.len(), ToolRow { key: p.key.clone(), file, expandable: !full.is_empty() }));
        }
        if let Body::Changes(changed) = &p.body {
            let first = lines.len();
            let (rows, region) = changes(changed, p, open, body_width, cache);
            tools.extend(rows.into_iter().map(|(at, row)| (first + at, row)));
            lines.extend(region.into_iter().map(|l| gutter(p, l)));
            items.push((item_start..lines.len(), p.key.clone()));
            continue;
        }
        let is_file = |c: &str| files::resolve(p.cwd.unwrap_or(""), c).is_some();
        let body = cache.get(&p.key, body_width, || render_body(&p.body, body_width, &is_file));
        let first = lines.len();
        lines.extend(body.iter().map(|l| gutter(p, l.clone())));
        if open {
            // The tool line's mark: ▸ collapsed, ▾ expanded.
            if let Some(mark) = lines[first].spans.iter_mut().find(|s| s.content == "▸ ") {
                mark.content = "▾ ".into();
            }
        }
        if let (true, Body::Tool { full, .. }) = (open, &p.body) {
            let key = format!("{}:full", p.key);
            let detail = cache.get(&key, body_width, || markdown::render(full, body_width.saturating_sub(2), &is_file));
            lines.extend(detail.iter().map(|l| gutter(p, indent(l.clone()))));
        }
        for f in &p.files {
            let name = uploads::display_name(std::path::Path::new(f));
            lines.push(gutter(p, Line::styled(format!("📎 {name}"), Style::new().fg(theme::t().link))));
            if let Some((cols, rows)) = images.as_deref_mut().and_then(|im| im.size(f, body_width as u16)) {
                slots.push(Slot { line: lines.len(), cols, rows, path: f.to_string() });
                lines.extend((0..rows).map(|_| gutter(p, Line::default())));
            }
        }
        items.push((item_start..lines.len(), p.key.clone()));
    }
    Layout { lines, slots, tools, items }
}

/// Most diff lines shown per file; the file itself is a click away.
const DIFF_LINES: usize = 400;

/// A turn's changed files as a marked region: a header that shows or hides
/// the diffs, and a row per file that opens it. Returns the click targets
/// (by line within the region) and the region's lines.
fn changes(changed: &[feed::Changed], p: &Post, open: bool, width: usize, cache: &mut Cache) -> (Vec<(usize, ToolRow)>, Vec<Line<'static>>) {
    use crate::transcript::ChangeOp;
    let th = theme::t();
    let bar = || Span::styled("┃ ", Style::new().fg(th.link));
    let dim = Style::new().fg(th.dim);
    let mut lines = vec![Line::from(vec![
        bar(),
        Span::styled("Files changed", Style::new().fg(th.fg).bold()),
        Span::styled(format!("  {}  {}", changed.len(), if open { "▾ diffs" } else { "▸ diffs" }), dim),
    ])];
    let mut rows = Vec::new();
    let edits: usize = changed.iter().map(|f| f.diffs.len()).sum();
    for (i, f) in changed.iter().enumerate() {
        let gone = f.op == ChangeOp::Deleted;
        let name = Style::new().fg(if gone { th.dim } else { th.link });
        let note = match f.op {
            ChangeOp::Added => "  new",
            ChangeOp::Deleted => "  deleted",
            ChangeOp::Updated => "",
        };
        let file = (!gone).then(|| f.path.to_string());
        rows.push((lines.len(), ToolRow { key: p.key.clone(), file, expandable: false }));
        lines.push(Line::from(vec![
            bar(),
            Span::styled(format!("+{:<5}", f.added), Style::new().fg(th.online)),
            Span::styled(format!("−{:<5}", f.removed), Style::new().fg(th.error)),
            Span::styled(crate::files::display_path(f.path, p.cwd), if gone { name } else { name.add_modifier(Modifier::UNDERLINED) }),
            Span::styled(note, dim),
        ]));
        if open {
            let key = format!("{}:{i}:{edits}", p.key);
            let diff = cache.get(&key, width, || {
                let text: Vec<&str> = f.diffs.iter().flat_map(|d| d.lines()).collect();
                let mut out: Vec<Line<'static>> = crate::highlight::lines("diff", &text[..text.len().min(DIFF_LINES)].join("\n"))
                    .into_iter()
                    .map(|tokens| Line::from(tokens.into_iter().map(|((r, g, b), t)| Span::styled(t, Style::new().fg(ratatui::style::Color::Rgb(r, g, b)))).collect::<Vec<_>>()))
                    .collect();
                if text.len() > DIFF_LINES {
                    out.push(Line::styled(format!("… {} more lines", text.len() - DIFF_LINES), Style::new().fg(theme::t().dim)));
                }
                out
            });
            lines.extend(diff.iter().map(|l| {
                let mut l = l.clone();
                l.spans.splice(0..0, [bar(), Span::raw("  ")]);
                l
            }));
        }
    }
    // The header last, so keyboard actions on the post toggle the diffs.
    rows.push((0, ToolRow { key: p.key.clone(), file: None, expandable: true }));
    (rows, lines)
}

fn indent(mut l: Line<'static>) -> Line<'static> {
    l.spans.insert(0, Span::raw("  "));
    l
}

fn day_divider(label: &str, width: usize) -> Line<'static> {
    let side = "─".repeat(width.saturating_sub(label.width() + 2) / 2);
    let dim = Style::new().fg(theme::t().dim);
    Line::from(vec![
        Span::styled(side.clone(), dim),
        Span::styled(format!(" {label} "), dim.bold()),
        Span::styled(side, dim),
    ])
}

fn header(p: &Post) -> Line<'static> {
    let dim = Style::new().fg(theme::t().dim);
    let arrow = if p.is_subagent() { "↳ " } else { "" };
    let mut spans = vec![Span::styled(
        format!("{arrow}{}", p.author),
        Style::new().fg(theme::kind_color(p.author_kind)).bold(),
    )];
    if let Some(tag) = p.tag {
        spans.push(Span::styled(format!("  {tag}"), dim));
    }
    spans.push(Span::styled(format!("  {}", feed::clock(p.ts)), dim));
    if let Some(note) = p.note {
        spans.push(Span::styled(format!("  {note}"), dim.italic()));
    }
    Line::from(spans)
}

/// The left margin: a colored bar for subagent activity, so it stands apart
/// from the agent's own.
fn gutter(p: &Post, mut l: Line<'static>) -> Line<'static> {
    let mark = if p.is_subagent() { Span::styled("▍ ", Style::new().fg(theme::t().subagent)) } else { Span::raw("  ") };
    l.spans.insert(0, mark);
    l
}

fn render_body(body: &Body, width: usize, is_file: &dyn Fn(&str) -> bool) -> Vec<Line<'static>> {
    match body {
        Body::Markdown { text, clamp } => {
            let mut lines = markdown::render(text, width, is_file);
            if let Some(hint) = clamp.hint().filter(|_| lines.len() > feed::STATUS_LINES) {
                lines.truncate(feed::STATUS_LINES);
                lines.push(Line::styled(hint, Style::new().fg(theme::t().dim).italic()));
            }
            lines
        }
        Body::Changes(_) => Vec::new(), // laid out by `changes`
        Body::Tool { name, detail, full, path, error } => {
            // ▸ collapsed, ▾ expanded (drawn by layout), ✗ failed; · nothing to expand.
            let mark = match (*error, full.is_empty()) {
                (true, _) => "✗ ",
                (false, true) => "· ",
                (false, false) => "▸ ",
            };
            let color = if *error { theme::t().error } else { theme::t().dim };
            let mut detail = detail.to_string();
            let room = width.saturating_sub(name.width() + 4);
            if detail.width() > room {
                detail = detail.chars().take(room.saturating_sub(1)).collect::<String>() + "…";
            }
            vec![Line::from(vec![
                Span::styled(mark, Style::new().fg(color)),
                Span::styled(name.to_string(), Style::new().fg(theme::t().dim).bold()),
                Span::raw("  "),
                // A file the tool worked on reads as a link, like mentions in text.
                match path {
                    Some(_) => Span::styled(detail, Style::new().fg(theme::t().link).add_modifier(Modifier::UNDERLINED)),
                    None => Span::styled(detail, Style::new().fg(theme::t().dim)),
                },
            ])]
        }
    }
}
