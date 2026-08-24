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
mod ui;

use anyhow::Result;
use crossterm::event::{Event as TermEvent, EventStream, KeyCode, KeyEventKind};
use crossterm::terminal::{LeaveAlternateScreen, disable_raw_mode};
use futures::StreamExt;
use slipmat_core::ipc::{Event, Request, Snapshot, Stage, Transport};

/// How often to redraw while playing.
///
/// The daemon sends a snapshot twice a second; this is what makes the clock
/// tick between them rather than stepping half a second at a time.
const FRAME_MS: u64 = 100;

#[derive(Default)]
struct App {
    snap: Snapshot,
    stage: Stage,
    queue_len: usize,
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
                        queue_len: app.queue_len,
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
fn on_key(code: KeyCode, link: &link::Link, app: &mut App) -> bool {
    match code {
        // Winamp's own transport row, where it has been since 1997.
        KeyCode::Char('z') => link.send(Request::Transport(Transport::Previous)),
        KeyCode::Char('x') => link.send(Request::Transport(Transport::Play)),
        KeyCode::Char('c') => link.send(Request::Transport(Transport::Pause)),
        KeyCode::Char('b') => link.send(Request::Transport(Transport::Next)),
        KeyCode::Char(' ') => link.send(Request::Transport(Transport::PlayPause)),
        KeyCode::Left => seek(link, app, -5_000),
        KeyCode::Right => seek(link, app, 5_000),
        KeyCode::Char('_') | KeyCode::Char('-') => return false,
        KeyCode::Char('q') => {
            app.quit_daemon = true;
            return false;
        }
        _ => {}
    }
    true
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
            Event::Queue { items, .. } => self.queue_len = items.len(),
            Event::Stage(stage) => self.stage = stage,
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
