//! What a composer line means: slash commands, `!` shell commands or a
//! message, plus autocomplete for the slash commands.

use crate::agents::Kind;
use crate::{db, theme};

const STATUS_ASK: &str = "Quick status update please: what are you working on, what's done, \
                          and is anything blocked? 2-3 lines.";
/// Commands: (name, usage after the name, what it does). `!` isn't a slash
/// command, so autocomplete skips it, but help lists it.
const COMMANDS: [(&str, &str, &str); 8] = [
    ("/status", "", "ask everyone here for a status update"),
    ("/new", "claude|codex [folder] [prompt]", "start an agent"),
    ("/rename", "<name>", "rename this DM's conversation"),
    ("/open", "<file>", "open a file from this agent's folder"),
    ("/theme", "[name]", "switch the theme for every UI"),
    ("/sound", "", "turn the reply chime on or off"),
    ("/quit", "", "leave agentrelay"),
    ("!", "<command>", "run it for a DM's agent (shell mode)"),
];

/// One line listing every command, for `/help`-style notes.
pub fn help() -> String {
    let all: Vec<String> = COMMANDS
        .iter()
        .map(|(name, usage, _)| match (*name, usage.is_empty()) {
            (_, true) => name.to_string(),
            ("!", false) => format!("!{usage}"),
            (_, false) => format!("{name} {usage}"),
        })
        .collect();
    format!("commands: {}", all.join("  "))
}

/// One autocomplete suggestion.
#[derive(serde::Serialize)]
pub struct Completion {
    /// The whole line once accepted.
    pub line: String,
    pub label: String,
    pub detail: String,
    /// Accepting it is a complete command: run it rather than keep typing.
    pub run: bool,
}

