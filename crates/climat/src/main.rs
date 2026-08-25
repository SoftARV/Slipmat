// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! `climat` — Winamp 2.x for Apple Music, in a terminal.
//!
//! A client of `slipmatd` and nothing more: it draws what the daemon says and
//! sends what the keyboard asks for. **No key changes what is on screen** — a
//! press becomes a request, and the screen moves when the daemon answers. That
//! is rule 3 reaching the third layer, and it is what keeps a terminal and a
//! GTK window from disagreeing about what is playing.
//!
//! **It needs a graphical session.** Not for itself — for the Chromium the
//! daemon runs, which wants a display server even with its window hidden. That
//! is Widevine, not a shortcut, and it is why this cannot run over plain SSH.

mod browser;
mod link;
mod queue;
mod ui;

use anyhow::Result;
use crossterm::event::{Event as TermEvent, EventStream, KeyCode, KeyEventKind};
use crossterm::terminal::{LeaveAlternateScreen, disable_raw_mode};
use futures::StreamExt;
use slipmat_core::ipc::{
    CatalogFilter, Event, PlayMode, Request, Snapshot, Stage, Transport, View as LibraryView,
};
use slipmat_core::player::protocol::RepeatMode;

use crate::browser::{SECTIONS, Showing};
use crate::ui::Focus;

/// How often to redraw while playing.
///
/// The daemon sends a snapshot twice a second; this is what makes the clock
/// tick between them rather than stepping half a second at a time.
const FRAME_MS: u64 = 100;

/// How long a refusal from the daemon stays on screen.
const MESSAGE_FOR: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Default)]
struct App {
    snap: Snapshot,
    stage: Stage,
    browser: browser::Browser,
    queue: queue::Queue,
    focus: Focus,
    /// The last thing the daemon refused, and when — so it can fade rather than
    /// sit there accusing a key that was pressed a minute ago.
    message: Option<(String, std::time::Instant)>,
    /// Set by `q`. `_` leaves without it, and the daemon keeps playing.
    quit_daemon: bool,
}

fn main() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(run());

    // Restored whatever happened: a panic that leaves the terminal in raw mode
    // and on the alternate screen leaves the shell unusable.
    let _ = disable_raw_mode();
    let _ = crossterm::execute!(std::io::stdout(), LeaveAlternateScreen);
    result
}

async fn run() -> Result<()> {
    let (link, mut events) = link::connect().await?;

    // `init` takes the terminal (raw mode, alternate screen) and installs a
    // panic hook that gives it back. The teardown in `main` covers the one case
    // it does not: an error returned from here after this line.
    let mut term = ratatui::init();

    let mut app = App::default();
    // The library is what the pane opens on, so ask for it with everything else
    // rather than waiting for a key. It comes from the daemon's cache, so this
    // is a local socket round trip and not a request to Apple.
    app.browse(&link);
    let mut keys = EventStream::new();
    let mut frame = tokio::time::interval(std::time::Duration::from_millis(FRAME_MS));
    // Drawn on the first pass, then only when something moved. The tick runs
    // whether or not anything is playing, and repainting a still screen ten
    // times a second is exactly the idle cost a background player should not
    // have.
    let mut dirty = true;

    loop {
        if dirty {
            term.draw(|f| {
                ui::draw(
                    f,
                    ui::View {
                        snap: &app.snap,
                        stage: &app.stage,
                        focus: app.focus,
                        typing: app.browser.typing,
                        catalog: app.browser.showing.is_catalog() || app.browser.from_catalog,
                        browser: &mut app.browser,
                        queue: &mut app.queue,
                        message: app.message.as_ref().map(|(text, _)| text.as_str()),
                    },
                )
            })?;
            dirty = false;
        }

        tokio::select! {
            // Keys first: a busy event stream must never starve the one thing
            // a person is waiting on.
            biased;
            Some(Ok(term_event)) = keys.next() => match term_event {
                TermEvent::Key(key) if key.kind == KeyEventKind::Press => {
                    if !on_key(key.code, &link, &mut app) {
                        break;
                    }
                    // **Always.** A key is the one event that is definitely
                    // worth a frame: half of them move only the cursor or the
                    // focus, which no daemon event will ever report back. Left
                    // out, those keys drew nothing at all — and the 100ms tick
                    // hid it completely while a track was playing, so the app
                    // looked frozen exactly when it was idle.
                    dirty = true;
                }
                // A resize invalidates the whole buffer, so it has to redraw
                // even though nothing about the music changed.
                TermEvent::Resize(..) => dirty = true,
                _ => {}
            },
            Some(message) = events.recv() => match message {
                link::Incoming::Event(event) => {
                    app.on_event(*event);
                    dirty = true;
                }
                link::Incoming::Lost(why) => {
                    ratatui::restore();
                    eprintln!("climat: lost the daemon — {why}");
                    return Ok(());
                }
            },
            _ = frame.tick() => {
                // Carry the clock forward between snapshots, so the time reads
                // like a clock rather than stepping twice a second.
                if app.message.as_ref().is_some_and(|(_, at)| at.elapsed() > MESSAGE_FOR) {
                    app.message = None;
                    dirty = true;
                }
                if app.snap.playing {
                    app.snap.position_ms = app
                        .snap
                        .position_ms
                        .saturating_add(FRAME_MS)
                        .min(app.snap.duration_ms.max(app.snap.position_ms));
                    dirty = true;
                }
            }
        }
    }

    ratatui::restore();
    if app.quit_daemon {
        // **Asked for, then waited on.** The daemon saves the session before it
        // goes, and leaving the moment the request is written would close the
        // socket underneath it. It answers by dying, so the wait ends when the
        // link is lost — or by refusing, when another window is still open.
        link.send(Request::Quit);
        if let Some(refusal) = wait_for_quit(&mut events).await {
            eprintln!("climat: {refusal}");
        }
    }
    Ok(())
}

