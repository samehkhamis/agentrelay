//! The terminal UI: UI-only state (selection, scroll, composer, notices) over
//! the shared [`Hub`]. Drawing lives in `view`.
//!
//! Dragging files onto the terminal pastes their paths; a paste made only of
//! existing file paths attaches those files instead of inserting text.

mod editor;
mod feed;
mod images;
mod markdown;
mod paste;
mod view;
mod viewer;

use crate::hub::{self, Hub, Input};
use crate::{sound, theme, uploads};
use anyhow::Result;
use editor::Editor;
use hub::channels::Channel;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event, KeyCode,
    KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Position, Rect};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(400);
/// The sidebar's "＋ new agent" row, clickable like a channel.
const NEW_AGENT: &str = "+new";

/// What background work reports back.
enum Done {
    /// A send's report.
    Delivered(crate::ipc::Report),
    /// A task's one-line note.
    Note(String),
}

pub struct App {
    hub: Hub,
    /// Id of the open channel.
    selected: String,
    editor: Editor,
    /// Highlighted autocomplete suggestion, and the line Esc dismissed them for.
    suggestion: usize,
    dismissed: Option<String>,
    /// Files to attach to the next message.
    pending: Vec<PathBuf>,
    /// Lines scrolled up from the bottom of the feed.
    scroll: usize,
    feed_height: usize,
    sidebar_area: Rect,
    feed_area: Rect,
    /// Expanded tool calls, by post key; and the tool lines on screen
    /// (screen row, key) plus every tool in the open feed, for clicks and Ctrl+E.
    expanded: HashSet<String>,
    tool_rows: Vec<(u16, feed::ToolRow)>,
    feed_tools: Vec<String>,
    /// The open file pane, and the Ctrl+O file list (mentions, highlighted).
    viewer: Option<viewer::Viewer>,
    viewer_area: Rect,
    picker: Option<(Vec<String>, usize)>,
    /// Keyboard selection: the selected post's key, the feed's posts in
    /// order, and its tool calls by key.
    cursor: Option<String>,
    /// The selection the view was last positioned for.
    cursor_placed: Option<String>,
    feed_items: Vec<String>,
    tool_info: std::collections::HashMap<String, feed::ToolRow>,
    /// Sidebar screen row -> channel id, for clicks.
    sidebar_rows: Vec<(u16, String)>,
    toast: Option<(String, Instant)>,
    sound: bool,
    /// Agents messaged that haven't replied since; their next reply pings.
    awaiting: HashSet<String>,
    cache: feed::Cache,
    /// Inline images; None when turned off.
    images: Option<images::Images>,
    /// The theme the cache was rendered in.
    cache_theme: &'static str,
    done_tx: Sender<Done>,
    done_rx: Receiver<Done>,
    quit: bool,
}

pub fn run() -> Result<()> {
    let mut app = App::new()?;
    let mut terminal = ratatui::init();
    app.images = images::Images::detect(); // queries the terminal, so after init
    execute!(std::io::stdout(), EnableMouseCapture, EnableBracketedPaste)?;
    let res = app.event_loop(&mut terminal);
    let _ = execute!(std::io::stdout(), DisableMouseCapture, DisableBracketedPaste);
    ratatui::restore();
    res
}

impl App {
    fn new() -> Result<App> {
        let (done_tx, done_rx) = mpsc::channel();
        Ok(App {
            hub: Hub::open()?,
            selected: "all".into(),
            editor: Editor::default(),
            suggestion: 0,
            dismissed: None,
            pending: Vec::new(),
            scroll: 0,
            feed_height: 10,
            sidebar_area: Rect::default(),
            feed_area: Rect::default(),
            sidebar_rows: Vec::new(),
            expanded: HashSet::new(),
            tool_rows: Vec::new(),
            feed_tools: Vec::new(),
            viewer: None,
            viewer_area: Rect::default(),
            picker: None,
            cursor: None,
            cursor_placed: None,
            feed_items: Vec::new(),
            tool_info: Default::default(),
            toast: Some(("discovering agents…".into(), Instant::now())),
            sound: sound::enabled_by_default(),
            awaiting: HashSet::new(),
            cache: feed::Cache::default(),
            images: None,
            cache_theme: theme::t().name,
            done_tx,
            done_rx,
            quit: false,
        })
    }

