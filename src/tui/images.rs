//! Inline images in the terminal via ratatui-image: the Kitty graphics
//! protocol where the terminal supports it, else Sixel or iTerm2, else
//! colored half blocks. The protocol is detected once at startup by asking
//! the terminal. `AGENTCHAT_IMAGES` overrides it: `kitty`, `sixel`, `iterm2`,
//! `halfblocks`, or `off` for plain `📎 name` lines.

use ratatui::layout::Rect;
use ratatui::Frame;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::StatefulProtocol;
use ratatui_image::StatefulImage;
use std::collections::HashMap;
use std::path::Path;

/// Largest image, in terminal rows and columns.
const MAX_ROWS: u16 = 14;
const MAX_COLS: u16 = 60;
/// Decoded images kept ready to draw.
const KEEP: usize = 48;

pub struct Images {
    picker: Picker,
    /// Whether the terminal answered the capability query. Through Windows'
    /// ConPTY (e.g. WezTerm on Windows running WSL) it never does.
    pub answered: bool,
    /// Pixel size per image, read from its header.
    dims: HashMap<String, Option<(u32, u32)>>,
    ready: HashMap<String, StatefulProtocol>,
}

impl Images {
    /// Asks the terminal what it supports. Call after entering the alternate
    /// screen and before reading events. None if images are turned off.
    pub fn detect() -> Option<Images> {
        let forced = std::env::var("AGENTCHAT_IMAGES").unwrap_or_default().to_ascii_lowercase();
        if forced == "off" || forced == "0" {
            return None;
        }
        let mut picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
        let answered = !picker.capabilities().is_empty();
        let protocol = match forced.as_str() {
            "kitty" => Some(ProtocolType::Kitty),
            "sixel" => Some(ProtocolType::Sixel),
            "iterm2" => Some(ProtocolType::Iterm2),
            "halfblocks" => Some(ProtocolType::Halfblocks),
            // Without an answer the picker guesses from env vars (e.g. iTerm2
            // for TERM_PROGRAM=WezTerm), which blanks images if wrong; half
            // blocks always show.
            _ if !answered => Some(ProtocolType::Halfblocks),
            _ => None,
        };
        if let Some(p) = protocol {
            picker.set_protocol_type(p);
        }
        Some(Images { picker, answered, dims: HashMap::new(), ready: HashMap::new() })
    }

    /// What was detected, for `agentchat doctor`.
    pub fn describe(&self) -> String {
        let font = self.picker.font_size();
        format!(
            "protocol: {:?}\nterminal answered the query: {}\ncell size: {}x{} px\ncapabilities: {:?}",
            self.picker.protocol_type(),
            if self.answered { "yes" } else { "no" },
            font.width,
            font.height,
            self.picker.capabilities(),
        )
    }

    /// The protocol in use, e.g. `Kitty`.
    pub fn protocol(&self) -> String {
        format!("{:?}", self.picker.protocol_type())
    }

    /// (columns, rows) to reserve for an image within `max_cols`, keeping its
    /// aspect ratio given the terminal's cell size. None if it isn't an image.
    pub fn size(&mut self, path: &str, max_cols: u16) -> Option<(u16, u16)> {
        let (w, h) = (*self.dims.entry(path.to_string()).or_insert_with(|| image::image_dimensions(path).ok()))?;
        let font = self.picker.font_size();
        let (cw, ch) = (f64::from(font.width.max(1)), f64::from(font.height.max(1)));
        let mut cols = (f64::from(w) / cw).ceil().min(f64::from(max_cols.min(MAX_COLS))).max(1.0);
        let mut rows = (cols * cw * f64::from(h) / f64::from(w.max(1)) / ch).ceil();
        if rows > f64::from(MAX_ROWS) {
            rows = f64::from(MAX_ROWS);
            cols = (rows * ch * f64::from(w) / f64::from(h.max(1)) / cw).ceil().max(1.0);
        }
        Some((cols as u16, rows.max(1.0) as u16))
    }

    /// Draws an image into `area`, decoding it on first use.
    pub fn draw(&mut self, f: &mut Frame, area: Rect, path: &str) {
        if !self.ready.contains_key(path) {
            let Ok(img) = image::open(Path::new(path)) else { return };
            if self.ready.len() >= KEEP {
                self.ready.clear();
            }
            self.ready.insert(path.to_string(), self.picker.new_resize_protocol(img));
        }
        if let Some(state) = self.ready.get_mut(path) {
            f.render_stateful_widget(StatefulImage::default(), area, state);
        }
    }
}
