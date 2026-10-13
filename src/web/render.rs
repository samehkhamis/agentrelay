//! Hub posts to HTML: markdown via pulldown-cmark with code blocks from the
//! shared highlighter, so colors match the TUI. Agent-written HTML is escaped
//! and only http(s)/mailto links survive, since the page can send prompts.

use crate::highlight;
use pulldown_cmark::{html, CowStr, Event, Tag, TagEnd};

/// Renders markdown; inline code that `is_file` accepts becomes a file link
/// the page opens in its file panel.
pub fn markdown(text: &str, is_file: &dyn Fn(&str) -> bool) -> String {
    let mut events = Vec::new();
    let mut code: Option<(String, String)> = None;
    for ev in highlight::markdown(text) {
        match (&mut code, ev) {
            (None, Event::Start(Tag::CodeBlock(kind))) => code = Some((highlight::fence_lang(&kind), String::new())),
            (Some((_, buf)), Event::Text(t)) => buf.push_str(&t),
            (Some(_), Event::End(TagEnd::CodeBlock)) => {
                let (lang, buf) = code.take().unwrap_or_default();
                events.push(Event::Html(code_block(&lang, &buf).into()));
            }
            (_, Event::Html(t) | Event::InlineHtml(t)) => events.push(Event::Text(t)),
            (_, Event::Code(t)) if is_file(&t) => {
                let t = escape(&t);
                events.push(Event::Html(format!("<a class=\"file\" data-file=\"{t}\"><code>{t}</code></a>").into()));
            }
            (_, Event::Start(Tag::Link { link_type, dest_url, title, id })) => {
                let safe = ["http://", "https://", "mailto:"].iter().any(|p| dest_url.starts_with(p));
                let dest_url = if safe { dest_url } else { CowStr::Borrowed("#") };
                events.push(Event::Start(Tag::Link { link_type, dest_url, title, id }));
            }
            // Images would load remote content; keep just their alt text.
            (_, Event::Start(Tag::Image { .. }) | Event::End(TagEnd::Image)) => {}
            (_, ev) => events.push(ev),
        }
    }
    let mut out = String::new();
    html::push_html(&mut out, events.into_iter());
    out
}

fn code_block(lang: &str, code: &str) -> String {
    let mut out = String::from("<pre class=\"code\"><code>");
    for (i, tokens) in highlight::lines(lang, code).into_iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        for ((r, g, b), text) in tokens {
            out.push_str(&format!("<span style=\"color:#{r:02x}{g:02x}{b:02x}\">{}</span>", escape(&text)));
        }
    }
    out.push_str("</code></pre>");
    out
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// A file for the file panel: highlighted, with line numbers, `line` marked.
pub fn file(lang: &str, text: &str, line: Option<usize>) -> String {
    let mut out = String::from("<pre class=\"code file-view\"><code>");
    for (i, tokens) in highlight::lines(lang, text).into_iter().enumerate() {
        let n = i + 1;
        let mark = if line == Some(n) { " class=\"hl\"" } else { "" };
        out.push_str(&format!("<span{mark} id=\"L{n}\"><span class=\"ln\">{n}</span>"));
        for ((r, g, b), text) in tokens {
            out.push_str(&format!("<span style=\"color:#{r:02x}{g:02x}{b:02x}\">{}</span>", escape(&text)));
        }
        out.push_str("</span>"); // each line is a block; no newline between them
    }
    out.push_str("</code></pre>");
    out
}

#[cfg(test)]
mod tests {
    use super::markdown;

    #[test]
    fn escapes_html_and_unsafe_links() {
        let out = markdown("<script>x()</script>\n\n[a](javascript:alert(1)) [b](https://ok.dev)", &|_| false);
        assert!(!out.contains("<script>"));
        assert!(out.contains("href=\"#\""), "{out}");
        assert!(out.contains("href=\"https://ok.dev\""));
    }

    #[test]
    fn links_files_in_inline_code() {
        let out = markdown("see `src/a.rs` and `cargo build`", &|c| c == "src/a.rs");
        assert!(out.contains("<a class=\"file\" data-file=\"src/a.rs\">"));
        assert!(out.contains("<code>cargo build</code>"));
    }

    #[test]
    fn highlights_code_blocks() {
        let out = markdown("```rust\nfn main() {}\n```", &|_| false);
        assert!(out.starts_with("<pre class=\"code\"><code><span style=\"color:#"));
    }
}
