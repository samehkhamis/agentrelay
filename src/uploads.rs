//! Files attached to messages. Each is copied into `<data dir>/uploads` under
//! a unique id, so both UIs can show it and the web server only ever serves
//! files from that one directory.

use crate::db;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// Largest attachment accepted.
pub const MAX_BYTES: usize = 25 << 20;

pub fn dir() -> PathBuf {
    db::data_dir().join("uploads")
}

/// Stores bytes under a new id derived from `name`; returns the stored path.
pub fn save(name: &str, bytes: &[u8]) -> Result<PathBuf> {
    if bytes.len() > MAX_BYTES {
        bail!("{name} is larger than {} MB", MAX_BYTES >> 20);
    }
    std::fs::create_dir_all(dir())?;
    let (ts, name) = (db::now_ms(), sanitize(name));
    // Same name in the same millisecond (two `shot.png`s dropped together):
    // bump the stamp rather than overwrite.
    for n in 0.. {
        let path = dir().join(format!("{}-{name}", ts + n));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => {
                std::io::Write::write_all(&mut f, bytes).with_context(|| format!("writing {}", path.display()))?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).with_context(|| format!("writing {}", path.display())),
        }
    }
    unreachable!()
}

/// The stored path for a fixed name, and whether it's already there.
pub fn named(name: &str) -> (PathBuf, bool) {
    let path = dir().join(sanitize(name));
    let exists = path.exists();
    (path, exists)
}

/// Stores bytes under a fixed name; returns the stored path.
pub fn store_named(name: &str, bytes: &[u8]) -> Result<PathBuf> {
    let (path, _) = named(name);
    std::fs::create_dir_all(dir())?;
    std::fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// Copies a local file into the uploads directory.
pub fn import(src: &Path) -> Result<PathBuf> {
    let name = src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
    save(&name, &std::fs::read(src).with_context(|| format!("reading {}", src.display()))?)
}

/// The stored file for an id (a bare file name in the uploads directory).
pub fn resolve(id: &str) -> Option<PathBuf> {
    let safe = !id.is_empty() && !id.contains(['/', '\\']) && !id.starts_with('.');
    let path = dir().join(id);
    (safe && path.is_file()).then_some(path)
}

/// The id of a stored file, if `path` is one.
pub fn id_of(path: &Path) -> Option<String> {
    (path.parent()? == dir()).then(|| path.file_name()?.to_str().map(str::to_string)).flatten()
}

/// The name the user gave it, without the id prefix.
pub fn display_name(path: &Path) -> String {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    match name.split_once('-') {
        Some((ts, rest)) if ts.chars().all(|c| c.is_ascii_digit()) => rest.to_string(),
        _ => name,
    }
}

pub fn is_image(path: &Path) -> bool {
    mime(path).starts_with("image/")
}

pub fn mime(path: &Path) -> &'static str {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "txt" | "md" | "log" | "csv" | "json" | "toml" | "yaml" | "yml" | "rs" | "py" | "js" | "ts" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn sanitize(name: &str) -> String {
    let clean: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '_' })
        .collect();
    let clean = clean.trim_start_matches('.');
    if clean.is_empty() { "file".into() } else { clean.chars().take(100).collect() }
}

#[cfg(test)]
mod tests {
    use super::{display_name, resolve, sanitize};
    use std::path::Path;

    #[test]
    fn ids_cannot_escape_the_directory() {
        assert!(resolve("../chat.db").is_none());
        assert!(resolve(".hidden").is_none());
        assert_eq!(sanitize("../../etc/passwd"), "_.._etc_passwd");
    }

    #[test]
    fn display_name_strips_the_id() {
        assert_eq!(display_name(Path::new("/x/1791855878462-shot 1.png")), "shot 1.png");
        assert_eq!(display_name(Path::new("/x/notes-v2.md")), "notes-v2.md");
    }
}
