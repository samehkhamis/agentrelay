//! Drawing: sidebar, channel header, feed and composer.

use super::{feed, App};
use crate::agents::{Kind, Presence};
use crate::hub::channels::{Channel, Section};
use crate::{theme, uploads};
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::Frame;
use std::time::Duration;
use unicode_width::UnicodeWidthStr;

const SIDEBAR_WIDTH: u16 = 30;
const TOAST_FOR: Duration = Duration::from_secs(4);

fn presence_dot(p: Option<Presence>) -> Span<'static> {
    match p {
        Some(Presence::Busy) => Span::styled("● ", Style::new().fg(theme::t().busy)),
        Some(Presence::Online) => Span::styled("● ", Style::new().fg(theme::t().online)),
        _ => Span::styled("○ ", Style::new().fg(theme::t().sidebar_dim)),
    }
}

impl App {
    pub(super) fn draw(&mut self, f: &mut Frame) {
        let [side, main] = Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(20)]).areas(f.area());
        let [header, body, composer] =
            Layout::vertical([Constraint::Length(2), Constraint::Min(1), Constraint::Length(3)]).areas(main);
        self.sidebar_area = side;
        self.feed_area = body;
        let th = theme::t();
        f.render_widget(Block::new().style(Style::new().bg(th.bg).fg(th.fg)), main);
        self.draw_sidebar(f, side);
        self.draw_header(f, header);
        self.draw_feed(f, body);
        self.draw_composer(f, composer);
        // Reading the bottom of a channel marks it read, in every UI.
        if self.scroll == 0 {
            let id = self.selected.clone();
            self.hub.mark_seen(&id);
        }
    }

    // ------------------------------------------------------------ sidebar

    fn draw_sidebar(&mut self, f: &mut Frame, area: Rect) {
        f.render_widget(Block::new().style(Style::new().bg(theme::t().sidebar_bg)), area);
        let width = area.width.saturating_sub(1) as usize;
        let dim = Style::new().fg(theme::t().sidebar_dim);
        let online = self.hub.online();
        let mut rows: Vec<(Line, Option<&str>)> = vec![
            (Line::styled(" agentchat", Style::new().fg(Color::White).bold()), None),
            (
                Line::from(vec![
                    Span::styled(" ● ", Style::new().fg(theme::t().online)),
                    Span::styled(format!("{} · {online} online", self.hub.me), dim),
                ]),
                None,
            ),
        ];
        let mut section = None;
        for c in &self.hub.channels {
            if section != Some(c.section) {
                section = Some(c.section);
                let title = match c.section {
                    Section::Top => "Channels",
                    Section::Projects => "Projects",
                    Section::Direct => "Direct messages",
                };
                rows.push((Line::default(), None));
                rows.push((Line::styled(format!(" ▾ {title}"), dim), None));
            }
            rows.push((self.sidebar_row(c, width), Some(c.id.as_str())));
        }
        rows.push((Line::default(), None));
        rows.push((Line::styled(" Tab switch · Ctrl+T theme", dim), None));

        // Keep the selected row on screen.
        let h = area.height as usize;
        let sel = rows.iter().position(|(_, id)| *id == Some(self.selected.as_str())).unwrap_or(0);
        let offset = (sel + 3).saturating_sub(h).min(rows.len().saturating_sub(h));
        let mut clickable = Vec::new();
        let mut visible = Vec::new();
        for (y, (line, id)) in rows.into_iter().skip(offset).take(h).enumerate() {
            if let Some(id) = id {
                clickable.push((area.y + y as u16, id.to_string()));
            }
            visible.push(line);
        }
        self.sidebar_rows = clickable;
        f.render_widget(Paragraph::new(visible), area);
    }

    fn sidebar_row(&self, c: &Channel, width: usize) -> Line<'static> {
        let selected = c.id == self.selected;
        let unread = if selected { 0 } else { self.hub.unread(c) };
        let base = if selected {
            Style::new().bg(theme::t().selected_bg).fg(Color::White)
        } else if unread > 0 {
            Style::new().fg(Color::White).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme::t().sidebar_fg)
        };
        let lead = match c.section {
            Section::Direct if selected => presence_dot(c.presence).bg(theme::t().selected_bg),
            Section::Direct => presence_dot(c.presence),
            _ => Span::styled("# ", base),
        };
        let (right, right_style) = if unread > 0 {
            (format!(" {unread} "), Style::new().bg(theme::t().badge_bg).fg(Color::White).bold())
        } else {
            let text = match c.section {
                Section::Direct => format!("{} ", c.kind.unwrap_or(Kind::Claude).as_str()),
                Section::Projects => format!("{} ", self.hub.members(c).len()),
                Section::Top => String::new(),
            };
            let color = c.kind.map_or(theme::t().sidebar_dim, |k| theme::kind_color(k.as_str()));
            (text, if selected { base } else { Style::new().fg(color) })
        };
        let used = 4 + right.width();
        let mut name = c.name().to_string();
        if name.width() + used + 1 > width {
            name = name.chars().take(width.saturating_sub(used + 2)).collect::<String>() + "…";
        }
        let pad = width.saturating_sub(used + name.width());
        Line::from(vec![
            Span::styled("  ", base),
            lead,
            Span::styled(name, base),
            Span::styled(" ".repeat(pad), base),
            Span::styled(right, right_style),
            Span::styled(" ", base),
        ])
    }

    // ------------------------------------------------------------ header

    fn draw_header(&self, f: &mut Frame, area: Rect) {
        let c = self.current();
        let dim = Style::new().fg(theme::t().dim);
        let mut line = match c.section {
            Section::Direct => Line::from(vec![presence_dot(c.presence), Span::raw(c.name().to_string()).bold()]),
            _ => Line::from(Span::raw(format!(" {}", c.title())).bold()),
        };
        line.push_span(Span::styled(format!("  {}", self.hub.detail(c)), dim));
        if let Some((t, _)) = self.toast.as_ref().filter(|(_, at)| at.elapsed() < TOAST_FOR) {
            let pad = (area.width as usize).saturating_sub(line.width() + t.width() + 3);
            line.push_span(Span::raw(" ".repeat(pad)));
            line.push_span(Span::styled(format!("{t} "), Style::new().fg(theme::t().user).italic()));
        }
        let block = Block::new().borders(Borders::BOTTOM).border_style(dim);
        f.render_widget(Paragraph::new(line).block(block), area);
    }

    // ------------------------------------------------------------ feed

    fn draw_feed(&mut self, f: &mut Frame, area: Rect) {
        let chan = self.current().clone();
        let posts = self.hub.posts(&chan);
        let (mut lines, slots) = if posts.is_empty() {
            (vec![Line::default(), Line::styled(empty_hint(&chan), Style::new().fg(theme::t().dim))], Vec::new())
        } else {
            feed::layout(&posts, area.width.saturating_sub(1) as usize, &mut self.cache, self.images.as_mut())
        };
        drop(posts);

        let h = area.height as usize;
        self.feed_height = h;
        self.scroll = self.scroll.min(lines.len().saturating_sub(h));
        let start = lines.len().saturating_sub(h + self.scroll);
        let end = (start + h).min(lines.len());
        f.render_widget(Paragraph::new(lines.drain(start..end).collect::<Vec<_>>()), area);
        // Images go into their reserved rows when those are fully on screen.
        if let Some(images) = self.images.as_mut() {
            for s in slots.iter().filter(|s| s.line >= start && s.line + s.rows as usize <= end) {
                let x = area.x + feed::BODY_X;
                let rect = Rect::new(x, area.y + (s.line - start) as u16, s.cols.min(area.right() - x), s.rows);
                images.draw(f, rect, &s.path);
            }
        }
        if self.scroll > 0 {
            let note = format!(" ↓ {} more lines · Esc to jump down ", self.scroll);
            let w = note.width() as u16;
            let r = Rect::new(area.right().saturating_sub(w + 1), area.bottom() - 1, w, 1);
            f.render_widget(Paragraph::new(note).style(Style::new().bg(theme::t().selected_bg).fg(Color::White)), r);
        }
    }

    // ------------------------------------------------------------ composer

    fn draw_composer(&self, f: &mut Frame, area: Rect) {
        let mut block = Block::bordered().border_type(BorderType::Rounded).border_style(Style::new().fg(theme::t().dim));
        if !self.pending.is_empty() {
            let names: Vec<String> = self.pending.iter().map(|p| uploads::display_name(p)).collect();
            let title = format!(" 📎 {} ", names.join("  "));
            block = block.title(Span::styled(title, Style::new().fg(theme::t().link)));
        }
        let inner = block.inner(area);
        let (shown, cursor) = self.editor.view(inner.width as usize);
        let para = if self.editor.is_empty() {
            Paragraph::new(Span::styled(format!("Message {}", self.current().title()), Style::new().fg(theme::t().dim)))
        } else {
            Paragraph::new(shown)
        };
        f.render_widget(para.block(block), area);
        let x = (inner.x + cursor as u16).min(inner.right().saturating_sub(1));
        f.set_cursor_position(Position::new(x, inner.y));
    }
}

fn empty_hint(chan: &Channel) -> String {
    match chan.section {
        Section::Direct => format!(" Nothing from {} yet.", chan.name()),
        _ => format!(" This is the start of {}. Type /status to ask everyone here for an update.", chan.title()),
    }
}