    fn event_loop(&mut self, terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
        let mut last_poll = Instant::now();
        while !self.quit {
            terminal.draw(|f| self.draw(f))?;
            if event::poll(Duration::from_millis(200))? {
                match event::read()? {
                    Event::Key(k) if k.kind != KeyEventKind::Release => self.on_key(k)?,
                    Event::Mouse(m) => self.on_mouse(m.kind, m.column, m.row),
                    Event::Paste(text) => self.on_paste(&text),
                    _ => {}
                }
            }
            if last_poll.elapsed() >= POLL {
                if let Err(e) = self.poll() {
                    self.notify(format!("couldn't refresh: {e}")); // e.g. the database busy; try again
                }
                last_poll = Instant::now();
            }
        }
        Ok(())
    }

    fn current(&self) -> &Channel {
        self.hub.channel(&self.selected).unwrap_or(&self.hub.channels[0])
    }

    fn notify(&mut self, text: impl Into<String>) {
        self.toast = Some((text.into(), Instant::now()));
    }

    fn poll(&mut self) -> Result<()> {
        let was_empty = self.hub.agents.is_empty();
        let replied = self.hub.poll()?;
        if was_empty && !self.hub.agents.is_empty() {
            let images = self.images.as_ref().map_or("off".into(), |im| im.protocol());
            self.notify(format!("{} agents online · images: {images}", self.hub.agents.len()));
        }
        if let Some(name) = replied.iter().find(|n| self.awaiting.contains(*n)) {
            if self.sound {
                sound::ping();
            }
            self.notify(format!("{name} replied"));
        }
        for name in &replied {
            self.awaiting.remove(name);
        }
        while let Ok(done) = self.done_rx.try_recv() {
            match done {
                Done::Delivered(report) => {
                    self.notify(report.note());
                    self.awaiting.extend(report.delivered);
                }
                Done::Note(note) => self.notify(note),
            }
        }
        self.hub.follow(&self.selected);
        // Rendered lines carry colors; redo them when any UI switches theme.
        if self.cache_theme != theme::t().name {
            self.cache_theme = theme::t().name;
            self.cache = feed::Cache::default();
        }
        Ok(())
    }

