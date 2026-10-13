//! Recognizing file paths that agents mention (in inline code or tool calls)
//! and resolving them against the agent's folder. A mention only counts if
//! the file exists, so links are never guesses.

use crate::db;
use std::path::PathBuf;

/// A mention resolved to a file, with the line it pointed at (`main.rs:42`).
#[derive(Clone, Debug, PartialEq)]
pub struct FileRef {
    pub path: PathBuf,
    pub line: Option<usize>,
}

/// Resolves `raw` (`src/main.rs`, `./a.txt:12`, `~/x/y.md`, `/abs/z.rs:3-9`)
/// against `cwd`. None unless it names an existing file.
pub fn resolve(cwd: &str, raw: &str) -> Option<FileRef> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 400 || raw.contains(char::is_whitespace) || !raw.contains(['/', '.']) {
        return None;
    }
    let (name, line) = split_line(raw);
    let path = match name.strip_prefix('~') {
        Some(_) => db::expand_tilde(name),
        None if name.starts_with('/') => PathBuf::from(name),
        None if !cwd.is_empty() => PathBuf::from(cwd).join(name.trim_start_matches("./")),
        None => return None,
    };
    path.is_file().then_some(FileRef { path, line })
}

/// `a.rs:42` -> (`a.rs`, 42); also `a.rs:42-50` and `a.rs:42:7`.
fn split_line(raw: &str) -> (&str, Option<usize>) {
    let mut parts = raw.splitn(2, ':');
    let name = parts.next().unwrap_or(raw);
    let line = parts.next().and_then(|rest| rest.split(['-', ':']).next()?.parse().ok());
    if line.is_some() { (name, line) } else { (raw, None) }
}

/// Files listed per folder, at most.
const LIST_MAX: usize = 50_000;

/// A folder's files, relative to it: a parallel walk with the `ignore` crate
/// (what fd and ripgrep use), skipping `.gitignore`d and hidden files.
pub fn list(root: &std::path::Path) -> Vec<String> {
    let found = std::sync::Mutex::new(Vec::new());
    ignore::WalkBuilder::new(root).build_parallel().run(|| {
        let found = &found;
        Box::new(move |entry| {
            let Ok(e) = entry else { return ignore::WalkState::Continue };
            if e.file_type().is_some_and(|t| t.is_file()) {
                let mut found = found.lock().unwrap();
                if found.len() >= LIST_MAX {
                    return ignore::WalkState::Quit;
                }
                if let Ok(rel) = e.path().strip_prefix(root) {
                    found.push(rel.to_string_lossy().into_owned());
                }
            }
            ignore::WalkState::Continue
        })
    });
    let mut files = found.into_inner().unwrap();
    files.sort();
    files
}

/// The best fuzzy matches for `query` among `files` (Helix's matcher, tuned
/// for paths), best first.
pub fn fuzzy<'a>(files: &'a [String], query: &str, limit: usize) -> Vec<&'a str> {
    use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
    let mut matcher = nucleo_matcher::Matcher::new(nucleo_matcher::Config::DEFAULT.match_paths());
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let mut hits = pattern.match_list(files.iter().map(String::as_str), &mut matcher);
    hits.truncate(limit);
    hits.into_iter().map(|(f, _)| f).collect()
}

/// Up to this much of a file is shown.
pub const VIEW_MAX: usize = 512 << 10;

/// A file's text for viewing; None if it's binary.
pub fn read_text(path: &std::path::Path) -> Option<String> {
    let mut bytes = std::fs::read(path).ok()?;
    bytes.truncate(VIEW_MAX);
    (!bytes.contains(&0)).then(|| String::from_utf8_lossy(&bytes).into_owned())
}

/// The extension, for highlighting.
pub fn lang(path: &std::path::Path) -> String {
    path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Markdown files get a rendered preview beside their source.
pub fn is_markdown(path: &std::path::Path) -> bool {
    matches!(lang(path).to_ascii_lowercase().as_str(), "md" | "markdown" | "mdx")
}

#[cfg(test)]
mod tests {
    use super::{resolve, split_line};

    #[test]
    fn splits_line_numbers() {
        assert_eq!(split_line("src/a.rs:42"), ("src/a.rs", Some(42)));
        assert_eq!(split_line("src/a.rs:42-50"), ("src/a.rs", Some(42)));
        assert_eq!(split_line("src/a.rs"), ("src/a.rs", None));
        assert_eq!(split_line("http://x"), ("http://x", None));
    }

    #[test]
    fn lists_and_fuzzy_finds() {
        let files = super::list(std::path::Path::new(env!("CARGO_MANIFEST_DIR")));
        assert!(files.iter().any(|f| f == "src/files.rs"));
        assert!(!files.iter().any(|f| f.starts_with("target/"))); // gitignored
        assert_eq!(super::fuzzy(&files, "tuimarkdown", 1), ["src/tui/markdown.rs"]);
    }

    #[test]
    fn only_existing_files_resolve() {
        let cwd = env!("CARGO_MANIFEST_DIR");
        assert!(resolve(cwd, "src/files.rs:10").is_some_and(|f| f.line == Some(10)));
        assert!(resolve(cwd, "src/nope.rs").is_none());
        assert!(resolve(cwd, "cargo build").is_none());
        assert!(resolve(cwd, "src").is_none()); // a folder
    }
}
