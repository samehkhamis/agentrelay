//! Images pasted into an agent are stored inline in its transcript; they are
//! written out to the uploads directory once (named by content hash) so the
//! UIs can show them like attachments.

use crate::uploads;
use base64::Engine;
use std::hash::{DefaultHasher, Hash, Hasher};

/// Stores a base64 image of `media_type` (e.g. `image/png`); returns its path.
pub fn store(media_type: &str, data: &str) -> Option<String> {
    let ext = match media_type {
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "png",
    };
    let mut h = DefaultHasher::new();
    data.hash(&mut h);
    let name = format!("image-{:016x}.{ext}", h.finish());
    let bytes = base64::engine::general_purpose::STANDARD.decode(data.trim()).ok()?;
    uploads::store_named(&name, &bytes).ok().map(|p| p.display().to_string())
}

/// Stores an image given as a `data:image/png;base64,…` URL.
pub fn store_data_url(url: &str) -> Option<String> {
    let (meta, data) = url.strip_prefix("data:")?.split_once(',')?;
    store(meta.strip_suffix(";base64")?, data)
}
