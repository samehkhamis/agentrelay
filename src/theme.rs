//! Colors, in one place.

use ratatui::style::Color;

pub const SIDEBAR_BG: Color = Color::Rgb(63, 14, 64);
pub const SIDEBAR_FG: Color = Color::Rgb(207, 195, 207);
pub const SIDEBAR_DIM: Color = Color::Rgb(160, 135, 160);
pub const SELECTED_BG: Color = Color::Rgb(17, 100, 163);
pub const BADGE_BG: Color = Color::Rgb(205, 37, 83);
pub const CLAUDE: Color = Color::Rgb(217, 119, 87);
pub const CODEX: Color = Color::Rgb(16, 163, 127);
pub const USER: Color = Color::Rgb(54, 197, 240);
pub const BUSY: Color = Color::Rgb(236, 178, 46);
pub const ONLINE: Color = Color::Rgb(46, 182, 125);
pub const DIM: Color = Color::Rgb(130, 130, 140);
pub const CODE: Color = Color::Rgb(232, 145, 45);
pub const ERROR: Color = Color::Rgb(224, 82, 82);
pub const LINK: Color = Color::Rgb(97, 175, 239);
pub const SUBAGENT: Color = Color::Rgb(160, 140, 230);

/// Author color by kind: `claude`, `codex`, `user`, `peer` (another session) or `subagent`.
pub fn kind_color(kind: &str) -> Color {
    match kind {
        "claude" | "peer" => CLAUDE,
        "codex" => CODEX,
        "user" => USER,
        "subagent" => SUBAGENT,
        _ => DIM,
    }
}