/// Returns whether to keep running.
///
/// **No arm here changes the queue or the library.** Each sends a request and
/// the rows move when the daemon echoes — rule 3 at the third layer. The cursor
/// and which pane has focus are the exceptions, because they are where this
/// terminal is looking rather than anything about the player.
fn on_key(code: KeyCode, link: &link::Link, app: &mut App) -> bool {
    // **Typing comes first, and takes everything.** While the filter is open a
    // letter is a letter: `q` must not quit and `d` must not remove a track.
    if app.browser.typing {
        typing(code, link, app);
        return true;
    }

    match code {
        // Winamp put play and pause on separate keys because its buttons were
        // separate. Nothing else has since: one key that toggles is what a
        // person reaches for, and space is where every player has put it.
        KeyCode::Char(' ') => link.send(Request::Transport(Transport::PlayPause)),
        KeyCode::Char('z') => link.send(Request::Transport(Transport::Previous)),
        KeyCode::Char('b') => link.send(Request::Transport(Transport::Next)),
        KeyCode::Left => seek(link, app, -5_000),
        KeyCode::Right => seek(link, app, 5_000),
        KeyCode::Char('s') => link.send(Request::Transport(Transport::SetShuffle {
            shuffle: !app.snap.shuffle,
        })),
        KeyCode::Char('r') => link.send(Request::Transport(Transport::SetRepeat {
            mode: next_repeat(app.snap.repeat),
        })),

        KeyCode::Tab | KeyCode::BackTab => {
            app.focus = match app.focus {
                Focus::Browser => Focus::Queue,
                Focus::Queue => Focus::Browser,
            }
        }
        // Not a section of the library — the rest of Apple Music, which is
        // empty until it is asked for.
        KeyCode::Char('5') => {
            app.focus = Focus::Browser;
            app.show_catalog();
        }
        KeyCode::Char(d @ '1'..='4') => {
            // Selecting a section is also a request to look at it, so it takes
            // focus — otherwise the arrows would still be driving the queue.
            let (view, _) = SECTIONS[d as usize - '1' as usize];
            app.focus = Focus::Browser;
            app.show_library(view, link);
        }
        KeyCode::Char('/') if app.focus == Focus::Browser => app.browser.typing = true,
        // Out of a page, then out of a filter — the order they were entered.
        KeyCode::Esc => app.back(link),

        KeyCode::Up | KeyCode::Char('k') => app.pane_cursor(-1),
        KeyCode::Down | KeyCode::Char('j') => app.pane_cursor(1),
        KeyCode::PageUp => app.pane_cursor(-10),
        KeyCode::PageDown => app.pane_cursor(10),
        KeyCode::Home if app.focus == Focus::Queue => app.queue.follow(),
        KeyCode::Enter => app.activate(link),

        KeyCode::Char('d') if app.focus == Focus::Queue => link.send(Request::RemoveFromQueue {
            index: app.queue.cursor,
        }),
        // Shift moves the row rather than the cursor. The cursor goes with it,
        // so the selection stays on the track being moved and a second press
        // moves the same one again.
        KeyCode::Char('K') if app.focus == Focus::Queue => reorder(link, app, -1),
        KeyCode::Char('J') if app.focus == Focus::Queue => reorder(link, app, 1),

        // Leaves the daemon alone: the music keeps playing, and a GTK
        // window or another terminal still has a player to attach to.
        KeyCode::Char('_') | KeyCode::Char('-') => return false,
        KeyCode::Char('q') => {
            app.quit_daemon = true;
            return false;
        }
        _ => {}
    }
    true
}

