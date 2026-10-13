//! The terminal UI: UI-only state (selection, scroll, composer, notices) over
//! the shared [`Hub`]. Drawing lives in `view`.
//!
//! Dragging files onto the terminal pastes their paths; a paste made only of
//! existing file paths attaches those files instead of inserting text.

mod editor;
mod feed;
pub mod images;
mod markdown;
mod paste;
mod view;

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
    /// A send: (delivered, failed) agent names.
    Delivered(Vec<String>, Vec<String>),
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
                self.poll()?;
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
                Done::Delivered(ok, failed) => {
                    let mut t = format!("delivered to {}", ok.len());
                    if !failed.is_empty() {
                        t.push_str(&format!(" · failed: {}", failed.join(", ")));
                    }
                    self.awaiting.extend(ok);
                    self.notify(t);
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
            Some(Input::Rename(name)) => match self.hub.rename(&self.selected.clone(), &name) {
                Ok((note, follow)) => {
                    self.selected = follow; // a renamed Claude DM reappears under its new name
                    self.notify(note);
                }
                Err(e) => self.notify(e.to_string()),
            },
            Some(Input::Shell(cmd)) => {
                let task = self.hub.shell(&self.selected, &cmd);
                self.background(task, "running…".into());
            }
            Some(Input::Quit) => self.quit = true,
            Some(Input::Help) => self.notify(hub::HELP),
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
                self.notify(format!("delivering to {}…", delivery.targets.len()));
                let tx = self.done_tx.clone();
                std::thread::spawn(move || {
                    let (ok, failed) = delivery.run();
                    let _ = tx.send(Done::Delivered(ok, failed));
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

    fn on_key(&mut self, k: KeyEvent) -> Result<()> {
        if self.on_suggestion_key(k)? {
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
            KeyCode::Up if self.editor.is_empty() => self.scroll += 3,
            KeyCode::Down if self.editor.is_empty() => self.scroll = self.scroll.saturating_sub(3),
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
        let in_sidebar = self.sidebar_area.contains(pos);
        match kind {
            MouseEventKind::ScrollUp if in_feed => self.scroll += 3,
            MouseEventKind::ScrollDown if in_feed => self.scroll = self.scroll.saturating_sub(3),
            MouseEventKind::ScrollUp if in_sidebar => self.move_selection(-1),
            MouseEventKind::ScrollDown if in_sidebar => self.move_selection(1),
            MouseEventKind::Down(MouseButton::Left) if in_sidebar => {
                if let Some((_, id)) = self.sidebar_rows.iter().find(|(y, _)| *y == row).cloned() {
                    self.select(&id);
                }
            }
            _ => {}
        }
    }
}
