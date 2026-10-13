//! Hub posts to HTML: markdown via pulldown-cmark with code blocks from the
//! shared highlighter, so colors match the TUI. Agent-written HTML is escaped
//! and only http(s)/mailto links survive, since the page can send prompts.

use crate::highlight;
use pulldown_cmark::{html, CodeBlockKind, CowStr, Event, Options, Parser, Tag, TagEnd};

pub fn markdown(text: &str) -> String {
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut events = Vec::new();
    let mut code: Option<(String, String)> = None;
    for ev in Parser::new_ext(text, opts) {
        match (&mut code, ev) {
            (None, Event::Start(Tag::CodeBlock(kind))) => {
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => info.split_whitespace().next().unwrap_or("").to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                code = Some((lang, String::new()));
            }
            (Some((_, buf)), Event::Text(t)) => buf.push_str(&t),
            (Some(_), Event::End(TagEnd::CodeBlock)) => {
                let (lang, buf) = code.take().unwrap_or_default();
                events.push(Event::Html(code_block(&lang, &buf).into()));
            }
            (_, Event::Html(t) | Event::InlineHtml(t)) => events.push(Event::Text(t)),
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

pub fn escape(s: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::markdown;

    #[test]
    fn escapes_html_and_unsafe_links() {
        let out = markdown("<script>x()</script>\n\n[a](javascript:alert(1)) [b](https://ok.dev)");
        assert!(!out.contains("<script>"));
        assert!(out.contains("href=\"#\""), "{out}");
        assert!(out.contains("href=\"https://ok.dev\""));
    }

    #[test]
    fn highlights_code_blocks() {
        let out = markdown("```rust\nfn main() {}\n```");
        assert!(out.starts_with("<pre class=\"code\"><code><span style=\"color:#"));
    }
}
