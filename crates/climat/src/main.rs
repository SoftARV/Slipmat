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
mod spectrum;
mod theme;
mod ui;

use anyhow::Result;
use crossterm::event::{
    Event as TermEvent, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use crossterm::terminal::{LeaveAlternateScreen, disable_raw_mode};
use futures::StreamExt;
use slipmat_core::ipc::{
    Event, PlayMode, Request, Snapshot, Stage, Transport, View as LibraryView,
};
use slipmat_core::player::protocol::RepeatMode;

use crate::browser::{SECTIONS, Showing};
use crate::ui::Pane;

/// How often to redraw while playing.
///
/// The daemon sends a snapshot twice a second; this is what makes the clock
/// tick between them rather than stepping half a second at a time.
const FRAME_MS: u64 = 100;

/// How long a notice stays on screen.
const MESSAGE_FOR: std::time::Duration = std::time::Duration::from_secs(4);

/// Something to tell the person at the keyboard.
///
/// **`bad` is not decoration.** Queueing a track and being refused one look
/// identical as a line of text, and drawing both in the accent taught people to
/// read the accent as "something happened" rather than "something is wrong".
struct Notice {
    text: String,
    at: std::time::Instant,
    bad: bool,
}

impl Notice {
    fn good(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            at: std::time::Instant::now(),
            bad: false,
        }
    }

    fn bad(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            at: std::time::Instant::now(),
            bad: true,
        }
    }
}

struct App {
    snap: Snapshot,
    stage: Stage,
    browser: browser::Browser,
    queue: queue::Queue,
    pane: Pane,
    /// The last thing worth saying, and when — so it fades rather than sitting
    /// there accusing a key that was pressed a minute ago.
    message: Option<Notice>,
    refreshing_library: bool,
    /// Set by `q`. `_` leaves without it, and the daemon keeps playing.
    quit_daemon: bool,
    /// The visualiser's last frame. All zeroes when there is nothing to hear,
    /// or when there was no audio server to listen to in the first place.
    bars: [f32; spectrum::BANDS],
}

impl Default for App {
    fn default() -> Self {
        Self {
            snap: Snapshot::default(),
            stage: Stage::default(),
            browser: browser::Browser::default(),
            queue: queue::Queue::default(),
            pane: Pane::default(),
            message: None,
            refreshing_library: false,
            quit_daemon: false,
            // `#[derive(Default)]` stops at arrays of 32.
            bars: [0.0; spectrum::BANDS],
        }
    }
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
    // **After `init`, before the key stream.** The reply comes back on stdin as
    // an escape sequence, so raw mode has to be on or it is echoed and
    // line-buffered — and it has to happen before crossterm starts consuming
    // stdin itself, or the answer is delivered as a keystroke.
    theme::detect();

