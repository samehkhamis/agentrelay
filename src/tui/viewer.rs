//! The file pane: a file a feed mentions, highlighted, with line numbers,
//! opened at the mentioned line. Markdown files can also show rendered.

use super::images::Images;
use super::markdown;
use crate::files::{self, FileRef};
use crate::{db, highlight, theme};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

pub struct Viewer {
    title: String,
    /// The file's folder, for links in the preview.
    dir: String,
    text: String,
    source: Vec<Line<'static>>,
    /// Some for markdown: the preview rendered at a width, once shown.
    preview: Option<Option<Preview>>,
    previewing: bool,
    /// Where the Preview/Source toggle was drawn, for clicks.
    toggle: Rect,
    pub scroll: usize,
    /// The source scroll, kept while the preview shows.
    source_scroll: usize,
}

impl Viewer {
    pub fn new(file: &FileRef, text: &str) -> Viewer {
        let th = theme::t();
        let (bg_r, bg_g, bg_b) = highlight::background();
        let mark = Style::new().bg(th.selected_bg);
        let source: Vec<Line<'static>> = highlight::lines(&files::lang(&file.path), text)
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
        let markdown = files::is_markdown(&file.path);
        let scroll = file.line.map_or(0, |l| l.saturating_sub(4));
        Viewer {
            title: db::tilde(&file.path.display().to_string()),
            dir: file.path.parent().map(|d| d.display().to_string()).unwrap_or_default(),
            text: text.to_string(),
            source,
            preview: markdown.then_some(None),
            // Markdown opens rendered, unless the link points at a line.
            previewing: markdown && file.line.is_none(),
            toggle: Rect::default(),
            scroll: if markdown && file.line.is_none() { 0 } else { scroll },
            source_scroll: scroll,
        }
    }

    /// Switches a markdown file between its preview and its source.
    pub fn toggle(&mut self) {
        if self.preview.is_none() {
            return;
        }
        self.previewing = !self.previewing;
        if self.previewing {
            self.source_scroll = self.scroll;
            self.scroll = 0;
        } else {
            self.scroll = self.source_scroll;
        }
    }

    /// Whether a click at (col, row) hit the Preview/Source toggle.
    pub fn on_toggle(&self, col: u16, row: u16) -> bool {
        row == self.toggle.y && (self.toggle.x..self.toggle.right()).contains(&col)
    }

    /// Draws the pane; `focused` gives its border the accent color.
    pub fn draw(&mut self, f: &mut Frame, area: Rect, focused: bool, mut images: Option<&mut Images>) {
        let th = theme::t();
        let dim = Style::new().fg(th.dim);
        let mut title = vec![Span::styled(format!(" {} ", self.title), Style::new().fg(th.fg).bold())];
        if self.preview.is_some() {
            let tab = |label: &str, on: bool| match on {
                true => Span::styled(format!(" {label} "), Style::new().fg(th.fg).bg(th.selected_bg).bold()),
                false => Span::styled(format!(" {label} "), dim),
            };
            // The title starts after the left border.
            let x = area.x + 1 + title[0].content.width() as u16;
            title.extend([tab("Preview", self.previewing), tab("Source", !self.previewing)]);
            self.toggle = Rect::new(x, area.y, ("Preview".len() + "Source".len() + 4) as u16, 1);
            title.push(Span::styled(" Alt+m ", dim));
        }
        title.push(Span::styled("· Esc closes ", dim));
        let block = Block::new()
            .borders(Borders::LEFT)
            .border_style(Style::new().fg(if focused { th.link } else { th.dim }))
            .title(Line::from(title));
        let inner = block.inner(area);
        let (lines, slots) = match &mut self.preview {
            Some(cached) if self.previewing => {
                let width = inner.width.saturating_sub(2);
                if cached.as_ref().is_none_or(|p| p.width != width) {
                    *cached = Some(Preview::new(&self.text, &self.dir, width, images.as_deref_mut()));
                }
                let p = cached.as_ref().expect("rendered above");
                (&p.lines, p.slots.as_slice())
            }
            _ => (&self.source, &[][..]),
        };
        let height = inner.height as usize;
        self.scroll = self.scroll.min(lines.len().saturating_sub(height));
        let shown: Vec<Line> = lines.iter().skip(self.scroll).take(height).cloned().collect();
        f.render_widget(Paragraph::new(shown).block(block), area);
        // Images go into their reserved rows when those are fully on screen.
        if let Some(images) = images {
            let end = self.scroll + height;
            for s in slots.iter().filter(|s| s.line >= self.scroll && s.line + s.rows as usize <= end) {
                let x = inner.x + 1;
                let rect = Rect::new(x, inner.y + (s.line - self.scroll) as u16, s.cols.min(inner.right() - x), s.rows);
                images.draw(f, rect, &s.path);
            }
        }
    }
}

/// A markdown file rendered for the pane, with rows reserved for its local
/// images.
struct Preview {
    width: u16,
    lines: Vec<Line<'static>>,
    slots: Vec<Slot>,
}

/// An image's place in the preview.
struct Slot {
    line: usize,
    cols: u16,
    rows: u16,
    path: String,
}

impl Preview {
    fn new(text: &str, dir: &str, width: u16, mut images: Option<&mut Images>) -> Preview {
        let is_file = |c: &str| files::resolve(dir, c).is_some();
        let (rendered, found) = markdown::render_doc(text, width as usize, &is_file);
        let mut lines = Vec::with_capacity(rendered.len());
        let mut slots = Vec::new();
        let mut found = found.into_iter().peekable();
        let mut skip = 0..0;
        for (i, line) in rendered.into_iter().enumerate() {
            // An image's alt text, when the image can be drawn, becomes blank
            // rows for it.
            let image = found.next_if(|(at, _)| at.start == i).and_then(|(at, url)| {
                let path = local_image(dir, &url)?;
                let (cols, rows) = images.as_deref_mut()?.size(&path, width)?;
                skip = at;
                Some(Slot { line: lines.len(), cols, rows, path })
            });
            match image {
                Some(slot) => {
                    lines.extend((0..slot.rows).map(|_| Line::default()));
                    slots.push(slot);
                }
                None if skip.contains(&i) => {}
                None => lines.push(pad(line)),
            }
        }
        Preview { width, lines, slots }
    }
}

/// A relative image URL as a file beside the markdown file.
fn local_image(dir: &str, url: &str) -> Option<String> {
    if url.contains("://") || url.starts_with("data:") {
        return None;
    }
    let path = std::path::Path::new(dir).join(url.split(['#', '?']).next()?);
    path.is_file().then(|| path.display().to_string())
}

/// A left margin for preview lines.
fn pad(mut l: Line<'static>) -> Line<'static> {
    l.spans.insert(0, Span::raw(" "));
    l
}
