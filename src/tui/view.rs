//! Drawing: sidebar, channel header, feed and composer.

use super::{feed, App};
use crate::agents::{Kind, Presence};
use crate::hub::channels::{Channel, Section};
use crate::{theme, uploads};
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;
use std::time::Duration;
use unicode_width::UnicodeWidthStr;

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
        let [side, main] = Layout::horizontal([Constraint::Length(self.widths.sidebar), Constraint::Min(20)]).areas(f.area());
        let [header, body, composer] =
            Layout::vertical([Constraint::Length(3), Constraint::Min(1), Constraint::Length(3)]).areas(main);
        self.sidebar_area = side;
        // An open file takes the right part of the feed.
        let (body, pane) = match self.viewer {
            Some(_) => {
                let pane = self.widths.pane.unwrap_or(body.width * 45 / 100);
                let [left, right] = Layout::horizontal([Constraint::Min(20), Constraint::Length(pane)]).areas(body);
                (left, Some(right))
            }
            None => (body, None),
        };
        self.feed_area = body;
        let th = theme::t();
        f.render_widget(Block::new().style(Style::new().bg(th.bg).fg(th.fg)), main);
        self.draw_sidebar(f, side);
        self.draw_header(f, header);
        self.draw_feed(f, body);
        self.viewer_area = Rect::default();
        if let (Some(v), Some(pane)) = (self.viewer.as_mut(), pane) {
            v.draw(f, pane, self.focus == super::Focus::File);
            self.viewer_area = pane;
        }
        self.draw_composer(f, composer);
        self.draw_suggestions(f, composer);
        self.draw_picker(f, composer);
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
            (Line::styled(" agentrelay", Style::new().fg(Color::White).bold()), None),
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
        rows.push((Line::styled("  ＋ new agent", Style::new().fg(theme::t().sidebar_fg)), Some(super::NEW_AGENT)));
        rows.push((Line::default(), None));
        rows.push((Line::styled(" Alt+h/l panes · ! shell · ^O files", dim), None));

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
        let base = if selected && self.focus == super::Focus::Sidebar {
            Style::new().bg(theme::t().link).fg(theme::t().sidebar_bg).add_modifier(Modifier::BOLD)
        } else if selected {
            Style::new().bg(theme::t().selected_bg).fg(Color::White)
        } else if unread > 0 {
            Style::new().fg(Color::White).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme::t().sidebar_fg)
        };
        let lead = match c.section {
            Section::Direct if selected => presence_dot(c.presence).bg(base.bg.unwrap_or(theme::t().selected_bg)),
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

    /// Two lines: the channel or agent with its conversation title, then details.
    fn draw_header(&self, f: &mut Frame, area: Rect) {
        let c = self.current();
        let th = theme::t();
        let dim = Style::new().fg(th.dim);
        let mut title = match c.section {
            Section::Direct => Line::from(vec![Span::raw(" "), presence_dot(c.presence), Span::raw(c.name().to_string()).bold()]),
            _ => Line::from(Span::raw(format!(" {}", c.title())).bold()),
        };
        if let Some(headline) = self.hub.headline(c) {
            title.push_span(Span::styled("  ·  ", dim));
            title.push_span(Span::styled(headline, Style::new().fg(th.fg).bold()));
        }
        if let Some((t, _)) = self.toast.as_ref().filter(|(_, at)| at.elapsed() < TOAST_FOR) {
            let pad = (area.width as usize).saturating_sub(title.width() + t.width() + 3);
            title.push_span(Span::raw(" ".repeat(pad)));
            title.push_span(Span::styled(format!("{t} "), Style::new().fg(th.user).italic()));
        }
        let detail = Line::styled(format!(" {}", self.hub.detail(c)), dim);
        let block = Block::new().borders(Borders::BOTTOM).border_style(dim);
        f.render_widget(Paragraph::new(vec![title, detail]).block(block), area);
    }

    // ------------------------------------------------------------ feed

    fn draw_feed(&mut self, f: &mut Frame, area: Rect) {
        let chan = self.current().clone();
        let posts = self.hub.posts(&chan);
        let feed::Layout { mut lines, slots, tools, items } = if posts.is_empty() {
            let hint = Line::styled(format!(" {}", self.hub.empty_hint(&chan)), Style::new().fg(theme::t().dim));
            feed::Layout { lines: vec![Line::default(), hint], slots: Vec::new(), tools: Vec::new(), items: Vec::new() }
        } else {
            let width = area.width.saturating_sub(1) as usize;
            feed::layout(&posts, width, &mut self.cache, self.images.as_mut(), &self.expanded)
        };
        drop(posts);

        let h = area.height as usize;
        self.feed_height = h;
        // Keyboard selection: bring a newly selected post into view (from its
        // top if it's taller than the screen), then leave scrolling to the user.
        if let Some((range, _)) = items.iter().find(|(_, k)| Some(k) == self.cursor.as_ref()) {
            if self.cursor_placed != self.cursor {
                self.cursor_placed = self.cursor.clone();
                let total = lines.len();
                let top = total.saturating_sub(h + self.scroll);
                if range.start < top || range.len() > h {
                    self.scroll = total.saturating_sub(h + range.start);
                } else if range.end > top + h {
                    self.scroll = total.saturating_sub(range.end);
                }
            }
            let bar = Span::styled("▌ ", Style::new().fg(theme::t().link));
            for line in &mut lines[range.clone()] {
                match line.spans.first_mut() {
                    Some(first) => *first = bar.clone(),
                    None => line.spans.push(bar.clone()),
                }
            }
        }
        self.feed_items = items.into_iter().map(|(_, k)| k).collect();
        self.scroll = self.scroll.min(lines.len().saturating_sub(h));
        let start = lines.len().saturating_sub(h + self.scroll);
        let end = (start + h).min(lines.len());
        f.render_widget(Paragraph::new(lines.drain(start..end).collect::<Vec<_>>()), area);
        self.tool_rows = tools
            .iter()
            .filter(|(line, _)| (start..end).contains(line))
            .map(|(line, t)| (area.y + (line - start) as u16, t.clone()))
            .collect();
        self.feed_tools = tools.iter().filter(|(_, t)| t.expandable).map(|(_, t)| t.key.clone()).collect();
        self.tool_info = tools.into_iter().map(|(_, t)| (t.key.clone(), t)).collect();
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
        let th = theme::t();
        let shell = self.editor.shell;
        let mut block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(if shell { th.code } else { th.dim }));
        if !self.pending.is_empty() {
            let names: Vec<String> = self.pending.iter().map(|p| uploads::display_name(p)).collect();
            let title = format!(" 📎 {} ", names.join("  "));
            block = block.title(Span::styled(title, Style::new().fg(th.link)));
        }
        let inner = block.inner(area);
        f.render_widget(block, area);
        // Shell mode: a `$` marker before the command, in the shell color.
        let marker = if shell { 2 } else { 0 };
        if shell {
            f.render_widget(Span::styled("$ ", Style::new().fg(th.code).bold()), inner);
        }
        let text_area = Rect { x: inner.x + marker, width: inner.width.saturating_sub(marker), ..inner };
        let (shown, cursor) = self.editor.view(text_area.width as usize);
        let hint = self.hub.placeholder(self.current(), shell);
        let para = if shown.is_empty() {
            Paragraph::new(Span::styled(hint, Style::new().fg(th.dim)))
        } else {
            Paragraph::new(shown)
        };
        f.render_widget(para, text_area);
        let x = (text_area.x + cursor as u16).min(text_area.right().saturating_sub(1));
        f.set_cursor_position(Position::new(x, text_area.y));
    }
}

