//! Drawing a hub feed as terminal lines: day dividers, author headers, bodies.

use super::markdown;
use crate::hub::feed::{self, Body, Post};
use crate::{theme, uploads};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use std::collections::HashMap;
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

pub fn layout(posts: &[Post], width: usize, cache: &mut Cache) -> Vec<Line<'static>> {
    let body_width = width.saturating_sub(2).max(16);
    let mut lines = Vec::new();
    for p in posts {
        if p.new_day {
            lines.push(Line::default());
            lines.push(day_divider(&feed::day_label(p.ts), width));
        }
        if p.new_group {
            lines.push(Line::default());
            lines.push(header(p));
        }
        let body = cache.get(&p.key, body_width, || render_body(&p.body, body_width));
        lines.extend(body.iter().map(|l| indent(l.clone())));
        for f in p.files {
            let name = uploads::display_name(std::path::Path::new(f));
            lines.push(indent(Line::styled(format!("📎 {name}"), Style::new().fg(theme::LINK))));
        }
    }
    lines
}

fn day_divider(label: &str, width: usize) -> Line<'static> {
    let side = "─".repeat(width.saturating_sub(label.width() + 2) / 2);
    let dim = Style::new().fg(theme::DIM);
    Line::from(vec![
        Span::styled(side.clone(), dim),
        Span::styled(format!(" {label} "), dim.bold()),
        Span::styled(side, dim),
    ])
}

fn header(p: &Post) -> Line<'static> {
    let dim = Style::new().fg(theme::DIM);
    let mut spans = vec![
        Span::raw(" "),
        Span::styled(p.author.to_string(), Style::new().fg(theme::kind_color(p.author_kind)).bold()),
    ];
    if let Some(tag) = p.tag {
        spans.push(Span::styled(format!("  {tag}"), dim));
    }
    spans.push(Span::styled(format!("  {}", feed::clock(p.ts)), dim));
    if let Some(note) = p.note {
        spans.push(Span::styled(format!("  {note}"), dim.italic()));
    }
    Line::from(spans)
}

fn indent(mut l: Line<'static>) -> Line<'static> {
    l.spans.insert(0, Span::raw(" "));
    l
}

fn render_body(body: &Body, width: usize) -> Vec<Line<'static>> {
    match body {
        Body::Markdown { text, clamp } => {
            let mut lines = markdown::render(text, width);
            if *clamp && lines.len() > feed::STATUS_LINES {
                lines.truncate(feed::STATUS_LINES);
                lines.push(Line::styled("… open the DM for the full reply", Style::new().fg(theme::DIM).italic()));
            }
            lines
        }
        Body::Tool { name, detail, error } => {
            let (mark, color) = if *error { ("✗ ", theme::ERROR) } else { ("▸ ", theme::DIM) };
            let mut detail = detail.to_string();
            let room = width.saturating_sub(name.width() + 4);
            if detail.width() > room {
                detail = detail.chars().take(room.saturating_sub(1)).collect::<String>() + "…";
            }
            vec![Line::from(vec![
                Span::styled(mark, Style::new().fg(color)),
                Span::styled(name.to_string(), Style::new().fg(theme::DIM).bold()),
                Span::styled(format!("  {detail}"), Style::new().fg(theme::DIM)),
            ])]
        }
    }
}