    let mut app = App::default();
    // The library is what the pane opens on, so ask for it with everything else
    // rather than waiting for a key. It comes from the daemon's cache, so this
    // is a local socket round trip and not a request to Apple.
    app.browse(&link);
    // Decoration: if there is no audio server to listen to, the bars simply
    // never appear and everything else works exactly as before.
    let mut spectrum = spectrum::start();
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
                        pane: app.pane,
                        typing: app.browser.typing,
                        catalog: app.browser.showing.is_catalog() || app.browser.from_catalog,
                        bars: &app.bars,
                        browser: &mut app.browser,
                        queue: &mut app.queue,
                        message: app
                            .message
                            .as_ref()
                            .map(|n| (n.text.as_str(), n.bad))
                            .or_else(|| {
                                app.refreshing_library
                                    .then_some(("Refreshing library…", false))
                            }),
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
                    if !on_key(key, &link, &mut app) {
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
                    app.on_event(*event, &link);
                    dirty = true;
                }
                link::Incoming::Lost(why) => {
                    ratatui::restore();
                    eprintln!("climat: lost the daemon — {why}");
                    return Ok(());
                }
            },
            // `spectrum.as_mut()` so a missing visualiser is a branch that
            // never completes rather than a second code path through the loop.
            Some(bars) = async {
                match spectrum.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                // **Only while Slipmat is playing.** The monitor carries
                // whatever the machine is making, and asking for one stream is
                // not enough — see `spectrum`. So the player's own state is
                // what decides whether there are bars at all: a notification
                // chime with nothing playing must not move them.
                if app.snap.playing {
                    app.bars = bars;
                    dirty = true;
                }
            }
            _ = frame.tick() => {
                // Carry the clock forward between snapshots, so the time reads
                // like a clock rather than stepping twice a second.
                if app.message.as_ref().is_some_and(|n| n.at.elapsed() > MESSAGE_FOR) {
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
fn on_key(key: KeyEvent, link: &link::Link, app: &mut App) -> bool {
    // **Before everything, including the filter.** In raw mode Ctrl+C is a
    // keystroke rather than a signal, so it has to be answered here — and it
    // is the one key that should work whatever the app is in the middle of.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return false;
    }
    let code = key.code;

    if matches!(app.stage, Stage::SignedOut) && code == KeyCode::Enter {
        link.send(Request::SignIn);
        return true;
    }

    // **Typing comes next, and takes everything else.** While the filter is
    // open a letter is a letter: `q` must not quit and `d` must not remove a
    // track.
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
        KeyCode::Char('R') if matches!(app.stage, Stage::Ready) => {
            link.send(Request::Refresh);
            app.refreshing_library = true;
            app.message = Some(Notice::good("Refreshing library…"));
        }
        // Both faces of each key, now that neither means anything else: `-`
        // and `_` are one key, and so are `=` and `+`.
        KeyCode::Char('-') | KeyCode::Char('_') => volume(link, app, -0.05),
        KeyCode::Char('=') | KeyCode::Char('+') => volume(link, app, 0.05),

        // **The tab strip in one key.** With a single pane there is nothing to
        // switch focus between, so `⇥` walks the tabs instead — the library
        // sections, then Apple Music, then the queue, then round.
        KeyCode::Tab => app.next_tab(link),
        KeyCode::BackTab => app.prev_tab(link),
        // Not a section of the library — the rest of Apple Music, which is
        // empty until it is asked for.
        KeyCode::Char('5') => {
            app.pane = Pane::Browser;
            app.show_catalog();
            // Straight into the box: `5` is only ever pressed in order to
            // search. Arriving by `⇥` is not — and opening the filter there
            // would swallow the very key that got you here.
            app.browser.typing = true;
        }
        // The queue's own tab, numbered like the rest. It does not get a hint
        // of its own — `1-6` already says it, and saying it twice is noise.
        KeyCode::Char('6') => app.pane = Pane::Queue,
        KeyCode::Char(d @ '1'..='4') => {
            // Selecting a section is also a request to look at it, so it takes
            // focus — otherwise the arrows would still be driving the queue.
            let (view, _) = SECTIONS[d as usize - '1' as usize];
            app.pane = Pane::Browser;
            app.show_library(view, link);
        }
        KeyCode::Char('/') if app.pane == Pane::Browser => app.browser.typing = true,
        // Out of a page, then out of a filter — the order they were entered.
        KeyCode::Esc => app.back(link),

        KeyCode::Up => {
            app.pane_cursor(-1);
        }
        KeyCode::Down => {
            app.pane_cursor(1);
            app.maybe_page(link);
        }
        KeyCode::PageUp => {
            app.pane_cursor(-10);
        }
        KeyCode::PageDown => {
            app.pane_cursor(10);
            app.maybe_page(link);
        }
        KeyCode::Home if app.pane == Pane::Queue => app.queue.follow(),
        KeyCode::Enter => app.activate(link),

        // **The `ijkl` cluster, laid out the way it sits under the hand.**
        //
        //         i          move the row up
        //     j   k   l      queue next · move it down · queue last
        //
        // `i` above `k` is up above down, and `j` before `l` is sooner before
        // later. It costs vim's `j`/`k` for the cursor, which the arrows still
        // do.
        // `o` for order: the key steps through what a section can honestly be
        // sorted by, `O` turns it round.
        // What Apple is asked for, on the catalog tab. `o` is the order on a
        // library section; a catalog list has no order of ours, so the same
        // corner of the screen carries this instead.
        KeyCode::Char('t') if app.browser.showing.is_catalog() => {
            app.browser.cycle_kinds();
            if !app.browser.filter().trim().is_empty() {
                app.search(link);
            }
        }
        KeyCode::Char('o') if app.pane == Pane::Browser => {
            if app.browser.cycle_sort() {
                app.browser.reset();
                app.browse(link);
            } else {
                app.message = Some(Notice::bad("Nothing else to sort these by"));
            }
        }
        KeyCode::Char('O') if app.pane == Pane::Browser => {
            app.browser.flip_sort();
            app.browser.reset();
            app.browse(link);
        }
        KeyCode::Char('j') if app.pane == Pane::Browser => app.enqueue(link, true),
        KeyCode::Char('l') if app.pane == Pane::Browser => app.enqueue(link, false),
        KeyCode::Char('i') if app.pane == Pane::Queue => reorder(link, app, -1),
        KeyCode::Char('k') if app.pane == Pane::Queue => reorder(link, app, 1),
        // **Said, not swallowed.** Reordering only means something in the
        // queue, and pressing it elsewhere used to do nothing at all — which
        // reads as a broken key rather than as the wrong place for it.
        KeyCode::Char('i') | KeyCode::Char('k') => {
            app.message = Some(Notice::bad("Reordering lives in the queue — press 6"))
        }

        KeyCode::Char('d') if app.pane == Pane::Queue => link.send(Request::RemoveFromQueue {
            index: app.queue.cursor,
        }),

        // Leaves the daemon alone, the way Ctrl+C above does: the music keeps
        // playing and a GTK window or another terminal still has a player to
        // attach to. `q` is the one that takes the player with it.
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
        KeyCode::Char(c) => app.browser.filter_mut().push(c),
        KeyCode::Backspace => {
            app.browser.filter_mut().pop();
        }
        KeyCode::Esc => {
            app.browser.filter_mut().clear();
            app.browser.typing = false;
            if app.browser.showing.is_catalog() {
                // The question is withdrawn, so its answer goes too.
                app.browser.forget_found();
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
        let (sort, reverse) = self.browser.sort();
        link.send(Request::Browse {
            view: self.browser.view,
            query: self.browser.filter().to_owned(),
            sort,
            reverse,
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
    /// Open the catalog pane. Nothing is fetched — it waits to be asked.
    ///
    /// **What was already asked stays.** The catalog's text is not a filter but
    /// a question Apple has already answered; clearing it on the way past would
    /// mean paying for the same search again on the way back.
    fn show_catalog(&mut self) {
        self.browser.showing = browser::Showing::Catalog { searching: false };
        // **Its own results, not whatever was showing.** Every other tab leaves
        // `rows` holding a library section or an album's tracks, and without
        // this those stayed on screen under Apple Music's name.
        self.browser.restore_found();
        self.browser.reset();
    }

    fn search(&mut self, link: &link::Link) {
        let query = self.browser.filter().trim().to_owned();
        if query.is_empty() {
            self.browser.forget_found();
            return;
        }
        self.browser.showing = browser::Showing::Catalog { searching: true };
        self.browser.forget_found();
        self.browser.reset();
        self.browser.more = false;
        self.browser.paging = false;
        link.send(Request::Search {
            query,
            filter: self.browser.kinds,
            offset: 0,
        });
    }

    /// Ask Apple for the next page of the search already on screen.
    fn page(&mut self, link: &link::Link) {
        let query = self.browser.filter().trim().to_owned();
        if query.is_empty() {
            return;
        }
        // Marked before the request goes, not when it comes back: every
        // keypress near the end would otherwise ask for the same page again.
        self.browser.paging = true;
        link.send(Request::Search {
            query,
            filter: self.browser.kinds,
            offset: self.browser.rows.len(),
        });
    }

    fn pane_cursor(&mut self, delta: isize) {
        match self.pane {
            Pane::Browser => self.browser.move_cursor(delta),
            Pane::Queue => self.queue.move_cursor(delta),
        }
    }

    /// Fetch the next page if the cursor has come close enough to the end.
    ///
    /// Called after every cursor move rather than on a key of its own: paging
    /// is not something to ask for, it is what a list does when you reach the
    /// bottom of it.
    fn maybe_page(&mut self, link: &link::Link) {
        if self.pane == Pane::Browser && self.browser.wants_more() {
            self.page(link);
        }
    }

    /// Walk the tab strip. Six places: four library sections, Apple Music, the
    /// queue.
    fn tab(&mut self, step: isize, link: &link::Link) {
        let here = if self.pane == Pane::Queue {
            5
        } else if self.browser.showing.is_catalog() {
            4
        } else {
            SECTIONS
                .iter()
                .position(|(v, _)| *v == self.browser.view)
                .unwrap_or(0) as isize as usize
        };
        let next = (here as isize + step).rem_euclid(6) as usize;
        self.pane = Pane::Browser;
        match next {
            5 => self.pane = Pane::Queue,
            4 => self.show_catalog(),
            n => {
                let (view, _) = SECTIONS[n];
                self.show_library(view, link);
            }
        }
    }

    fn next_tab(&mut self, link: &link::Link) {
        self.tab(1, link);
    }

    fn prev_tab(&mut self, link: &link::Link) {
        self.tab(-1, link);
    }

    /// Enter: play the selected row, or open the page it leads to.
    fn activate(&mut self, link: &link::Link) {
        if self.pane == Pane::Queue {
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
            self.message = Some(Notice::bad("Nothing here can be streamed"));
            return;
        }
        link.send(Request::Play {
            ids,
            index,
            start: PlayMode::InOrder,
        });
    }

    /// Put the selected row in the queue rather than playing it.
    ///
    /// **Songs only.** An album row has no playable id of its own — its tracks
    /// are a page that has not been fetched — so queueing one would mean an
    /// open, a wait, and a queue edit somebody did not watch happen. Saying so
    /// is better than half-doing it.
    fn enqueue(&mut self, link: &link::Link, next: bool) {
        let Some(entry) = self.browser.selected() else {
            return;
        };
        let Some(id) = entry.catalog_id() else {
            self.message = Some(Notice::bad(if entry.opens_a_page() {
                "Open it first — only songs can be queued"
            } else {
                "Apple cannot stream this one"
            }));
            return;
        };
        let title = entry.title().to_owned();
        link.send(Request::Enqueue {
            ids: vec![id.to_owned()],
            next,
        });
        self.message = Some(Notice::good(if next {
            format!("Playing next: {title}")
        } else {
            format!("Added to the queue: {title}")
        }));
    }

    /// Esc: out of a page first, then out of a filter.
    fn back(&mut self, link: &link::Link) {
        if matches!(self.browser.showing, Showing::Page { .. }) {
            // **Back to where the page was opened from.** An album reached from
            // a catalog search belongs to the search, and returning to the
            // library instead loses the results and the question that found it.
            // The catalog keeps its own answers, so this costs nothing — it
            // used to ask Apple the same question a second time.
            if self.browser.from_catalog {
                self.browser.from_catalog = false;
                self.browser.showing = browser::Showing::Catalog { searching: false };
                self.browser.restore_found();
                self.browser.reset();
                return;
            }
            let view = self.browser.view;
            return self.show_library(view, link);
        }
        if !self.browser.filter().is_empty() {
            self.browser.filter_mut().clear();
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

/// Volume, which only the daemon knows: MusicKit never reports it back, so the
/// snapshot's value is the last one somebody set and the right thing to step
/// from.
fn volume(link: &link::Link, app: &mut App, delta: f64) {
    let volume = (app.snap.volume + delta).clamp(0.0, 1.0);
    link.send(Request::Transport(Transport::SetVolume { volume }));
    // **Adopted, not awaited.** Every other control here waits for the daemon
    // to say what happened, because the daemon is the one that knows. Volume is
    // the exception: MusicKit never reports it, so the daemon's record is
    // exactly the number just sent and there is nothing to disagree with.
    // Waiting would mean each press stepping from the value the *last* one
    // started at — hold the key and it sends the same volume over and over.
    app.snap.volume = volume;
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
    fn clear_account_state(&mut self) {
        self.snap = Snapshot {
            volume: self.snap.volume,
            ..Default::default()
        };
        self.browser.clear_account_state();
        self.queue = queue::Queue::default();
        self.pane = Pane::Browser;
        self.message = None;
        self.refreshing_library = false;
        self.bars = [0.0; spectrum::BANDS];
    }

    fn on_event(&mut self, event: Event, link: &link::Link) {
        match event {
            Event::Snapshot(snap) => {
                // Stopping clears the bars at once rather than leaving the
                // last frame to decay over whatever the machine plays next.
                if !snap.playing {
                    self.bars = [0.0; spectrum::BANDS];
                }
                self.snap = snap;
            }
            Event::Queue { items, position } => self.queue.replace(items, position),
            Event::Stage(stage) => {
                if stage == Stage::SignedOut {
                    self.clear_account_state();
                }
                self.stage = stage;
            }
            Event::Rows { entries, total, .. } => {
                // Ignored while the catalog is showing: a stale library answer
                // must not overwrite the results somebody asked Apple for.
                if !self.browser.showing.is_catalog() {
                    self.browser.replace(entries, total);
                }
            }
            Event::Results {
                query,
                entries,
                offset,
                more,
            } => {
                // **Kept whichever tab is showing.** A search is a question
                // already paid for, and tabbing away while Apple thinks about
                // it used to throw the answer out — so the results are stored
                // either way, and only *shown* if the catalog is what is up.
                if query != self.browser.catalog_query() {
                    return; // a slow answer to an abandoned query
                }
                self.browser.more = more;
                self.browser.paging = false;
                let showing = self.browser.showing.is_catalog();
                if offset == 0 {
                    self.browser.keep_found(entries, showing);
                    if showing {
                        self.browser.showing = browser::Showing::Catalog { searching: false };
                        self.browser.reset();
                    }
                } else {
                    // A page, not an answer: it goes under what is already
                    // there and the cursor stays where somebody left it.
                    self.browser.extend(entries, showing);
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
            Event::LibraryChanged => {
                self.refreshing_library = false;
                self.message = None;
                if matches!(self.stage, Stage::Ready)
                    && matches!(self.browser.showing, Showing::Library)
                {
                    self.browse(link);
                }
            }
            Event::LibraryRefreshing { refreshing } => {
                self.refreshing_library = refreshing;
            }
            // The daemon refuses things — removing the track it is playing is
            // the one that will be hit most. Saying so is rule 4's job.
            Event::Error { detail } => self.message = Some(Notice::bad(detail)),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_out_enter_requests_sign_in_before_typing_consumes_it() {
        let (link, mut requests) = link::Link::channel();
        let mut app = App {
            stage: Stage::SignedOut,
            ..Default::default()
        };
        app.browser.typing = true;

        assert!(on_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &link,
            &mut app,
        ));
        assert!(matches!(
            requests.try_recv().expect("sign-in request"),
            Request::SignIn
        ));
    }

    #[test]
    fn other_stages_do_not_request_sign_in() {
        for stage in [
            Stage::Connecting,
            Stage::Ready,
            Stage::Broken {
                detail: "failed".into(),
            },
        ] {
            let (link, mut requests) = link::Link::channel();
            let mut app = App {
                stage,
                ..Default::default()
            };

            assert!(on_key(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                &link,
                &mut app,
            ));
            assert!(requests.try_recv().is_err());
        }
    }

    #[test]
    fn signed_out_clears_transient_player_state_and_keeps_choices() {
        let (link, _) = link::Link::channel();
        let mut app = App {
            snap: Snapshot {
                track_id: Some("old".into()),
                title: "Old song".into(),
                volume: 0.75,
                playing: true,
                ..Default::default()
            },
            ..Default::default()
        };
        app.browser.view = LibraryView::Albums;
        app.browser.kinds = slipmat_core::ipc::CatalogFilter::Artists;
        app.browser.sorts[1] = (slipmat_core::sort::SortBy::Year, true);
        app.browser.showing = Showing::Catalog { searching: true };
        *app.browser.filter_mut() = "old search".into();
        app.queue.replace(
            vec![slipmat_core::ipc::QueueItem {
                title: "Old song".into(),
                ..Default::default()
            }],
            0,
        );
        app.pane = Pane::Queue;
        app.message = Some(Notice::bad("old error"));
        app.bars = [1.0; spectrum::BANDS];

        app.on_event(Event::Stage(Stage::SignedOut), &link);

        assert_eq!(app.stage, Stage::SignedOut);
        assert!(app.snap.track_id.is_none());
        assert!(app.snap.title.is_empty());
        assert_eq!(app.snap.volume, 0.75);
        assert!(app.queue.items.is_empty());
        assert!(matches!(app.pane, Pane::Browser));
        assert!(app.message.is_none());
        assert_eq!(app.bars, [0.0; spectrum::BANDS]);
        assert_eq!(app.browser.view, LibraryView::Albums);
        assert_eq!(app.browser.kinds, slipmat_core::ipc::CatalogFilter::Artists);
        assert_eq!(
            app.browser.sorts[1],
            (slipmat_core::sort::SortBy::Year, true)
        );
        assert!(matches!(app.browser.showing, Showing::Library));
        assert!(app.browser.catalog_query().is_empty());
    }

    #[test]
    fn library_change_reloads_the_visible_climat_section() {
        let (link, mut requests) = link::Link::channel();
        let mut app = App {
            stage: Stage::Ready,
            ..Default::default()
        };
        app.browser.view = LibraryView::Albums;

        app.on_event(Event::LibraryChanged, &link);

        assert!(matches!(
            requests.try_recv().expect("browse request"),
            Request::Browse {
                view: LibraryView::Albums,
                ..
            }
        ));
    }

    #[test]
    fn uppercase_r_requests_a_library_refresh() {
        let (link, mut requests) = link::Link::channel();
        let mut app = App {
            stage: Stage::Ready,
            ..Default::default()
        };

        assert!(on_key(
            KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT),
            &link,
            &mut app,
        ));

        assert!(matches!(
            requests.try_recv().expect("refresh request"),
            Request::Refresh
        ));
        assert!(app.refreshing_library);
    }

    #[test]
    fn daemon_refresh_state_controls_the_climat_indicator() {
        let (link, _) = link::Link::channel();
        let mut app = App::default();

        app.on_event(Event::LibraryRefreshing { refreshing: true }, &link);
        assert!(app.refreshing_library);

        app.on_event(Event::LibraryRefreshing { refreshing: false }, &link);
        assert!(!app.refreshing_library);
    }
}
