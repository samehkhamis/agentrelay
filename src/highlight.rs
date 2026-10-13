//! Markdown parsing and syntax highlighting shared by both UIs: one parser
//! setup and one syntect theme, producing events and colored tokens that the
//! TUI turns into spans and the web into HTML.

use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

pub type Rgb = (u8, u8, u8);

/// Agent markdown, with the extensions agents use (tables, ~~, task lists).
pub fn markdown(text: &str) -> pulldown_cmark::Parser<'_> {
    use pulldown_cmark::Options;
    pulldown_cmark::Parser::new_ext(text, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS)
}

/// A code block's language: the first word of its fence info.
pub fn fence_lang(kind: &pulldown_cmark::CodeBlockKind) -> String {
    match kind {
        pulldown_cmark::CodeBlockKind::Fenced(info) => info.split_whitespace().next().unwrap_or("").to_string(),
        pulldown_cmark::CodeBlockKind::Indented => String::new(),
    }
}

fn assets() -> &'static (SyntaxSet, ThemeSet) {
    static ASSETS: OnceLock<(SyntaxSet, ThemeSet)> = OnceLock::new();
    ASSETS.get_or_init(|| (SyntaxSet::load_defaults_newlines(), ThemeSet::load_defaults()))
}

/// The syntax theme matching the active color theme.
fn syntax_theme() -> &'static Theme {
    let themes = &assets().1.themes;
    themes.get(crate::theme::t().syntax).unwrap_or_else(|| &themes["base16-ocean.dark"])
}

/// The syntax theme's background, for code block backgrounds.
pub fn background() -> Rgb {
    let c = syntax_theme().settings.background.unwrap_or(syntect::highlighting::Color::BLACK);
    (c.r, c.g, c.b)
}

/// `code` highlighted as `lang` (a fence tag like `rust` or `py`; plain text if
/// unknown): one token list per source line, tabs expanded.
pub fn lines(lang: &str, code: &str) -> Vec<Vec<(Rgb, String)>> {
    let syntaxes = &assets().0;
    // syntect's built-in set lacks some common languages; use the nearest one.
    let lang = match lang {
        "ts" | "tsx" | "typescript" | "mts" | "cts" | "jsx" => "js",
        "zsh" | "fish" | "console" | "shell" => "bash",
        other => other,
    };
    let syntax = syntaxes.find_syntax_by_token(lang).unwrap_or_else(|| syntaxes.find_syntax_plain_text());
    let mut hl = HighlightLines::new(syntax, syntax_theme());
    LinesWithEndings::from(code)
        .map(|line| {
            hl.highlight_line(line, syntaxes)
                .unwrap_or_default()
                .into_iter()
                .map(|(style, text)| {
                    let c = style.foreground;
                    ((c.r, c.g, c.b), text.trim_end_matches(['\n', '\r']).replace('\t', "    "))
                })
                .filter(|(_, t)| !t.is_empty())
                .collect()
        })
        .collect()
}

/// An `<img>` tag in a markdown file's HTML, as READMEs use to size images.
pub struct ImgTag {
    pub src: String,
    pub alt: String,
    pub width: Option<u32>,
}

/// The `<img>` tags in an HTML fragment, in order.
pub fn img_tags(html: &str) -> Vec<ImgTag> {
    let lower = html.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(start) = lower[at..].find("<img").map(|i| at + i) {
        let end = lower[start..].find('>').map_or(html.len(), |i| start + i);
        let tag = &html[start..end];
        if let Some(src) = attr(tag, "src") {
            out.push(ImgTag {
                src,
                alt: attr(tag, "alt").unwrap_or_default(),
                width: attr(tag, "width").and_then(|w| w.trim_end_matches("px").parse().ok()),
            });
        }
        at = end;
    }
    out
}

/// A quoted attribute's value in a tag.
fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(name).map(|i| from + i) {
        from = i + name.len();
        // A whole attribute name, followed by =.
        let before = lower[..i].chars().next_back();
        if !before.is_some_and(char::is_whitespace) {
            continue;
        }
        let rest = tag[from..].trim_start().strip_prefix('=')?.trim_start();
        let quote = rest.chars().next().filter(|q| *q == '"' || *q == '\'')?;
        let value = &rest[1..];
        return value.find(quote).map(|j| value[..j].to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::img_tags;

    #[test]
    fn finds_img_tags() {
        let tags = img_tags("<p align=\"center\">\n  <img src=\"a.png\" alt=\"An A\" width=\"220\">\n  <IMG data-src='x' src='b.png'></p>");
        let got: Vec<_> = tags.iter().map(|t| (t.src.as_str(), t.alt.as_str(), t.width)).collect();
        assert_eq!(got, [("a.png", "An A", Some(220)), ("b.png", "", None)]);
    }
}
