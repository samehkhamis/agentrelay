//! Syntax highlighting shared by both UIs: one syntect setup and theme,
//! producing colored tokens that the TUI turns into spans and the web into HTML.

use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

pub type Rgb = (u8, u8, u8);

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
