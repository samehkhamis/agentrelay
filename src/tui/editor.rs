//! The single-line message composer.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::UnicodeWidthStr;

#[derive(Default)]
pub struct Editor {
    text: String,
    /// Cursor position in chars.
    cursor: usize,
    /// Shell mode: `!` typed into an empty line; the text is a command.
    pub shell: bool,
}

impl Editor {
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Nothing typed and not in shell mode.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && !self.shell
    }

    /// The entered line, `!`-prefixed in shell mode, leaving shell mode.
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        let text = std::mem::take(&mut self.text);
        if std::mem::take(&mut self.shell) { format!("!{text}") } else { text }
    }

    /// Replaces the line, e.g. to prefill a command.
    pub fn set(&mut self, text: &str) {
        self.text = text.to_string();
        self.cursor = text.chars().count();
        self.shell = false;
    }

    /// Inserts text at the cursor.
    pub fn insert(&mut self, text: &str) {
        let at = self.byte_at(self.cursor);
        self.text.insert_str(at, text);
        self.cursor += text.chars().count();
    }

    fn byte_at(&self, char_idx: usize) -> usize {
        self.text.char_indices().nth(char_idx).map_or(self.text.len(), |(b, _)| b)
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }

    /// Applies an editing key. False if the key isn't an editing key.
    pub fn handle(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        match k.code {
            // Like Claude Code: `!` on an empty line enters shell mode and
            // Backspace on an empty line leaves it.
            KeyCode::Char('!') if self.text.is_empty() && !self.shell => self.shell = true,
            KeyCode::Backspace if self.text.is_empty() && self.shell => self.shell = false,
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = self.len(),
            KeyCode::Char('u') if ctrl => {
                self.take();
            }
            KeyCode::Char('w') if ctrl => {
                let chars: Vec<char> = self.text.chars().collect();
                let mut i = self.cursor;
                while i > 0 && chars[i - 1] == ' ' {
                    i -= 1;
                }
                while i > 0 && chars[i - 1] != ' ' {
                    i -= 1;
                }
                self.text = chars[..i].iter().chain(&chars[self.cursor..]).collect();
                self.cursor = i;
            }
            KeyCode::Char(c) if !ctrl && !alt => {
                let at = self.byte_at(self.cursor);
                self.text.insert(at, c);
                self.cursor += 1;
            }
            KeyCode::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                let at = self.byte_at(self.cursor);
                self.text.remove(at);
            }
            KeyCode::Delete if self.cursor < self.len() => {
                let at = self.byte_at(self.cursor);
                self.text.remove(at);
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.len()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.len(),
            _ => return false,
        }
        true
    }

    /// The visible slice for a box `width` wide, scrolled so the cursor
    /// shows, and the cursor's column within it.
    pub fn view(&self, width: usize) -> (String, usize) {
        let before: String = self.text.chars().take(self.cursor).collect();
        let overflow = before.width().saturating_sub(width.saturating_sub(1));
        let mut skipped = 0;
        let shown: String = self
            .text
            .chars()
            .skip_while(|c| {
                let skip = skipped < overflow;
                if skip {
                    skipped += c.to_string().width();
                }
                skip
            })
            .collect();
        (shown, before.width() - skipped)
    }
}
