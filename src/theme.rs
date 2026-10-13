//! Color themes, shared by both UIs: the TUI paints with them and the web
//! page gets them as CSS variables. The active theme is a process-wide
//! setting the hub keeps in sync with the database.

use ratatui::style::Color;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct Theme {
    pub name: &'static str,
    /// Message pane.
    pub bg: Color,
    pub fg: Color,
    pub sidebar_bg: Color,
    pub sidebar_fg: Color,
    pub sidebar_dim: Color,
    pub selected_bg: Color,
    pub badge_bg: Color,
    pub claude: Color,
    pub codex: Color,
    pub user: Color,
    pub subagent: Color,
    pub busy: Color,
    pub online: Color,
    /// Secondary text: times, tags, tool lines.
    pub dim: Color,
    /// Inline code.
    pub code: Color,
    pub error: Color,
    pub link: Color,
    /// The syntect theme for code blocks.
    pub syntax: &'static str,
}

const fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

static THEMES: [Theme; 8] = [
    Theme {
        name: "slack",
        bg: rgb(0x1a1d21),
        fg: rgb(0xd1d2d3),
        sidebar_bg: rgb(0x3f0e40),
        sidebar_fg: rgb(0xcfc3cf),
        sidebar_dim: rgb(0xa087a0),
        selected_bg: rgb(0x1164a3),
        badge_bg: rgb(0xcd2553),
        claude: rgb(0xd97757),
        codex: rgb(0x10a37f),
        user: rgb(0x36c5f0),
        subagent: rgb(0xa08ce6),
        busy: rgb(0xecb22e),
        online: rgb(0x2eb67d),
        dim: rgb(0x8a8b8f),
        code: rgb(0xe8912d),
        error: rgb(0xe05252),
        link: rgb(0x61afef),
        syntax: "base16-ocean.dark",
    },
    Theme {
        name: "slack-light",
        bg: rgb(0xffffff),
        fg: rgb(0x1d1c1d),
        sidebar_bg: rgb(0x3f0e40),
        sidebar_fg: rgb(0xcfc3cf),
        sidebar_dim: rgb(0xa087a0),
        selected_bg: rgb(0x1164a3),
        badge_bg: rgb(0xcd2553),
        claude: rgb(0xc4562f),
        codex: rgb(0x0b8566),
        user: rgb(0x1264a3),
        subagent: rgb(0x7a5fd0),
        busy: rgb(0xc8901a),
        online: rgb(0x2eb67d),
        dim: rgb(0x616061),
        code: rgb(0xc0392b),
        error: rgb(0xd03030),
        link: rgb(0x1264a3),
        syntax: "InspiredGitHub",
    },
    Theme {
        name: "dracula",
        bg: rgb(0x282a36),
        fg: rgb(0xf8f8f2),
        sidebar_bg: rgb(0x21222c),
        sidebar_fg: rgb(0xf8f8f2),
        sidebar_dim: rgb(0x6272a4),
        selected_bg: rgb(0x44475a),
        badge_bg: rgb(0xff5555),
        claude: rgb(0xffb86c),
        codex: rgb(0x50fa7b),
        user: rgb(0x8be9fd),
        subagent: rgb(0xbd93f9),
        busy: rgb(0xf1fa8c),
        online: rgb(0x50fa7b),
        dim: rgb(0x6272a4),
        code: rgb(0xff79c6),
        error: rgb(0xff5555),
        link: rgb(0x8be9fd),
        syntax: "base16-eighties.dark",
    },
    Theme {
        name: "nord",
        bg: rgb(0x2e3440),
        fg: rgb(0xd8dee9),
        sidebar_bg: rgb(0x3b4252),
        sidebar_fg: rgb(0xe5e9f0),
        sidebar_dim: rgb(0x81a1c1),
        selected_bg: rgb(0x5e81ac),
        badge_bg: rgb(0xbf616a),
        claude: rgb(0xd08770),
        codex: rgb(0xa3be8c),
        user: rgb(0x88c0d0),
        subagent: rgb(0xb48ead),
        busy: rgb(0xebcb8b),
        online: rgb(0xa3be8c),
        dim: rgb(0x7b88a1),
        code: rgb(0xebcb8b),
        error: rgb(0xbf616a),
        link: rgb(0x88c0d0),
        syntax: "base16-ocean.dark",
    },
    Theme {
        name: "gruvbox",
        bg: rgb(0x282828),
        fg: rgb(0xebdbb2),
        sidebar_bg: rgb(0x1d2021),
        sidebar_fg: rgb(0xebdbb2),
        sidebar_dim: rgb(0x928374),
        selected_bg: rgb(0x504945),
        badge_bg: rgb(0xfb4934),
        claude: rgb(0xfe8019),
        codex: rgb(0xb8bb26),
        user: rgb(0x83a598),
        subagent: rgb(0xd3869b),
        busy: rgb(0xfabd2f),
        online: rgb(0xb8bb26),
        dim: rgb(0x928374),
        code: rgb(0xfabd2f),
        error: rgb(0xfb4934),
        link: rgb(0x8ec07c),
        syntax: "base16-mocha.dark",
    },
    Theme {
        name: "tokyo-night",
        bg: rgb(0x1a1b26),
        fg: rgb(0xc0caf5),
        sidebar_bg: rgb(0x16161e),
        sidebar_fg: rgb(0xa9b1d6),
        sidebar_dim: rgb(0x565f89),
        selected_bg: rgb(0x283457),
        badge_bg: rgb(0xf7768e),
        claude: rgb(0xff9e64),
        codex: rgb(0x9ece6a),
        user: rgb(0x7aa2f7),
        subagent: rgb(0xbb9af7),
        busy: rgb(0xe0af68),
        online: rgb(0x9ece6a),
        dim: rgb(0x565f89),
        code: rgb(0x7dcfff),
        error: rgb(0xf7768e),
        link: rgb(0x7aa2f7),
        syntax: "base16-ocean.dark",
    },
    Theme {
        name: "catppuccin",
        bg: rgb(0x1e1e2e),
        fg: rgb(0xcdd6f4),
        sidebar_bg: rgb(0x181825),
        sidebar_fg: rgb(0xcdd6f4),
        sidebar_dim: rgb(0x7f849c),
        selected_bg: rgb(0x45475a),
        badge_bg: rgb(0xf38ba8),
        claude: rgb(0xfab387),
        codex: rgb(0xa6e3a1),
        user: rgb(0x89dceb),
        subagent: rgb(0xcba6f7),
        busy: rgb(0xf9e2af),
        online: rgb(0xa6e3a1),
        dim: rgb(0x7f849c),
        code: rgb(0xf5c2e7),
        error: rgb(0xf38ba8),
        link: rgb(0x89b4fa),
        syntax: "base16-mocha.dark",
    },
    Theme {
        name: "solarized",
        bg: rgb(0x002b36),
        fg: rgb(0x93a1a1),
        sidebar_bg: rgb(0x073642),
        sidebar_fg: rgb(0x93a1a1),
        sidebar_dim: rgb(0x586e75),
        selected_bg: rgb(0x268bd2),
        badge_bg: rgb(0xdc322f),
        claude: rgb(0xcb4b16),
        codex: rgb(0x859900),
        user: rgb(0x2aa198),
        subagent: rgb(0x6c71c4),
        busy: rgb(0xb58900),
        online: rgb(0x859900),
        dim: rgb(0x657b83),
        code: rgb(0xb58900),
        error: rgb(0xdc322f),
        link: rgb(0x268bd2),
        syntax: "Solarized (dark)",
    },
];

static CURRENT: AtomicUsize = AtomicUsize::new(0);

/// The active theme.
pub fn t() -> &'static Theme {
    &THEMES[CURRENT.load(Ordering::Relaxed)]
}

/// Makes `name` active. False if there's no such theme.
pub fn set(name: &str) -> bool {
    match THEMES.iter().position(|th| th.name == name) {
        Some(i) => {
            CURRENT.store(i, Ordering::Relaxed);
            true
        }
        None => false,
    }
}

pub fn names() -> Vec<&'static str> {
    THEMES.iter().map(|th| th.name).collect()
}

/// Author color by kind: `claude`, `codex`, `user`, `peer` (another session) or `subagent`.
pub fn kind_color(kind: &str) -> Color {
    let th = t();
    match kind {
        "claude" | "peer" => th.claude,
        "codex" => th.codex,
        "user" => th.user,
        "subagent" => th.subagent,
        _ => th.dim,
    }
}