impl App {
    /// The Ctrl+O list of files the feed mentions, just above the composer.
    fn draw_picker(&self, f: &mut Frame, composer: Rect) {
        let Some((items, sel)) = &self.picker else { return };
        let th = theme::t();
        let shown = items.len().min(10);
        let first = sel.saturating_sub(shown - 1);
        let height = shown as u16 + 2;
        let area = Rect::new(composer.x, composer.y.saturating_sub(height), composer.width.min(90), height);
        let rows: Vec<Line> = items
            .iter()
            .enumerate()
            .skip(first)
            .take(shown)
            .map(|(i, raw)| {
                let style = if i == *sel { Style::new().bg(th.selected_bg).fg(Color::White) } else { Style::new().fg(th.link) };
                Line::styled(format!(" {raw} "), style)
            })
            .collect();
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(th.dim))
            .title(Span::styled(" files mentioned · Enter opens · Esc ", Style::new().fg(th.dim)))
            .style(Style::new().bg(th.bg).fg(th.fg));
        f.render_widget(Clear, area);
        f.render_widget(Paragraph::new(rows).block(block), area);
    }

    /// The autocomplete list, just above the composer.
    fn draw_suggestions(&self, f: &mut Frame, composer: Rect) {
        let list = self.suggestions();
        if list.is_empty() {
            return;
        }
        let th = theme::t();
        let shown = list.len().min(8);
        let first = self.suggestion.saturating_sub(shown - 1);
        let width = composer.width.min(80);
        let height = shown as u16 + 2;
        let area = Rect::new(composer.x, composer.y.saturating_sub(height), width, height);
        let rows: Vec<Line> = list
            .iter()
            .enumerate()
            .skip(first)
            .take(shown)
            .map(|(i, c)| {
                let base = if i == self.suggestion { Style::new().bg(th.selected_bg).fg(Color::White) } else { Style::new() };
                let pad = (width as usize).saturating_sub(c.label.width() + c.detail.width() + 4);
                Line::from(vec![
                    Span::styled(format!(" {}", c.label), base.bold()),
                    Span::styled(" ".repeat(pad), base),
                    Span::styled(format!("{} ", c.detail), base.fg(if i == self.suggestion { Color::White } else { th.dim })),
                ])
            })
            .collect();
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(th.dim))
            .title(Span::styled(" ↑↓ choose · Tab/Enter accept · Esc ", Style::new().fg(th.dim)))
            .style(Style::new().bg(th.bg).fg(th.fg));
        f.render_widget(Clear, area);
        f.render_widget(Paragraph::new(rows).block(block), area);
    }
}