/// Suggestions for a composer line starting with `/`: commands, then their
/// arguments (agent kinds, theme names, folders, and for `/open` the fuzzy
/// matches among `files`).
pub fn completions(line: &str, folders: &[String], files: &[String]) -> Vec<Completion> {
    let item = |line: String, label: &str, detail: &str, run: bool| Completion {
        line,
        label: label.to_string(),
        detail: detail.to_string(),
        run,
    };
    let Some(rest) = line.strip_prefix('/') else { return Vec::new() };
    let Some((cmd, arg)) = rest.split_once(' ') else {
        return COMMANDS
            .iter()
            .filter(|(name, ..)| name.starts_with('/') && name[1..].starts_with(rest))
            .map(|(name, usage, what)| {
                let label = if usage.is_empty() { name.to_string() } else { format!("{name} {usage}") };
                let line = if usage.is_empty() { name.to_string() } else { format!("{name} ") };
                item(line, &label, what, usage.is_empty())
            })
            .collect();
    };
    match cmd {
        "open" => crate::files::fuzzy(files, arg.trim(), 12)
            .into_iter()
            .map(|f| item(format!("/open {f}"), f, "", true))
            .collect(),
        "theme" => theme::names()
            .into_iter()
            .filter(|n| n.starts_with(arg.trim()) && *n != arg.trim())
            .map(|n| item(format!("/theme {n}"), n, if n == theme::t().name { "current" } else { "" }, true))
            .collect(),
        "new" => match arg.split_once(' ') {
            None => ["claude", "codex"]
                .into_iter()
                .filter(|k| k.starts_with(arg) && *k != arg)
                .map(|k| item(format!("/new {k} "), k, "then a folder and a first message, both optional", false))
                .collect(),
            Some((kind, folder)) if folder.starts_with(['~', '/', '.']) && !folder.contains(' ') => {
                folders
                    .iter()
                    .map(|f| db::tilde(f))
                    .filter(|f| f.starts_with(folder) && f != folder)
                    .take(12)
                    .map(|f| item(format!("/new {kind} {f} "), &f, "", false))
                    .collect()
            }
            Some(_) => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// What the user typed in a composer.
pub enum Input {
    Send(String),
    /// `/new claude|codex [folder] [prompt]`: start an agent, idle without a prompt.
    New { kind: Kind, folder: Option<String>, prompt: String },
    /// `!command`: run it in a DM's agent's folder.
    Shell(String),
    /// `/rename <name>`: rename a DM's conversation.
    Rename(String),
    /// `/open <file>`: view a file from the channel's folder.
    Open(String),
    /// `/theme name`, or `/theme` alone to list them.
    Theme(Option<String>),
    ToggleSound,
    Quit,
    Help,
}

pub fn parse_input(text: &str) -> Option<Input> {
    let text = text.trim();
    if let Some(cmd) = text.strip_prefix('!') {
        return (!cmd.trim().is_empty()).then(|| Input::Shell(cmd.trim().to_string()));
    }
    if let Some(file) = text.strip_prefix("/open") {
        let file = file.trim();
        return Some(if file.is_empty() { Input::Help } else { Input::Open(file.to_string()) });
    }
    if let Some(name) = text.strip_prefix("/rename") {
        let name = name.trim();
        return Some(if name.is_empty() { Input::Help } else { Input::Rename(name.to_string()) });
    }
    if let Some(rest) = text.strip_prefix("/new ") {
        let (kind, rest) = rest.trim().split_once(' ').unwrap_or((rest.trim(), ""));
        let kind = match kind {
            "claude" => Kind::Claude,
            "codex" => Kind::Codex,
            _ => return Some(Input::Help),
        };
        let rest = rest.trim();
        let (folder, prompt) = match rest.split_once(' ') {
            Some((f, p)) if f.starts_with(['/', '~', '.']) => (Some(f.to_string()), p.trim()),
            _ if rest.starts_with(['/', '~', '.']) => (Some(rest.to_string()), ""),
            _ => (None, rest),
        };
        return Some(Input::New { kind, folder, prompt: prompt.to_string() });
    }
    if let Some(rest) = text.strip_prefix("/theme") {
        let name = rest.trim();
        return Some(Input::Theme((!name.is_empty()).then(|| name.to_string())));
    }
    Some(match text {
        "" => return None,
        "/quit" | "/q" => Input::Quit,
        "/sound" => Input::ToggleSound,
        "/status" => Input::Send(STATUS_ASK.into()),
        t if t.starts_with('/') => Input::Help,
        t => Input::Send(t.into()),
    })
}

/// The `/theme` reply: the themes, the active one marked.
pub fn theme_list() -> String {
    let names: Vec<String> = theme::names()
        .into_iter()
        .map(|n| if n == theme::t().name { format!("[{n}]") } else { n.to_string() })
        .collect();
    format!("themes: {}", names.join(" "))
}

#[cfg(test)]
mod tests {
    use super::{parse_input, Input};
    use crate::agents::Kind;

    #[test]
    fn completes_commands_and_arguments() {
        use super::completions;
        let files = ["src/main.rs".to_string(), "README.md".to_string()];
        let labels = |line: &str| completions(line, &["/home/x/code/api".into()], &files).into_iter().map(|c| c.line).collect::<Vec<_>>();
        assert_eq!(labels("/s"), ["/status", "/sound"]);
        assert_eq!(labels("/re"), ["/rename "]);
        assert_eq!(labels("/open main"), ["/open src/main.rs"]);
        assert_eq!(labels("/new c"), ["/new claude ", "/new codex "]);
        assert_eq!(labels("/theme dra"), ["/theme dracula"]);
        assert!(labels("hello").is_empty());
    }

    #[test]
    fn parses_new_and_shell() {
        match parse_input("/new codex ~/code/infra fix the CI") {
            Some(Input::New { kind: Kind::Codex, folder: Some(f), prompt }) => {
                assert_eq!((f.as_str(), prompt.as_str()), ("~/code/infra", "fix the CI"))
            }
            _ => panic!("expected /new with a folder"),
        }
        assert!(matches!(parse_input("/new claude look around"), Some(Input::New { folder: None, .. })));
        assert!(matches!(parse_input("!git status"), Some(Input::Shell(c)) if c == "git status"));
    }
}
