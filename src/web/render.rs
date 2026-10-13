//! Hub posts to HTML: markdown via pulldown-cmark with code blocks from the
//! shared highlighter, so colors match the TUI. Agent-written HTML is escaped
//! and only http(s)/mailto links survive, since the page can send prompts.

use crate::highlight;
use pulldown_cmark::{html, CowStr, Event, Tag, TagEnd};
use std::path::{Path, PathBuf};

/// Renders markdown; inline code that `is_file` accepts becomes a file link
/// the page opens in its file panel.
pub fn markdown(text: &str, is_file: &dyn Fn(&str) -> bool) -> String {
    render(text, &|c| is_file(c).then(|| c.to_string()), None)
}

/// A markdown file rendered for the file panel's preview. Relative links and
/// inline code naming files beside it open in the panel; local images under
/// its folder are inlined. Remote images keep just their alt text.
pub fn preview(text: &str, file: &Path) -> String {
    let dir = file.parent().unwrap_or(Path::new("/"));
    let cwd = dir.display().to_string();
    let code_link = |c: &str| {
        crate::files::resolve(&cwd, c).map(|f| match f.line {
            Some(n) => format!("{}:{n}", f.path.display()),
            None => f.path.display().to_string(),
        })
    };
    render(text, &code_link, Some(dir))
}

/// `file_link` maps inline code to the file it names, if any. With `dir`
/// (a previewed file's folder), relative links open files and local images
/// are inlined.
fn render(text: &str, file_link: &dyn Fn(&str) -> Option<String>, dir: Option<&Path>) -> String {
    let mut events = Vec::new();
    let mut code: Option<(String, String)> = None;
    // Per open link: whether it became a file link, which needs its own close tag.
    let mut links = Vec::new();
    let mut in_image = false;
    for ev in highlight::markdown(text) {
        match (&mut code, ev) {
            (None, Event::Start(Tag::CodeBlock(kind))) => code = Some((highlight::fence_lang(&kind), String::new())),
            (Some((_, buf)), Event::Text(t)) => buf.push_str(&t),
            (Some(_), Event::End(TagEnd::CodeBlock)) => {
                let (lang, buf) = code.take().unwrap_or_default();
                events.push(Event::Html(code_block(&lang, &buf).into()));
            }
            // In a preview, `<img>` tags (as READMEs use to size images) show;
            // other markup around them is dropped.
            (None, Event::Html(t) | Event::InlineHtml(t)) if dir.is_some() => {
                for img in highlight::img_tags(&t) {
                    match dir.and_then(|d| inline_image(d, &img.src)) {
                        Some(src) => {
                            let width = img.width.map(|w| format!(" width=\"{w}\"")).unwrap_or_default();
                            events.push(Event::Html(format!("<img src=\"{src}\" alt=\"{}\"{width}>", escape(&img.alt)).into()));
                        }
                        None => events.push(Event::Text(img.alt.into())),
                    }
                }
            }
            (_, Event::Html(t) | Event::InlineHtml(t)) => events.push(Event::Text(t)),
            (_, Event::Code(t)) => match file_link(&t) {
                Some(f) => events.push(Event::Html(
                    format!("<a class=\"file\" data-file=\"{}\"><code>{}</code></a>", escape(&f), escape(&t)).into(),
                )),
                None => events.push(Event::Code(t)),
            },
            (_, Event::Start(Tag::Link { link_type, dest_url, title, id })) => {
                let file = dir.and_then(|d| local_file(d, &dest_url));
                links.push(file.is_some());
                match file {
                    Some(f) => events.push(Event::Html(format!("<a class=\"file\" data-file=\"{}\">", escape(&f.display().to_string())).into())),
                    None => {
                        let safe = ["http://", "https://", "mailto:"].iter().any(|p| dest_url.starts_with(p));
                        let dest_url = if safe { dest_url } else { CowStr::Borrowed("#") };
                        events.push(Event::Start(Tag::Link { link_type, dest_url, title, id }));
                    }
                }
            }
            (_, Event::End(TagEnd::Link)) => match links.pop() {
                Some(true) => events.push(Event::Html("</a>".into())),
                _ => events.push(Event::End(TagEnd::Link)),
            },
            (_, Event::Start(Tag::Image { dest_url, .. })) => {
                if let Some(src) = dir.and_then(|d| inline_image(d, &dest_url)) {
                    events.push(Event::Html(format!("<img src=\"{src}\" alt=\"\">").into()));
                    in_image = true;
                }
            }
            // Images would load remote content; keep just their alt text.
            (_, Event::End(TagEnd::Image)) => in_image = false,
            (_, Event::Text(_)) if in_image => {}
            (_, ev) => events.push(ev),
        }
    }
    let mut out = String::new();
    html::push_html(&mut out, events.into_iter());
    out
}

/// A relative link's target, if it's a file. Opening it still goes through
/// the hub's folder check.
fn local_file(dir: &Path, url: &str) -> Option<PathBuf> {
    let url = url.split('#').next().unwrap_or("");
    if url.is_empty() || url.contains(':') || url.starts_with('/') {
        return None;
    }
    let path = dir.join(url).canonicalize().ok()?;
    path.is_file().then_some(path)
}

/// A local image under `dir` as a data URL, so the page needs no route to
/// fetch it.
fn inline_image(dir: &Path, url: &str) -> Option<String> {
    use base64::Engine;
    const MAX: u64 = 5 << 20;
    // Inlining skips the hub's check, so only images under the file's folder.
    let path = local_file(dir, url).filter(|p| dir.canonicalize().is_ok_and(|d| p.starts_with(d)))?;
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    // SVG is safe as an <img> (no scripts run), though not served as a page.
    let mime = if ext == "svg" { "image/svg+xml" } else { crate::uploads::mime(&path) };
    if !mime.starts_with("image/") || path.metadata().ok()?.len() > MAX {
        return None;
    }
    let data = base64::engine::general_purpose::STANDARD.encode(std::fs::read(&path).ok()?);
    Some(format!("data:{mime};base64,{data}"))
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
    use super::{markdown, preview};

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
    fn previews_link_local_files_and_inline_images() {
        let dir = std::env::temp_dir().join(format!("agentrelay-preview-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        std::fs::write(dir.join("docs/guide.md"), "# hi").unwrap();
        std::fs::write(dir.join("docs/shot.png"), [0x89, b'P', b'N', b'G']).unwrap();
        let readme = dir.join("README.md");
        let out = preview("[guide](docs/guide.md) ![s](docs/shot.png) ![r](https://x.dev/a.png) [w](https://x.dev)", &readme);
        let guide = dir.join("docs/guide.md").canonicalize().unwrap();
        assert!(out.contains(&format!("<a class=\"file\" data-file=\"{}\">guide</a>", guide.display())), "{out}");
        assert!(out.contains("<img src=\"data:image/png;base64,"), "{out}");
        // A remote image keeps only its alt text.
        assert!(!out.contains("x.dev/a.png") && out.contains("> r <a"), "{out}");
        assert!(out.contains("href=\"https://x.dev\""));
        // Sized `<img>` tags too, without the markup around them.
        let out = preview("<p align=\"center\">\n  <img src=\"docs/shot.png\" alt=\"s\" width=\"220\">\n</p>", &readme);
        assert!(out.contains("<img src=\"data:image/png;base64,") && out.contains("width=\"220\""), "{out}");
        assert!(!out.contains("&lt;p"), "{out}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn highlights_code_blocks() {
        let out = markdown("```rust\nfn main() {}\n```", &|_| false);
        assert!(out.starts_with("<pre class=\"code\"><code><span style=\"color:#"));
    }
}
