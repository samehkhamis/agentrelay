//! The terminal UI: UI-only state (selection, scroll, composer, notices) over
//! the shared [`Hub`]. Drawing lives in `view`.
//!
//! Dragging files onto the terminal pastes their paths; a paste made only of
//! existing file paths attaches those files instead of inserting text.

mod editor;
mod feed;
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

/// Delivery results from a background send: (delivered, failed) names.
type Sent = (Vec<String>, Vec<String>);

pub struct App {
    hub: Hub,
    /// Id of the open channel.
    selected: String,
    editor: Editor,
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
    /// The theme the cache was rendered in.
    cache_theme: &'static str,
    sent_tx: Sender<Sent>,
    sent_rx: Receiver<Sent>,
    quit: bool,
}

pub fn run() -> Result<()> {
    let mut app = App::new()?;
    let mut terminal = ratatui::init();
    execute!(std::io::stdout(), EnableMouseCapture, EnableBracketedPaste)?;
    let res = app.event_loop(&mut terminal);
    let _ = execute!(std::io::stdout(), DisableMouseCapture, DisableBracketedPaste);
    ratatui::restore();
    res
}

impl App {
    fn new() -> Result<App> {
        let (sent_tx, sent_rx) = mpsc::channel();
        Ok(App {
            hub: Hub::open()?,
            selected: "all".into(),
            editor: Editor::default(),
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
            cache_theme: theme::t().name,
            sent_tx,
            sent_rx,
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
            self.notify(format!("{} agents online", self.hub.agents.len()));
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
        while let Ok((ok, failed)) = self.sent_rx.try_recv() {
            let mut t = format!("delivered to {}", ok.len());
            if !failed.is_empty() {
                t.push_str(&format!(" · failed: {}", failed.join(", ")));
            }
            self.awaiting.extend(ok);
            self.notify(t);
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
                let tx = self.sent_tx.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(delivery.run());
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

    fn on_key(&mut self, k: KeyEvent) -> Result<()> {
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
