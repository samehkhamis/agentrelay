//! Recognizing files dropped onto the terminal, which arrive as pasted paths:
//! POSIX paths (quoted or with escaped spaces), Windows paths (converted to
//! their WSL mount), `\\wsl.localhost\<distro>\…` paths and `file://` URIs.

use std::path::PathBuf;

/// The files in a paste, if it consists only of paths to existing files.
pub fn files(text: &str) -> Option<Vec<PathBuf>> {
    let paths: Vec<PathBuf> = split(text.trim()).iter().map(|t| to_local(t)).collect();
    (!paths.is_empty() && paths.iter().all(|p| p.is_file())).then_some(paths)
}

/// Splits on whitespace, honoring quotes; a backslash only escapes a space
/// or quote, so Windows separators survive.
fn split(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote = None;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => quote = Some(c),
            (None, '\\') if matches!(chars.peek(), Some(' ' | '"' | '\'')) => cur.push(chars.next().unwrap()),
            (None, c) if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn to_local(token: &str) -> PathBuf {
    let token = token.strip_prefix("file://").unwrap_or(token);
    let bytes = token.as_bytes();
    if bytes.len() > 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && matches!(bytes[2], b'\\' | b'/') {
        let drive = (bytes[0] as char).to_ascii_lowercase();
        return PathBuf::from(format!("/mnt/{drive}/{}", token[3..].replace('\\', "/")));
    }
    for unc in [r"\\wsl.localhost\", r"\\wsl$\"] {
        if let Some(rest) = token.strip_prefix(unc) {
            let inner = rest.split_once('\\').map_or("", |(_, p)| p);
            return PathBuf::from(format!("/{}", inner.replace('\\', "/")));
        }
    }
    PathBuf::from(token)
}

#[cfg(test)]
mod tests {
    use super::{split, to_local};
    use std::path::PathBuf;

    #[test]
    fn splits_quoted_and_escaped_paths() {
        assert_eq!(split(r#"'/tmp/a b.png' /tmp/c\ d.txt "C:\x y\z.png""#), ["/tmp/a b.png", "/tmp/c d.txt", r"C:\x y\z.png"]);
    }

    #[test]
    fn converts_windows_and_wsl_paths() {
        assert_eq!(to_local(r"C:\Users\me\shot.png"), PathBuf::from("/mnt/c/Users/me/shot.png"));
        assert_eq!(to_local(r"\\wsl.localhost\Ubuntu\home\me\a.txt"), PathBuf::from("/home/me/a.txt"));
        assert_eq!(to_local("file:///tmp/x.png"), PathBuf::from("/tmp/x.png"));
    }
}