/// The filter, which owns the keyboard while it is open.
///
/// **Who answers decides what a keystroke costs.** Over the library every edit
/// re-browses, which is affordable because the daemon replies from the cache it
/// already holds — a local socket, not Apple. Over the catalog every query is a
/// real request to somebody else's API, so keystrokes only edit the text and
/// `↵` is what sends it. Same box, same key, two rules, and the hint row says
/// which one is in force.
fn typing(code: KeyCode, link: &link::Link, app: &mut App) {
    match code {
        KeyCode::Char(c) => app.browser.filter.push(c),
        KeyCode::Backspace => {
            app.browser.filter.pop();
        }
        KeyCode::Esc => {
            app.browser.filter.clear();
            app.browser.typing = false;
            if app.browser.showing.is_catalog() {
                app.browser.rows.clear();
            } else {
                app.browser.reset();
                app.browse(link);
            }
            return;
        }
        KeyCode::Enter => {
            // Keeps the filter, closes the box: the arrows go back to moving
            // through the list. Over the catalog this is also the send.
            app.browser.typing = false;
            if app.browser.showing.is_catalog() {
                app.search(link);
            }
            return;
        }
        _ => return,
    }
    if !app.browser.showing.is_catalog() {
        app.browser.reset();
        app.browse(link);
    }
}

impl App {
    /// Ask for whatever the browser is currently meant to be showing.
    fn browse(&self, link: &link::Link) {
        link.send(Request::Browse {
            view: self.browser.view,
            query: self.browser.filter.clone(),
            offset: 0,
            // The daemon answers from its own cache, so the whole section costs
            // no more than a page of it and saves paging entirely.
            limit: 0,
        });
    }

    fn show_library(&mut self, view: LibraryView, link: &link::Link) {
        self.browser.view = view;
        self.browser.showing = Showing::Library;
        self.browser.reset();
        self.browse(link);
    }

    /// Open the catalog pane. Nothing is fetched — it waits to be asked.
    fn show_catalog(&mut self) {
        self.browser.showing = browser::Showing::Catalog { searching: false };
        self.browser.rows.clear();
        self.browser.filter.clear();
        self.browser.reset();
        // Straight into the box: `5` is only ever pressed in order to type.
        self.browser.typing = true;
    }

    fn search(&mut self, link: &link::Link) {
        let query = self.browser.filter.trim().to_owned();
        if query.is_empty() {
            self.browser.rows.clear();
            return;
        }
        self.browser.showing = browser::Showing::Catalog { searching: true };
        self.browser.rows.clear();
        self.browser.reset();
        link.send(Request::Search {
            query,
            filter: CatalogFilter::All,
            offset: 0,
        });
    }

    fn pane_cursor(&mut self, delta: isize) {
        match self.focus {
            Focus::Browser => self.browser.move_cursor(delta),
            Focus::Queue => self.queue.move_cursor(delta),
        }
    }

    /// Enter: play the selected row, or open the page it leads to.
    fn activate(&mut self, link: &link::Link) {
        if self.focus == Focus::Queue {
            return link.send(Request::JumpTo {
                index: self.queue.cursor,
            });
        }
        let came_from_catalog = self.browser.showing.is_catalog();
        let Some(entry) = self.browser.selected() else {
            return;
        };
        if let Some((kind, id)) = entry.page_target() {
            // Named before the daemon answers, so an album that takes a moment
            // to arrive shows whose it is rather than going blank.
            self.browser.showing = Showing::Page {
                title: entry.title().to_owned(),
                subtitle: entry.subtitle(),
                loading: true,
            };
            self.browser.from_catalog = came_from_catalog;
            self.browser.rows.clear();
            self.browser.reset();
            return link.send(Request::Open { kind, id });
        }
        // A song: the queue is the whole list it sits in, opened at this row.
        let (ids, index) = self.browser.queue_from_here();
        if ids.is_empty() {
            self.message = Some((
                "Nothing here can be streamed".into(),
                std::time::Instant::now(),
            ));
            return;
        }
        link.send(Request::Play {
            ids,
            index,
            start: PlayMode::InOrder,
        });
    }

