//! The file pane: a file a feed mentions, highlighted, with line numbers,
//! opened at the mentioned line.

use crate::files::{self, FileRef};
use crate::{db, highlight, theme};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

pub struct Viewer {
    title: String,
    lines: Vec<Line<'static>>,
    pub scroll: usize,
}

impl Viewer {
    pub fn new(file: &FileRef, text: &str) -> Viewer {
        let th = theme::t();
        let (bg_r, bg_g, bg_b) = highlight::background();
        let mark = Style::new().bg(th.selected_bg);
        let lines: Vec<Line<'static>> = highlight::lines(&files::lang(&file.path), text)
            .into_iter()
            .enumerate()
            .map(|(i, tokens)| {
                let n = i + 1;
                let mut spans = vec![Span::styled(format!("{n:>5} "), Style::new().fg(th.dim))];
                spans.extend(tokens.into_iter().map(|((r, g, b), t)| Span::styled(t, Style::new().fg(Color::Rgb(r, g, b)))));
                let line = Line::from(spans).style(Style::new().bg(Color::Rgb(bg_r, bg_g, bg_b)));
                if file.line == Some(n) { line.style(mark) } else { line }
            })
            .collect();
        Viewer {
            title: db::tilde(&file.path.display().to_string()),
            scroll: file.line.map_or(0, |l| l.saturating_sub(4)),
            lines,
        }
    }

    /// Draws the pane; `focused` gives its border the accent color.
    pub fn draw(&mut self, f: &mut Frame, area: Rect, focused: bool) {
        let th = theme::t();
        let block = Block::new()
            .borders(Borders::LEFT)
            .border_style(Style::new().fg(if focused { th.link } else { th.dim }))
            .title(Line::from(vec![
                Span::styled(format!(" {} ", self.title), Style::new().fg(th.fg).bold()),
                Span::styled("· Esc closes ", Style::new().fg(th.dim)),
            ]));
        let inner = block.inner(area);
        self.scroll = self.scroll.min(self.lines.len().saturating_sub(inner.height as usize));
        let shown: Vec<Line> = self.lines.iter().skip(self.scroll).take(inner.height as usize).cloned().collect();
        f.render_widget(Paragraph::new(shown).block(block), area);
    }
}
