//! Markdown to wrapped terminal lines: paragraphs, headings, emphasis, inline
//! code, lists, task lists, quotes, rules, tables and highlighted code blocks.

use crate::{highlight, theme};
use pulldown_cmark::{Event, HeadingLevel, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::ops::Range;
use unicode_width::UnicodeWidthStr;

/// Renders markdown; inline code that `is_file` accepts is drawn as a file link.
pub fn render(text: &str, width: usize, is_file: &dyn Fn(&str) -> bool) -> Vec<Line<'static>> {
    let mut r = Renderer { width: width.max(16), ..Renderer::default() };
    r.run(text, is_file);
    r.out
}

/// Like [`render`], also returning each image's lines (holding its alt text)
/// and its URL, for a caller that can draw images there.
pub fn render_doc(text: &str, width: usize, is_file: &dyn Fn(&str) -> bool) -> (Vec<Line<'static>>, Vec<(Range<usize>, String)>) {
    let mut r = Renderer { width: width.max(16), doc: true, ..Renderer::default() };
    r.run(text, is_file);
    (r.out, r.images)
}

#[derive(Default)]
struct Table {
    rows: Vec<Vec<String>>,
    header_rows: usize,
}

#[derive(Default)]
struct Renderer {
    width: usize,
    out: Vec<Line<'static>>,
    /// The inline run being collected for the current block.
    spans: Vec<Span<'static>>,
    /// Inline style stack (emphasis, strong, links, …).
    styles: Vec<Style>,
    heading: Option<HeadingLevel>,
    quote: usize,
    /// One entry per open list: the next number for ordered lists.
    lists: Vec<Option<u64>>,
    /// Marker for the first line of the current list item, until used.
    marker: Option<String>,
    code: Option<(String, String)>,
    table: Option<Table>,
    /// Images: the lines with their alt text, and their URL.
    images: Vec<(Range<usize>, String)>,
    /// A document (a previewed file) rather than a message: its `<img>`
    /// tags count as images, and other HTML is dropped.
    doc: bool,
}

impl Renderer {
    fn run(&mut self, text: &str, is_file: &dyn Fn(&str) -> bool) {
        for ev in highlight::markdown(text) {
            match ev {
                Event::Code(t) if is_file(&t) => {
                    let link = Style::new().fg(theme::t().link).add_modifier(Modifier::UNDERLINED);
                    self.push(&t, link);
                }
                ev => self.event(ev),
            }
        }
        self.flush();
        while self.out.last().is_some_and(|l| l.width() == 0) {
            self.out.pop();
        }
    }

    fn style(&self) -> Style {
        let mut s = self.styles.iter().fold(Style::new(), |a, b| a.patch(*b));
        match self.heading {
            Some(HeadingLevel::H1) => s = s.add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            Some(_) => s = s.add_modifier(Modifier::BOLD),
            None => {}
        }
        s
    }