    fn select(&mut self, id: &str) {
        self.cursor = None;
        if id == NEW_AGENT {
            self.editor.set("/new claude ");
            self.notify("/new claude|codex [folder] [prompt] · Enter to start");
            return;
        }
        if id != self.selected {
            self.selected = id.to_string();
            self.scroll = 0;
            self.hub.follow(id);
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let chans = &self.hub.channels;
        let i = chans.iter().position(|c| c.id == self.selected).unwrap_or(0) as isize;
        let next = chans[(i + delta).rem_euclid(chans.len() as isize) as usize].id.clone();
        self.select(&next);
    }

    fn submit(&mut self) -> Result<()> {
        match hub::parse_input(&self.editor.take()) {
            None if self.pending.is_empty() => {}
            None => self.send(String::new())?,
            Some(Input::Send(body)) => self.send(body)?,
            Some(Input::New { kind, folder, prompt }) => {
                let task = self.hub.start_agent(&self.selected, kind, folder.as_deref(), &prompt);
                self.background(task, format!("starting {}…", kind.as_str()));
            }
            Some(Input::Rename(name)) => {
                let renamed = self.hub.rename(&self.selected.clone(), &name);
                let task = renamed.map(|(task, follow)| {
                    self.selected = follow; // a renamed Claude DM reappears under its new name
                    task
                });
                self.background(task, "renaming…".into());
            }
            Some(Input::Shell(cmd)) => {
                let task = self.hub.shell(&self.selected, &cmd);
                self.background(task, "running…".into());
            }
            Some(Input::Quit) => self.quit = true,
            Some(Input::Help) => self.notify(hub::help()),
            Some(Input::Theme(None)) => self.notify(hub::theme_list()),
            Some(Input::Theme(Some(name))) => match self.hub.set_theme(&name)? {
                true => self.notify(format!("theme: {name}")),
                false => self.notify(format!("no theme {name}; {}", hub::theme_list())),
            },
            Some(Input::ToggleSound) => {
                self.sound = !self.sound;
                self.notify(format!("sound {}", if self.sound { "on" } else { "off" }));
                if self.sound {
                    sound::ping();
                }
            }
        }
        Ok(())
    }

    /// Runs a hub task off the UI thread; its note shows when done.
    fn background(&mut self, task: Result<hub::Task>, starting: String) {
        match task {
            Ok(task) => {
                self.notify(starting);
                let tx = self.done_tx.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(Done::Note(task.run()));
                });
            }
            Err(e) => self.notify(e.to_string()),
        }
    }

    /// Logs the message with any pending files and delivers it in the background.
    fn send(&mut self, body: String) -> Result<()> {
        let files = match self.pending.drain(..).map(|p| uploads::import(&p)).collect::<Result<Vec<_>>>() {
            Ok(files) => files,
            Err(e) => {
                self.notify(format!("couldn't attach: {e}"));
                return Ok(());
            }
        };
        self.scroll = 0;
        match self.hub.send(&self.selected.clone(), &body, &files)? {
            None => self.notify("nobody online here to deliver to"),
            Some(delivery) => {
                self.notify(format!("delivering to {}…", delivery.len()));
                let tx = self.done_tx.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(Done::Delivered(delivery.run()));
                });
            }
        }
        Ok(())
    }

    fn on_paste(&mut self, text: &str) {
        match paste::files(text) {
            Some(files) => {
                self.notify(format!("attached {} file(s) · Backspace removes", files.len()));
                self.pending.extend(files);
            }
            None => self.editor.insert(&text.replace(['\r', '\n'], " ")),
        }
    }

    /// Autocomplete suggestions for the composer line, unless dismissed.
    fn suggestions(&self) -> Vec<hub::Completion> {
        let text = self.editor.text();
        if self.editor.shell || self.dismissed.as_deref() == Some(text) {
            return Vec::new();
        }
        self.hub.complete(text)
    }

    /// Keys for an open suggestion list. False if the key isn't one of them.
    fn on_suggestion_key(&mut self, k: KeyEvent) -> Result<bool> {
        let list = self.suggestions();
        if list.is_empty() {
            return Ok(false);
        }
        let n = list.len();
        match k.code {
            KeyCode::Up => self.suggestion = (self.suggestion + n - 1) % n,
            KeyCode::Down => self.suggestion = (self.suggestion + 1) % n,
            KeyCode::Esc => self.dismissed = Some(self.editor.text().to_string()),
            KeyCode::Tab | KeyCode::Enter => {
                let pick = &list[self.suggestion.min(n - 1)];
                self.editor.set(&pick.line);
                self.suggestion = 0;
                if pick.run && k.code == KeyCode::Enter {
                    self.submit()?;
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Opens a file the open feed mentions in the file pane.
    fn open_file(&mut self, raw: &str) {
        let opened = self.hub.channel(&self.selected).map(|c| self.hub.open_file(c, raw));
        match opened {
            Some(Ok((file, text))) => self.viewer = Some(viewer::Viewer::new(&file, &text)),
            Some(Err(e)) => self.notify(e.to_string()),
            None => {}
        }
    }

    /// Keys while the Ctrl+O file list is open. False if the list isn't open.
    fn on_picker_key(&mut self, k: KeyEvent) -> bool {
        let Some((items, sel)) = self.picker.as_mut() else { return false };
        let n = items.len().max(1);
        match k.code {
            KeyCode::Up => *sel = (*sel + n - 1) % n,
            KeyCode::Down => *sel = (*sel + 1) % n,
            KeyCode::Enter => {
                let raw = items.get(*sel).cloned();
                self.picker = None;
                if let Some(raw) = raw {
                    self.open_file(&raw);
                }
            }
            KeyCode::Esc => self.picker = None,
            _ => {}
        }
        true
    }

    /// Moves the keyboard selection through the feed's posts: up from the
    /// composer selects the newest; down past the newest returns to typing.
    fn move_cursor(&mut self, up: bool) {
        let n = self.feed_items.len();
        let at = self.cursor.as_ref().and_then(|k| self.feed_items.iter().position(|i| i == k));
        let next = match (at, up) {
            (None, true) => n.checked_sub(1),
            (None, false) => None,
            (Some(i), true) => Some(i.saturating_sub(1)),
            (Some(i), false) => (i + 1 < n).then_some(i + 1),
        };
        self.cursor = next.map(|i| self.feed_items[i].clone());
    }

    /// Keys while a post is selected. False if none is, or the key isn't one.
    fn on_cursor_key(&mut self, k: KeyEvent) -> bool {
        let Some(key) = self.cursor.clone() else { return false };
        let actions = self.hub.channel(&self.selected).and_then(|c| self.hub.post_actions(c, &key));
        let (files, text) = actions.unwrap_or_default();
        let tool = self.tool_info.get(&key).cloned();
        match k.code {
            KeyCode::Up | KeyCode::Down => self.move_cursor(k.code == KeyCode::Up),
            KeyCode::Esc => self.cursor = None,
            KeyCode::Enter => match tool {
                Some(t) if t.expandable => {
                    if !self.expanded.remove(&key) {
                        self.expanded.insert(key);
                    }
                }
                _ => self.open_files(files),
            },
            KeyCode::Char('o') => self.open_files(files),
            KeyCode::Char('y') => {
                copy_to_clipboard(&text);
                self.notify(format!("copied {} characters", text.chars().count()));
            }
            // Scrolling through a long selected post keeps it selected.
            KeyCode::PageUp | KeyCode::PageDown => return false,
            _ => {
                self.cursor = None; // anything else goes back to typing
                return false;
            }
        }
        true
    }

    /// Opens a post's one file, or lists them when there are several.
    fn open_files(&mut self, files: Vec<(String, crate::files::FileRef)>) {
        match files.len() {
            0 => self.notify("no files here"),
            1 => self.open_file(&files[0].0),
            _ => self.picker = Some((files.into_iter().map(|(raw, _)| raw).collect(), 0)),
        }
    }

    fn on_key(&mut self, k: KeyEvent) -> Result<()> {
        if self.on_picker_key(k) || self.on_suggestion_key(k)? || self.on_cursor_key(k) {
            return Ok(());
        }
        let before = self.editor.text().to_string();
        self.handle_key(k)?;
        if self.editor.text() != before {
            self.suggestion = 0;
            self.dismissed = None;
        }
        Ok(())
    }

    fn handle_key(&mut self, k: KeyEvent) -> Result<()> {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let page = self.feed_height.saturating_sub(2).max(1);
        match k.code {
            KeyCode::Char('c' | 'q') if ctrl => self.quit = true,
            KeyCode::Char('o') if ctrl => {
                let files = self.hub.channel(&self.selected).map(|c| self.hub.mentioned_files(c)).unwrap_or_default();
                if files.is_empty() {
                    self.notify("no files mentioned here");
                } else {
                    self.picker = Some((files.into_iter().map(|(raw, _)| raw).collect(), 0));
                }
            }
            KeyCode::Esc if self.viewer.is_some() => self.viewer = None,
            KeyCode::PageUp if self.viewer.is_some() => {
                if let Some(v) = self.viewer.as_mut() {
                    v.scroll = v.scroll.saturating_sub(page);
                }
            }
            KeyCode::PageDown if self.viewer.is_some() => {
                if let Some(v) = self.viewer.as_mut() {
                    v.scroll += page;
                }
            }
            KeyCode::Char('e') if ctrl => {
                // Expand every tool call in the open feed, or collapse them all.
                if self.feed_tools.iter().any(|k| self.expanded.contains(k)) {
                    self.expanded.clear();
                } else {
                    self.expanded.extend(self.feed_tools.iter().cloned());
                }
            }
            KeyCode::Char('t') if ctrl => {
                let name = self.hub.cycle_theme()?;
                self.notify(format!("theme: {name} · Ctrl+T for the next"));
            }
            KeyCode::Up if ctrl || alt => self.move_selection(-1),
            KeyCode::Down if ctrl || alt => self.move_selection(1),
            KeyCode::Char('p') if ctrl => self.move_selection(-1),
            KeyCode::Char('n') if ctrl => self.move_selection(1),
            KeyCode::BackTab => self.move_selection(-1),
            KeyCode::Tab => self.move_selection(1),
            KeyCode::PageUp => self.scroll += page,
            KeyCode::PageDown => self.scroll = self.scroll.saturating_sub(page),
            KeyCode::Up if self.editor.is_empty() => self.move_cursor(true),
            KeyCode::Esc => self.scroll = 0,
            KeyCode::Enter => self.submit()?,
            KeyCode::Backspace if self.editor.is_empty() && !self.pending.is_empty() => {
                self.pending.pop();
            }
            _ => {
                self.editor.handle(k);
            }
        }
        Ok(())
    }

    fn on_mouse(&mut self, kind: MouseEventKind, col: u16, row: u16) {
        let pos = Position::new(col, row);
        let in_feed = self.feed_area.contains(pos);
        let in_viewer = self.viewer.is_some() && self.viewer_area.contains(pos);
        if let Some(v) = self.viewer.as_mut().filter(|_| in_viewer) {
            match kind {
                MouseEventKind::ScrollUp => v.scroll = v.scroll.saturating_sub(3),
                MouseEventKind::ScrollDown => v.scroll += 3,
                _ => {}
            }
            return;
        }
        let in_sidebar = self.sidebar_area.contains(pos);
        match kind {
            MouseEventKind::ScrollUp if in_feed => self.scroll += 3,
            MouseEventKind::ScrollDown if in_feed => self.scroll = self.scroll.saturating_sub(3),
            MouseEventKind::ScrollUp if in_sidebar => self.move_selection(-1),
            MouseEventKind::ScrollDown if in_sidebar => self.move_selection(1),
            MouseEventKind::Down(MouseButton::Left) if in_feed => {
                let clicked = self.tool_rows.iter().find(|(y, _)| *y == row).map(|(_, t)| t.clone());
                match clicked {
                    // A call with nothing to expand but a file (Read): open the file.
                    Some(feed::ToolRow { file: Some(file), expandable: false, .. }) => self.open_file(&file),
                    Some(t) if t.expandable && !self.expanded.remove(&t.key) => {
                        self.expanded.insert(t.key);
                    }
                    _ => {}
                }
            }
            MouseEventKind::Down(MouseButton::Left) if in_sidebar => {
                if let Some((_, id)) = self.sidebar_rows.iter().find(|(y, _)| *y == row).cloned() {
                    self.select(&id);
                }
            }
            _ => {}
        }
    }
}

/// Reports terminal image support, run in the terminal in question.
pub fn doctor() {
    ratatui::crossterm::terminal::enable_raw_mode().ok();
    let images = images::Images::detect();
    ratatui::crossterm::terminal::disable_raw_mode().ok();
    match images {
        Some(im) => println!("{}", im.describe()),
        None => println!("images: off (AGENTCHAT_IMAGES)"),
    }
    for k in ["AGENTCHAT_IMAGES", "TERM", "TERM_PROGRAM", "TERM_PROGRAM_VERSION", "WEZTERM_PANE", "WEZTERM_EXECUTABLE", "TMUX", "WSL_DISTRO_NAME", "HERDR_ENV"] {
        if let Ok(v) = std::env::var(k) {
            println!("{k}={v}");
        }
    }
}

/// Copies text to the system clipboard through the terminal (OSC 52), which
/// works over SSH and in WezTerm, kitty, iTerm2, Windows Terminal and others.
fn copy_to_clipboard(text: &str) {
    use base64::Engine;
    use std::io::Write;
    let b64 = base64::engine::general_purpose::STANDARD.encode(text);
    let osc = format!("\x1b]52;c;{b64}\x07");
    // Inside tmux, the sequence has to be passed through to the outer terminal.
    let seq = if std::env::var_os("TMUX").is_some() { format!("\x1bPtmux;\x1b{osc}\x1b\\") } else { osc };
    let mut out = std::io::stdout();
    let _ = out.write_all(seq.as_bytes());
    let _ = out.flush();
}
