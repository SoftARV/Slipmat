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

mod link;
mod queue;
mod ui;

use anyhow::Result;
use crossterm::event::{Event as TermEvent, EventStream, KeyCode, KeyEventKind};
use crossterm::terminal::{LeaveAlternateScreen, disable_raw_mode};
use futures::StreamExt;
use slipmat_core::ipc::{Event, Request, Snapshot, Stage, Transport};
use slipmat_core::player::protocol::RepeatMode;

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
    queue: queue::Queue,
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
/// **No arm here changes the queue.** Each sends a request and the rows move
/// when the daemon echoes — rule 3 at the third layer. The cursor is the one
/// thing this owns, so it moves immediately.
fn on_key(code: KeyCode, link: &link::Link, app: &mut App) -> bool {
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

        KeyCode::Up | KeyCode::Char('k') => app.queue.move_cursor(-1),
        KeyCode::Down | KeyCode::Char('j') => app.queue.move_cursor(1),
        KeyCode::PageUp => app.queue.move_cursor(-10),
        KeyCode::PageDown => app.queue.move_cursor(10),
        // Back to the music, for a cursor that has wandered down a long queue.
        KeyCode::Home => app.queue.follow(),
        KeyCode::Enter => link.send(Request::JumpTo {
            index: app.queue.cursor,
        }),
        KeyCode::Char('d') => link.send(Request::RemoveFromQueue {
            index: app.queue.cursor,
        }),
        // Shift moves the row rather than the cursor. The cursor goes with it,
        // so the selection stays on the track being moved and a second press
        // moves the same one again.
        KeyCode::Char('K') => reorder(link, app, -1),
        KeyCode::Char('J') => reorder(link, app, 1),

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