    /// Prefixes for the first and following lines of the current block.
    fn prefixes(&mut self) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
        let mut base = Vec::new();
        for _ in 0..self.quote {
            base.push(Span::styled("│ ", Style::new().fg(theme::t().dim)));
        }
        let indent = "  ".repeat(self.lists.len().saturating_sub(1));
        if self.lists.is_empty() {
            return (base.clone(), base);
        }
        let mut first = base.clone();
        let mut rest = base;
        let marker = self.marker.take();
        let pad = marker.as_ref().map_or(2, |m| m.width());
        first.push(Span::raw(indent.clone()));
        first.push(Span::styled(marker.unwrap_or_else(|| " ".repeat(pad)), Style::new().fg(theme::t().dim)));
        rest.push(Span::raw(format!("{indent}{}", " ".repeat(pad))));
        (first, rest)
    }

    fn gap(&mut self) {
        if self.out.last().is_some_and(|l| l.width() > 0) {
            self.out.push(Line::default());
        }
    }

    /// Starts an image's line, marked, noting where it is (outside tables)
    /// for a caller that can draw the image there.
    fn image(&mut self, url: &str) {
        if self.table.is_none() {
            self.flush();
            self.images.push((self.out.len()..self.out.len(), url.to_string()));
        }
        self.push("🖼 ", Style::new().fg(theme::t().link));
    }

    /// Ends an image's alt text, noting the lines it took.
    fn end_image(&mut self) {
        if self.table.is_none() {
            self.flush();
            if let Some((lines, _)) = self.images.last_mut() {
                lines.end = self.out.len();
            }
        }
    }

    fn flush(&mut self) {
        if self.spans.is_empty() {
            return;
        }
        let spans = std::mem::take(&mut self.spans);
        let (first, rest) = self.prefixes();
        self.out.extend(wrap(spans, self.width, first, rest));
    }

    fn push(&mut self, text: &str, style: Style) {
        if let Some(t) = &mut self.table {
            if let Some(cell) = t.rows.last_mut().and_then(|r| r.last_mut()) {
                cell.push_str(text);
            }
        } else if let Some((_, buf)) = &mut self.code {
            buf.push_str(text);
        } else {
            self.spans.push(Span::styled(text.to_string(), style));
        }
    }

    fn event(&mut self, ev: Event) {
        match ev {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => self.push(&t, self.style()),
            Event::Code(t) | Event::InlineMath(t) => self.push(&t, Style::new().fg(theme::t().code)),
            Event::DisplayMath(t) => self.push(&t, Style::new().fg(theme::t().code)),
            Event::Html(t) | Event::InlineHtml(t) if self.doc => {
                for img in highlight::img_tags(&t) {
                    self.image(&img.src);
                    self.push(&img.alt, Style::new().fg(theme::t().link).add_modifier(Modifier::ITALIC));
                    self.end_image();
                }
            }
            Event::Html(t) | Event::InlineHtml(t) => self.push(&t, Style::new().fg(theme::t().dim)),
            Event::SoftBreak => self.push(" ", Style::new()),
            Event::HardBreak => self.push("\n", Style::new()),
            Event::TaskListMarker(done) => self.push(if done { "☑ " } else { "☐ " }, Style::new().fg(theme::t().dim)),
            Event::FootnoteReference(r) => self.push(&format!("[{r}]"), Style::new().fg(theme::t().dim)),
            Event::Rule => {
                self.flush();
                self.gap();
                self.out.push(Line::styled("─".repeat(self.width.min(40)), Style::new().fg(theme::t().dim)));
            }
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph => {
                if self.marker.is_none() {
                    self.gap();
                }
            }
            Tag::Heading { level, .. } => {
                self.flush();
                self.gap();
                self.heading = Some(level);
            }
            Tag::BlockQuote(_) => {
                self.flush();
                self.gap();
                self.quote += 1;
            }
            Tag::CodeBlock(kind) => {
                self.flush();
                self.gap();
                self.code = Some((highlight::fence_lang(&kind), String::new()));
            }
            Tag::List(start) => {
                self.flush();
                if self.lists.is_empty() {
                    self.gap();
                }
                self.lists.push(start);
            }
            Tag::Item => {
                self.flush();
                let marker = match self.lists.last_mut() {
                    Some(Some(n)) => {
                        *n += 1;
                        format!("{}. ", *n - 1)
                    }
                    _ => "• ".to_string(),
                };
                self.marker = Some(marker);
            }
            Tag::Emphasis => self.styles.push(Style::new().add_modifier(Modifier::ITALIC)),
            Tag::Strong => self.styles.push(Style::new().add_modifier(Modifier::BOLD)),
            Tag::Strikethrough => self.styles.push(Style::new().add_modifier(Modifier::CROSSED_OUT)),
            Tag::Link { .. } => self.styles.push(Style::new().fg(theme::t().link).add_modifier(Modifier::UNDERLINED)),
            // An image shows as its alt text, marked, on a line of its own
            // (outside tables) where a caller can draw the image instead.
            Tag::Image { dest_url, .. } => {
                self.image(&dest_url);
                self.styles.push(Style::new().fg(theme::t().link).add_modifier(Modifier::ITALIC));
            }
            Tag::Table(_) => {
                self.flush();
                self.gap();
                self.table = Some(Table::default());
            }
            Tag::TableHead | Tag::TableRow => {
                if let Some(t) = &mut self.table {
                    t.rows.push(Vec::new());
                }
            }
            Tag::TableCell => {
                if let Some(row) = self.table.as_mut().and_then(|t| t.rows.last_mut()) {
                    row.push(String::new());
                }
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Item => self.flush(),
            TagEnd::Heading(_) => {
                self.flush();
                self.heading = None;
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.quote = self.quote.saturating_sub(1);
            }
            TagEnd::CodeBlock => {
                if let Some((lang, code)) = self.code.take() {
                    let (prefix, _) = self.prefixes();
                    self.out.extend(code_block(&lang, &code, self.width, &prefix));
                }
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Link => {
                self.styles.pop();
            }
            TagEnd::Image => {
                self.styles.pop();
                self.end_image();
            }
            TagEnd::TableHead => {
                if let Some(t) = &mut self.table {
                    t.header_rows = t.rows.len();
                }
            }
            TagEnd::Table => {
                if let Some(t) = self.table.take() {
                    self.out.extend(table(t, self.width));
                }
            }
            _ => {}
        }
    }
}

/// A highlighted code block, hard-wrapped to `width`, each line behind
/// `prefix` and a gutter bar.
fn code_block(lang: &str, code: &str, width: usize, prefix: &[Span<'static>]) -> Vec<Line<'static>> {
    use unicode_width::UnicodeWidthChar;
    let gutter = Span::styled("▎ ", Style::new().fg(theme::t().dim));
    let room = width.saturating_sub(prefix.iter().map(|s| s.width()).sum::<usize>() + 2).max(8);
    let start = || prefix.iter().cloned().chain([gutter.clone()]).collect::<Vec<_>>();
    let mut out = Vec::new();
    for tokens in highlight::lines(lang, code) {
        let mut line = start();
        let mut used = 0;
        for ((r, g, b), text) in tokens {
            let style = Style::new().fg(ratatui::style::Color::Rgb(r, g, b));
            let mut buf = String::new();
            for c in text.chars() {
                let w = c.width().unwrap_or(0);
                if used + w > room {
                    line.push(Span::styled(std::mem::take(&mut buf), style));
                    out.push(Line::from(std::mem::replace(&mut line, start())));
                    used = 0;
                }
                buf.push(c);
                used += w;
            }
            if !buf.is_empty() {
                line.push(Span::styled(buf, style));
            }
        }
        out.push(Line::from(line));
    }
    out
}

/// Word-wraps styled spans to `width`, with prefixes for the first and the
/// following lines. Words longer than a line are split.
fn wrap(spans: Vec<Span<'static>>, width: usize, first: Vec<Span<'static>>, rest: Vec<Span<'static>>) -> Vec<Line<'static>> {
    let prefix_w = |p: &[Span]| p.iter().map(|s| s.width()).sum::<usize>();
    let mut lines = Vec::new();
    let mut cur = first.clone();
    let mut used = prefix_w(&first);
    let mut fresh = true; // nothing but the prefix on this line yet
    let mut newline = |cur: &mut Vec<Span<'static>>, used: &mut usize, fresh: &mut bool| {
        while cur.last().is_some_and(|s| s.content == " ") {
            cur.pop();
            *used -= 1;
        }
        lines.push(Line::from(std::mem::replace(cur, rest.clone())));
        *used = prefix_w(&rest);
        *fresh = true;
    };
    for span in spans {
        for token in tokens(&span.content) {
            if token == "\n" {
                newline(&mut cur, &mut used, &mut fresh);
            } else if token.trim().is_empty() {
                if !fresh && used < width {
                    cur.push(Span::raw(" "));
                    used += 1;
                }
            } else {
                let mut word = token.to_string();
                if !fresh && used + word.width() > width {
                    newline(&mut cur, &mut used, &mut fresh);
                }
                // Split words that can't fit on any line.
                while used + word.width() > width && word.chars().count() > 1 {
                    let room = width.saturating_sub(used).max(1);
                    let mut head = String::new();
                    for c in word.chars() {
                        if (head.clone() + &c.to_string()).width() > room {
                            break;
                        }
                        head.push(c);
                    }
                    if head.is_empty() {
                        head = word.chars().next().unwrap().to_string();
                    }
                    word = word[head.len()..].to_string();
                    cur.push(Span::styled(head, span.style));
                    newline(&mut cur, &mut used, &mut fresh);
                }
                used += word.width();
                cur.push(Span::styled(word, span.style));
                fresh = false;
            }
        }
    }
    if !fresh {
        newline(&mut cur, &mut used, &mut fresh);
    }
    lines
}

/// Splits into words, runs of spaces, and single newlines.
fn tokens(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let class = |c: char| if c == '\n' { 0 } else if c.is_whitespace() { 1 } else { 2 };
    let mut prev = None;
    for (i, c) in s.char_indices() {
        let k = class(c);
        if prev.is_some_and(|p| p != k || k == 0) {
            out.push(&s[start..i]);
            start = i;
        }
        prev = Some(k);
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// A table with columns sized to content, shrunk to fit the width.
fn table(t: Table, width: usize) -> Vec<Line<'static>> {
    let cols = t.rows.iter().map(Vec::len).max().unwrap_or(0);
    if cols == 0 {
        return Vec::new();
    }
    let mut widths: Vec<usize> = (0..cols)
        .map(|c| t.rows.iter().filter_map(|r| r.get(c)).map(|s| s.width()).max().unwrap_or(0))
        .collect();
    let room = width.saturating_sub(3 * (cols - 1));
    while widths.iter().sum::<usize>() > room {
        let (i, w) = widths.iter().enumerate().max_by_key(|(_, w)| **w).map(|(i, w)| (i, *w)).unwrap();
        if w <= 4 {
            break;
        }
        widths[i] = w - 1;
    }
    let sep = Span::styled(" │ ", Style::new().fg(theme::t().dim));
    let mut out = Vec::new();
    for (r, row) in t.rows.iter().enumerate() {
        let header = r < t.header_rows;
        let style = if header { Style::new().add_modifier(Modifier::BOLD) } else { Style::new() };
        let mut spans = Vec::new();
        for (c, w) in widths.iter().enumerate() {
            if c > 0 {
                spans.push(sep.clone());
            }
            spans.push(Span::styled(fit(row.get(c).map_or("", String::as_str), *w), style));
        }
        out.push(Line::from(spans));
        if header && r + 1 == t.header_rows {
            let rule: Vec<String> = widths.iter().map(|w| "─".repeat(*w)).collect();
            out.push(Line::styled(rule.join("─┼─"), Style::new().fg(theme::t().dim)));
        }
    }
    out
}

/// Pads or truncates to exactly `w` columns.
fn fit(s: &str, w: usize) -> String {
    if s.width() <= w {
        return format!("{s}{}", " ".repeat(w - s.width()));
    }
    let mut out = String::new();
    for c in s.chars() {
        if (out.clone() + &c.to_string()).width() + 1 > w {
            break;
        }
        out.push(c);
    }
    let pad = w.saturating_sub(out.width() + 1);
    format!("{out}…{}", " ".repeat(pad))
}

#[cfg(test)]
mod tests {
    use super::render;

    fn plain(text: &str, width: usize) -> Vec<String> {
        render(text, width, &|_| false).iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect()
    }

    #[test]
    fn list_items_align() {
        let out = plain("Intro:\n- **One:** first item\n- **Two:** second\n- **Three:** third", 80);
        assert_eq!(out, ["Intro:", "", "• One: first item", "• Two: second", "• Three: third"]);
    }

    #[test]
    fn wraps_with_hanging_indent() {
        let out = plain("- alpha beta gamma delta", 14);
        assert_eq!(out, ["• alpha beta", "  gamma delta"]);
    }

    #[test]
    fn code_block_keeps_lines() {
        let out = plain("```rust\nfn main() {}\nlet x = 1;\n```", 40);
        assert_eq!(out, ["▎ fn main() {}", "▎ let x = 1;"]);
    }

    #[test]
    fn table_aligns_columns() {
        let out = plain("| a | bb |\n|---|---|\n| ccc | d |", 40);
        assert_eq!(out, ["a   │ bb", "────┼───", "ccc │ d "]);
    }
}