    /// Esc: out of a page first, then out of a filter.
    fn back(&mut self, link: &link::Link) {
        if matches!(self.browser.showing, Showing::Page { .. }) {
            // **Back to where the page was opened from.** An album reached from
            // a catalog search belongs to the search, and returning to the
            // library instead loses the results and the question that found it.
            // The rows were cleared to open the page, so this costs the one
            // request again.
            if self.browser.from_catalog {
                self.browser.from_catalog = false;
                return self.search(link);
            }
            let view = self.browser.view;
            return self.show_library(view, link);
        }
        if !self.browser.filter.is_empty() {
            self.browser.filter.clear();
            self.browser.reset();
            self.browse(link);
        }
    }
}

/// Off, then all, then one — the order every player cycles them in.
fn next_repeat(mode: RepeatMode) -> RepeatMode {
    match mode {
        RepeatMode::None => RepeatMode::All,
        RepeatMode::All => RepeatMode::One,
        RepeatMode::One => RepeatMode::None,
    }
}

fn reorder(link: &link::Link, app: &mut App, delta: isize) {
    let Some(to) = app.queue.swap_target(delta) else {
        return;
    };
    link.send(Request::MoveInQueue {
        from: app.queue.cursor,
        to,
    });
    app.queue.cursor_to(to);
}

/// Seek relative to where the clock has got to, not to the last snapshot — the
/// two differ by up to half a second, which is enough to feel wrong.
fn seek(link: &link::Link, app: &App, delta: i64) {
    let target = (app.snap.position_ms as i64 + delta).max(0) as u64;
    link.send(Request::Transport(Transport::Seek {
        position_ms: target,
    }));
}

impl App {
    fn on_event(&mut self, event: Event) {
        match event {
            Event::Snapshot(snap) => self.snap = snap,
            Event::Queue { items, position } => self.queue.replace(items, position),
            Event::Stage(stage) => self.stage = stage,
            Event::Rows { entries, total, .. } => {
                // Ignored while the catalog is showing: a stale library answer
                // must not overwrite the results somebody asked Apple for.
                if !self.browser.showing.is_catalog() {
                    self.browser.replace(entries, total);
                }
            }
            Event::Results { query, entries, .. } => {
                // Only if it is still the question being asked — a slow answer
                // to an abandoned query would otherwise land on screen.
                if self.browser.showing.is_catalog() && query == self.browser.filter.trim() {
                    self.browser.showing = browser::Showing::Catalog { searching: false };
                    let total = entries.len();
                    self.browser.replace(entries, total);
                    self.browser.reset();
                }
            }
            Event::Page {
                header, entries, ..
            } => {
                self.browser.showing = Showing::Page {
                    title: header.title().to_owned(),
                    subtitle: header.subtitle(),
                    loading: false,
                };
                self.browser.replace(entries, 0);
                self.browser.reset();
            }
            // The daemon refuses things — removing the track it is playing is
            // the one that will be hit most. Saying so is rule 4's job.
            Event::Error { detail } => self.message = Some((detail, std::time::Instant::now())),
            // Slice 01 draws the player and nothing else. The rest of the
            // contract is answered in the slices that draw it.
            _ => {}
        }
    }
}

/// Wait for the daemon to act on [`Request::Quit`], and report a refusal.
///
/// Returns `None` when it went away, which is the request being honoured.
async fn wait_for_quit(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<link::Incoming>,
) -> Option<String> {
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(5));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            message = events.recv() => match message {
                Some(link::Incoming::Event(event)) => {
                    if let Event::Error { detail } = *event {
                        return Some(detail);
                    }
                }
                // The socket closed: the daemon is gone, which is what was asked.
                Some(link::Incoming::Lost(_)) | None => return None,
            },
            _ = &mut deadline => return Some("the daemon did not answer".into()),
        }
    }
}
