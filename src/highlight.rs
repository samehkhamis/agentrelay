//! Syntax highlighting shared by both UIs: one syntect setup and theme,
//! producing colored tokens that the TUI turns into spans and the web into HTML.

use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

pub type Rgb = (u8, u8, u8);

const THEME: &str = "base16-ocean.dark";

fn assets() -> &'static (SyntaxSet, Theme) {
    static ASSETS: OnceLock<(SyntaxSet, Theme)> = OnceLock::new();
    ASSETS.get_or_init(|| (SyntaxSet::load_defaults_newlines(), ThemeSet::load_defaults().themes[THEME].clone()))
}

/// The theme's background, for code block backgrounds.
pub fn background() -> Rgb {
    let c = assets().1.settings.background.unwrap_or(syntect::highlighting::Color::BLACK);
    (c.r, c.g, c.b)
}

/// `code` highlighted as `lang` (a fence tag like `rust` or `py`; plain text if
/// unknown): one token list per source line, tabs expanded.
pub fn lines(lang: &str, code: &str) -> Vec<Vec<(Rgb, String)>> {
    let (syntaxes, theme) = assets();
    let syntax = syntaxes.find_syntax_by_token(lang).unwrap_or_else(|| syntaxes.find_syntax_plain_text());
    let mut hl = HighlightLines::new(syntax, theme);
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
