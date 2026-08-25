// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The socket, the sidecar, and the loop that keeps them agreeing.

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::{Context, Result};
use slipmat_core::artwork;
use slipmat_core::catalog;
use slipmat_core::entry::Entry;
use slipmat_core::ipc::{
    self, CatalogFilter, Event, PageKind, Request, Stage, Transport, WriteAction,
};
use slipmat_core::music::client::Client;
use slipmat_core::music::types::Artwork;
use slipmat_core::player::protocol::{Command, Event as PlayerEvent, PlaybackState};
use slipmat_core::player::{Incoming, sidecar};
use slipmat_core::queue::{Start, queue_from_ids, start_index, unresolvable_ids};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;

use crate::bus;
use crate::heal;
use crate::state::Model;
use crate::watchdog;

/// How many events a slow client may fall behind before it is dropped.
///
/// A client that cannot keep up with a 500ms tick is not going to catch up, and
/// holding the backlog for it would make every other client's memory its
/// problem. `broadcast` tells it how far it lagged; it can ask for a snapshot.
const BACKLOG: usize = 64;

/// Position ticks while playing. The same cadence the GTK client uses.
const TICK_MS: u64 = 500;

/// How long with nobody listening and nothing playing before the sidecar goes.
///
/// It costs **393 MB of the daemon's 404** and near-zero CPU, so this is about
/// resident memory, not battery. Long enough that closing a window and
/// reopening it does not pay for a restart; short enough that a machine left
/// alone gets its memory back.
const IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// How often to ask whether the sidecar is still wanted.
const IDLE_CHECK: std::time::Duration = std::time::Duration::from_secs(30);

pub struct Daemon {
    pub model: RefCell<Model>,
    /// Replaced on every respawn, so everything holding an `Rc<Daemon>` keeps
    /// talking to whichever sidecar is alive now.
    pub sidecar: RefCell<Option<sidecar::Handle>>,
    pub events: broadcast::Sender<Event>,
    /// Whether the session has been put back since the current sidecar started.
    pub restored: std::cell::Cell<bool>,
    /// Consecutive failed starts, for the backoff. Reset once MusicKit attaches.
    pub restarts: std::cell::Cell<u32>,
    /// The last queue we asked for, and the track we aimed at. Kept for the
    /// dead-track retry, which has to rebuild it without the ids Apple refused.
    pub last_queue: RefCell<Option<(Vec<String>, Option<String>)>>,
    /// The track a new queue was meant to open on, until `verify_start` has
    /// confirmed it. `None` for a shuffled queue, which has no wrong answer.
    pub pending_start: RefCell<Option<String>>,
    /// Whether a non-playing `play` has already been healed once. A second
    /// attempt that also does nothing is a real failure.
    pub healed: std::cell::Cell<bool>,
    /// A position to restore once the reloaded track is current.
    pub resume_at: std::cell::Cell<Option<u64>>,
    /// A finished command whose ending state is worth judging, once the mirror
    /// has caught up with it.
    pub after_apply: std::cell::Cell<Option<String>>,
    /// The bus. Filled in after the daemon exists, because it needs one.
    pub mpris: RefCell<Option<slipmat_core::mpris::Mpris>>,
    /// Whether a library fetch is in flight, so a reload button cannot stack
    /// four of them.
    pub refreshing: std::cell::Cell<bool>,
    /// The artwork template already fetched, so a 500ms tick does not ask again
    /// for a file that is on disk.
    pub art_for: RefCell<Option<String>>,
    /// Clients currently connected. The sidecar is only worth holding while
    /// somebody is listening, or something is playing for them to come back to.
    pub clients: std::cell::Cell<usize>,
    /// Set when the sidecar was stopped on purpose, so the supervisor waits to
    /// be asked rather than treating it as a crash.
    pub idle: std::cell::Cell<bool>,
    /// Rung when a client turns up and the sidecar is not running.
    pub wake: tokio::sync::Notify,
    /// The desktop's own volume for this application, or `None` where there is
    /// no audio server to talk to.
    pub mixer: Option<crate::mixer::Mixer>,
    /// Rung by [`Request::Quit`], and answered where SIGTERM is — so leaving on
    /// purpose and being stopped by a service manager take the same path.
    pub quitting: tokio::sync::Notify,
}

impl Daemon {
    pub fn send(&self, cmd: Command) {
        match self.sidecar.borrow().as_ref() {
            Some(handle) => handle.send(cmd),
            // Only reachable in the moment between a client connecting and the
            // sidecar coming back — under three seconds, and nothing has been
            // drawn yet for anyone to click.
            None => tracing::debug!(cmd = cmd.name(), "dropped: the sidecar is asleep"),
        }
    }

    /// Whether the sidecar is worth holding: somebody is listening, or
    /// something is playing for them to come back to.
    fn wanted(&self) -> bool {
        self.clients.get() > 0 || self.model.borrow().player.state.is_playing()
    }

    pub fn publish(&self, event: Event) {
        // An error here means nobody is listening, which is the ordinary state
        // of a daemon with no client attached.
        let _ = self.events.send(event);
    }

    /// A client for Apple's API, if the tokens for one have arrived.
    fn client(&self) -> Option<Client> {
        let model = self.model.borrow();
        let tokens = model.tokens.as_ref()?;
        Some(Client::new(
            tokens.developer_token.clone(),
            tokens.music_user_token.clone(),
            tokens.storefront.clone(),
        ))
    }

    fn publish_snapshot(&self) {
        self.publish(Event::Snapshot(self.model.borrow().snapshot()));
        if let Some(mpris) = self.mpris.borrow().as_ref() {
            mpris.update(bus::state(self));
        }
    }
}

pub async fn run() -> Result<()> {
    let path = ipc::socket_path().context("XDG_RUNTIME_DIR is not set")?;

    // **Ask before removing.** A socket file outlives the process that made it,
    // so a crash leaves one behind that accepts nothing — but unlinking it and
    // binding a fresh inode would let a *second* daemon start beside a live
    // one. Both would spawn a sidecar, and the Chromium profile lock is the
    // thing this whole design exists to respect. So: if something answers, it
    // is not ours to replace.
    if path.exists() {
        match std::os::unix::net::UnixStream::connect(&path) {
            Ok(_) => anyhow::bail!("a daemon is already listening on {}", path.display()),
            // Nobody home — the file is a leftover.
            Err(_) => {
                tracing::info!(socket = %path.display(), "removing a stale socket");
                std::fs::remove_file(&path).with_context(|| format!("removing {path:?}"))?;
            }
        }
    }
    let listener = UnixListener::bind(&path).with_context(|| format!("binding {path:?}"))?;
    tracing::info!(socket = %path.display(), "listening");

    let (handle, mut incoming) = sidecar::spawn().context("spawning the sidecar")?;
    let (events, _) = broadcast::channel(BACKLOG);
    let daemon = Rc::new(Daemon {
        model: RefCell::new(Model::new()),
        sidecar: RefCell::new(Some(handle)),
        events,
        restored: std::cell::Cell::new(false),
        restarts: std::cell::Cell::new(0),
        last_queue: RefCell::new(None),
        pending_start: RefCell::new(None),
        healed: std::cell::Cell::new(false),
        resume_at: std::cell::Cell::new(None),
        after_apply: std::cell::Cell::new(None),
        mpris: RefCell::new(None),
        refreshing: std::cell::Cell::new(false),
        art_for: RefCell::new(None),
        clients: std::cell::Cell::new(0),
        idle: std::cell::Cell::new(false),
        wake: tokio::sync::Notify::new(),
        quitting: tokio::sync::Notify::new(),
        mixer: crate::mixer::Mixer::start(),
    });

    // After the `Rc` exists: MPRIS holds one so a button on a bar can reach the
    // sidecar.
    *daemon.mpris.borrow_mut() = Some(bus::start(&daemon));

    // Accept loop: one task per client, each holding a handle to the daemon.
    let accepting = daemon.clone();
    tokio::task::spawn_local(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let daemon = accepting.clone();
                    tokio::task::spawn_local(async move {
                        // Counted around the connection rather than inside it,
                        // so an error on the way out still releases the sidecar.
                        daemon.clients.set(daemon.clients.get() + 1);
                        // Somebody is here: if the sidecar was put down for
                        // being unwanted, it is wanted again. Woken on connect
                        // rather than on the first command, so it is ready by
                        // the time anything has been drawn to click.
                        if daemon.sidecar.borrow().is_none() {
                            daemon.wake.notify_one();
                        }
                        if let Err(err) = client(stream, daemon.clone()).await {
                            tracing::debug!(?err, "client gone");
                        }
                        daemon.clients.set(daemon.clients.get().saturating_sub(1));
                        tracing::debug!(clients = daemon.clients.get(), "client left");
                    });
                }
                Err(err) => {
                    tracing::error!(?err, "accept failed");
                    return;
                }
            }
        }
    });

    // Position ticks. Only while playing — a paused player's position is a
    // fact, not something to extrapolate from.
    let ticking = daemon.clone();
    tokio::task::spawn_local(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(TICK_MS));
        let mut watch = watchdog::Watch::default();
        loop {
            tick.tick().await;
            // **Whoever moved it, everyone hears.** The desktop's audio panel
            // can change this without Slipmat being asked, and a client still
            // showing the old number is the disagreement this whole thing
            // moved to fix.
            let outside = ticking.mixer.as_ref().and_then(|m| m.current());
            let moved = outside.is_some_and(|v| (v - ticking.model.borrow().volume).abs() > 0.005);
            if moved {
                ticking.model.borrow_mut().volume = outside.unwrap_or_default();
            }
            if moved || ticking.model.borrow().player.state.is_playing() {
                ticking.publish_snapshot();
            }
            // On every tick, not only the playing ones: a stall is measured
            // against a position that has stopped moving, and the state that
            // says so is exactly the one that stops publishing.
            watchdog::check(&ticking, &mut watch);
        }
    });

    // **Give the memory back when nobody wants it.** The sidecar is a hidden
    // Chromium: 393 MB of the daemon's 404, and measured at 0.22% of a core
    // doing nothing. Holding it while no client is connected and nothing is
    // playing buys nothing at all — and starting it again costs 0.6s warm,
    // 2.4s cold, which is cheaper than the memory it was holding.
    let idling = daemon.clone();
    tokio::task::spawn_local(async move {
        let mut unwanted_since: Option<std::time::Instant> = None;
        loop {
            tokio::time::sleep(IDLE_CHECK).await;
            if idling.sidecar.borrow().is_none() {
                unwanted_since = None;
                continue;
            }
            if idling.wanted() {
                unwanted_since = None;
                continue;
            }
            let since = *unwanted_since.get_or_insert_with(std::time::Instant::now);
            if since.elapsed() < IDLE_TIMEOUT {
                continue;
            }
            unwanted_since = None;

            tracing::info!("no clients and nothing playing — stopping the sidecar");
            // Written first: after this the mirror is the only record of what
            // was loaded, and it is about to be emptied.
            save_session(&idling);
            idling.idle.set(true);
            // Dropping the handle closes the child's stdin, which is the
            // shutdown signal `main.js` waits on; `kill_on_drop` is the backstop.
            idling.sidecar.borrow_mut().take();
            idling.model.borrow_mut().stage = Stage::Connecting;
            idling.publish(Event::Stage(Stage::Connecting));
        }
    });

    // **Leaving is a thing to do properly.** A service manager stops this with
    // SIGTERM, and that is the last moment the position is accurate — after it,
    // the process is gone and the session file still says where the last track
    // change left off.
    let leaving = daemon.clone();
    let socket = path.clone();
    tokio::task::spawn_local(async move {
        let Ok(mut term) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        else {
            return;
        };
        let mut interrupt =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()) {
                Ok(sig) => sig,
                Err(_) => return,
            };
        let why = tokio::select! {
            _ = term.recv() => "SIGTERM",
            _ = interrupt.recv() => "SIGINT",
            _ = leaving.quitting.notified() => "a client quit",
        };
        tracing::info!(why, "shutting down");
        save_session(&leaving);
        // The socket outlives the process that made it, and a leftover one
        // answers for a daemon that is not running.
        let _ = std::fs::remove_file(&socket);
        std::process::exit(0);
    });

    // **The daemon outlives its sidecar.** A child that dies takes the queue
    // with it, but not the process every client is connected to — so this
    // respawns rather than returning, and the session it saves on the way down
    // is what puts playback back where it was.
    loop {
        while let Some(message) = incoming.recv().await {
            match message {
                Incoming::Event(event) => on_event(&daemon, event),
                Incoming::Unparsed(line) => {
                    tracing::warn!(%line, "sidecar sent a line we could not parse")
                }
                Incoming::Died(why) => {
                    tracing::error!(%why, "sidecar died");
                    daemon.publish(Event::Stage(Stage::Broken { detail: why }));
                    break;
                }
            }
        }

        // **The player is gone, so nothing is playing.** Carrying the last
        // state across the gap reports `playing` over a sidecar that no longer
        // exists — the lie rule 6 exists to prevent — and it is load-bearing
        // for the watchdog: a restored queue loads *without* starting, so its
        // position is legitimately frozen, and a stale `playing` beside it read
        // as a wedge. That restarted a healthy replacement every twelve
        // seconds, forever. The queue and position are untouched; only the
        // claim to be playing goes.
        daemon.model.borrow_mut().player.state = PlaybackState::None;
        daemon.publish_snapshot();

        // Written from the live mirror, not from what was on disk: a crash
        // mid-track should come back to that track, not to last night's.
        save_session(&daemon);

        // Put down on purpose: wait to be asked rather than backing off, and do
        // not count it as a failure.
        if daemon.idle.replace(false) {
            tracing::info!("sidecar stopped; waiting for a client");
            daemon.wake.notified().await;
            tracing::info!("a client turned up — starting the sidecar");
            daemon.restarts.set(0);
        }

        let attempt = daemon.restarts.get();
        daemon.restarts.set(attempt + 1);
        if attempt > 0 {
            let delay = sidecar::restart_delay(attempt - 1);
            tracing::warn!(attempt, ?delay, "restarting the sidecar");
            tokio::time::sleep(delay).await;
        }
        daemon.publish(Event::Stage(Stage::Connecting));

        match sidecar::spawn() {
            Ok((handle, rx)) => {
                *daemon.sidecar.borrow_mut() = Some(handle);
                // The new child holds nothing, so the session goes back in once
                // MusicKit attaches to it.
                daemon.restored.set(false);
                daemon.model.borrow_mut().stage = Stage::Connecting;
                incoming = rx;
            }
            Err(err) => {
                // Reported down the same path as a crash, so there is one
                // recovery route rather than two.
                tracing::error!(?err, "could not respawn the sidecar");
                daemon.publish(Event::Stage(Stage::Broken {
                    detail: err.to_string(),
                }));
            }
        }
    }
}

/// Handle MusicKit's all-or-nothing `NOT_FOUND` by dropping the ids it named
/// and trying again.
///
/// Returns whether it took ownership of the error, so the caller does not also
/// tell a client about something it cannot act on.
fn retry_without_dead_tracks(daemon: &Daemon, detail: &str) -> bool {
    let dead = unresolvable_ids(detail);
    if dead.is_empty() {
        return false;
    }
    let Some((songs, wanted)) = daemon.last_queue.borrow_mut().take() else {
        return false;
    };

    let newly_dead = {
        let mut model = daemon.model.borrow_mut();
        let before = model.dead_ids.len();
        model.dead_ids.extend(dead);
        model.dead_ids.len() - before
    };

    // Nothing new: the retry already happened and failed again. Stop, or we
    // loop for ever on an error we cannot parse our way out of.
    if newly_dead == 0 {
        tracing::warn!("queue still unresolvable after dropping known-dead ids");
        return false;
    }
    slipmat_core::unplayable::save(&daemon.model.borrow().dead_ids);

    let dead_ids = daemon.model.borrow().dead_ids.clone();
    let retry: Vec<String> = songs
        .into_iter()
        .filter(|id| !dead_ids.contains(id))
        .collect();
    if retry.is_empty() {
        daemon.publish(Event::Error {
            detail: "None of these tracks are available to stream".into(),
        });
        return true;
    }

    let position = start_index(&retry, wanted.as_ref());
    tracing::info!(
        dropped = newly_dead,
        queue = retry.len(),
        position,
        "retrying queue without unresolvable tracks"
    );
    *daemon.pending_start.borrow_mut() = wanted.clone();
    *daemon.last_queue.borrow_mut() = Some((retry.clone(), wanted));
    daemon.send(Command::SetQueue {
        songs: retry,
        start_position: position,
        start_playing: true,
        start_time_ms: 0,
    });
    true
}

/// Remember the queue and where we are in it.
///
/// Called on every track change *and* when the sidecar dies. The second is the
/// one that matters: a crash is exactly when nobody gets to save anything, so
/// the worst case is coming back to the start of the right track rather than to
/// nothing at all.
fn save_session(daemon: &Daemon) {
    let model = daemon.model.borrow();
    let songs: Vec<String> = model
        .player
        .queue
        .iter()
        .filter_map(|item| item.catalog_id.clone().or_else(|| item.id.clone()))
        .collect();

    if songs.is_empty() {
        slipmat_core::session::clear();
        return;
    }

    slipmat_core::session::save(&slipmat_core::session::Session {
        start: model
            .player
            .queue_position
            .min(songs.len().saturating_sub(1)),
        position_ms: model.player.position_ms,
        songs,
    });
}

fn on_event(daemon: &Rc<Daemon>, event: PlayerEvent) {
    // The name, not the payload: a `NowPlaying` carries the whole queue, and
    // 112 items per line is not observability.
    tracing::debug!(event = event_name(&event), "sidecar event");
    // Stage transitions before the mirror moves, so a client is told the daemon
    // is ready by the same pass that makes it so.
    match &event {
        PlayerEvent::HookReady { authorized, .. } => {
            let stage = if *authorized {
                Stage::Ready
            } else {
                Stage::SignedOut
            };
            daemon.restarts.set(0);
            let restore = *authorized && !daemon.restored.replace(true);
            daemon.model.borrow_mut().stage = stage.clone();
            daemon.publish(Event::Stage(stage));
            // Once per run: the hook re-attaches on every navigation, and a
            // second restore would throw away whatever is playing by then.
            if restore {
                restore_session(daemon);
            }
            if *authorized {
                refresh_library(daemon);
            }
        }
        PlayerEvent::HookFailed { detail } => {
            let stage = Stage::Broken {
                detail: detail.clone(),
            };
            daemon.model.borrow_mut().stage = stage.clone();
            daemon.publish(Event::Stage(stage));
        }
        PlayerEvent::Error { detail, .. } => {
            // A refused queue is worth healing rather than reporting: `setQueue`
            // is all-or-nothing, so one delisted track makes a whole playlist
            // unplayable and the person can do nothing about it.
            if !retry_without_dead_tracks(daemon, detail) {
                daemon.publish(Event::Error {
                    detail: detail.clone(),
                });
            }
        }
        PlayerEvent::Volume { volume } => daemon.model.borrow_mut().volume = *volume,
        // The sidecar's half of a library write. Removing and un-favouriting
        // can only be done by MusicKit itself, so they settle here rather than
        // where the REST writes do — and the library is stale until a refresh
        // says otherwise.
        PlayerEvent::LibraryWrite {
            kind,
            id,
            ok,
            detail,
            ..
        } => {
            if *ok {
                tracing::info!(%kind, %id, "library write settled");
                refresh_library(daemon);
            } else {
                tracing::warn!(%kind, %id, %detail, "library write refused");
                daemon.publish(Event::Error {
                    detail: detail.clone(),
                });
            }
        }
        PlayerEvent::CmdDone { cmd, .. } => {
            // Checked after the mirror moves, below — the state this reports is
            // the one the command ended in.
            let cmd = cmd.clone();
            daemon.after_apply.set(Some(cmd));
        }
        PlayerEvent::Tokens(tokens) => {
            // Never the token itself (rule 7) — only whether we have the one
            // that matters. A developer token alone gets catalog search but not
            // playback, and the difference is otherwise invisible.
            tracing::info!(
                storefront = %tokens.storefront,
                authorized = tokens.authorized,
                has_user_token = tokens.music_user_token.is_some(),
                "tokens harvested"
            );
            daemon.model.borrow_mut().tokens = Some(tokens.clone());
        }
        _ => {}
    }

    let queue_changed = matches!(
        event,
        PlayerEvent::Queue(_) | PlayerEvent::NowPlaying { .. }
    );
    daemon.model.borrow_mut().player.apply(&event);

    if queue_changed {
        let (items, position) = daemon.model.borrow().queue();
        tracing::debug!(len = items.len(), position, "queue changed");
        daemon.publish(Event::Queue { items, position });
        // On every track change, because shutdown is the moment that might not
        // run — a SIGKILL, a session ending badly.
        save_session(daemon);
    }
    // The position goes back once there is an item to seek within, which is
    // what `nowPlayingItemDidChange` means.
    if matches!(event, PlayerEvent::NowPlaying { .. }) {
        heal::resume_position(daemon);
        fetch_artwork(daemon);
    }
    if let Some(cmd) = daemon.after_apply.take() {
        heal::play_did_nothing(daemon, &cmd);
    }
    // After the mirror has the new queue, confirm MusicKit put us on the track
    // that was actually asked for.
    heal::verify_start(daemon);

    daemon.publish_snapshot();
}

/// Is there a queue with nothing open in it?
///
/// The signature of a restore: tracks loaded, `startPlaying: false`, so
/// MusicKit holds a queue with no now-playing item to press play on.
fn needs_opening(daemon: &Daemon) -> bool {
    let model = daemon.model.borrow();
    model.player.now_playing.is_none() && !model.player.queue.is_empty()
}

async fn client(stream: UnixStream, daemon: Rc<Daemon>) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    let mut feed: Option<broadcast::Receiver<Event>> = None;

    loop {
        // Either the client says something, or — once subscribed — the daemon
        // does. `biased` so a request is never starved by a busy event stream.
        let outgoing = tokio::select! {
            biased;
            line = lines.next_line() => match line? {
                Some(line) => answer(&line, &daemon, &mut feed),
                None => return Ok(()), // client hung up
            },
            event = recv(&mut feed) => Some(event?),
        };

        if let Some(event) = outgoing {
            let mut line = serde_json::to_vec(&event)?;
            line.push(b'\n');
            write.write_all(&line).await?;
        }
    }
}

/// Wait on the event feed, or forever if this client has not subscribed.
///
/// `select!` needs both arms to be futures; a client that never subscribes
/// simply has one that never completes.
async fn recv(feed: &mut Option<broadcast::Receiver<Event>>) -> Result<Event> {
    match feed {
        Some(rx) => Ok(rx.recv().await?),
        None => std::future::pending().await,
    }
}

fn answer(
    line: &str,
    daemon: &Rc<Daemon>,
    feed: &mut Option<broadcast::Receiver<Event>>,
) -> Option<Event> {
    let request: Request = match serde_json::from_str(line) {
        Ok(request) => request,
        Err(err) => {
            // Loudly, per rule 4: a client sending something we cannot read is
            // a version mismatch, and silence would make it look like a hang.
            tracing::warn!(%line, ?err, "unreadable request");
            return Some(Event::Error {
                detail: format!("unreadable request: {err}"),
            });
        }
    };

    match request {
        Request::Subscribe => {
            if feed.is_none() {
                *feed = Some(daemon.events.subscribe());
            }
            // The first thing a subscriber gets is where things stand, so it
            // never has to draw an empty bar waiting for a change.
            Some(Event::Snapshot(daemon.model.borrow().snapshot()))
        }
        Request::Snapshot => Some(Event::Snapshot(daemon.model.borrow().snapshot())),
        Request::Stage => Some(Event::Stage(daemon.model.borrow().stage.clone())),
        Request::Queue => {
            let (items, position) = daemon.model.borrow().queue();
            Some(Event::Queue { items, position })
        }
        Request::JumpTo { index } => {
            // Nothing pending: there is no new queue to verify against.
            *daemon.pending_start.borrow_mut() = None;
            daemon.send(Command::ChangeToIndex { index });
            // Clicking a row is a request to *play* it, and
            // `changeToMediaAtIndex` only moves the cursor.
            if !daemon.model.borrow().player.state.is_playing() {
                daemon.send(Command::Play);
            }
            None
        }
        Request::Browse {
            view,
            query,
            offset,
            limit,
        } => {
            let model = daemon.model.borrow();
            let (entries, total) = model.library.browse(view, &query, offset, limit);
            Some(Event::Rows {
                view,
                entries,
                total,
            })
        }
        Request::SignIn => {
            daemon.send(Command::ShowLogin);
            None
        }
        Request::SignOut => {
            daemon.send(Command::SignOut);
            None
        }
        Request::Search {
            query,
            filter,
            offset,
        } => {
            search(daemon, query, filter, offset);
            None
        }
        Request::Open { kind, id } => {
            // Answered off this task: opening a page is a network round trip,
            // and a client waiting on one must not stop the daemon answering
            // everyone else.
            open_page(daemon, kind, id);
            None
        }
        Request::Quit => {
            // Counted around the connection, so this client is in the total.
            // Anyone else attached is a window that would lose its player.
            if daemon.clients.get() > 1 {
                return Some(Event::Error {
                    detail: "Another Slipmat client is still open".into(),
                });
            }
            tracing::info!("a client asked to quit");
            // **Through the same door a service manager uses.** Saving the
            // session and clearing the socket are already done properly on
            // SIGTERM; a second copy of that here is a second one to keep
            // right. Raising it on ourselves also lets this return first, so
            // the client is not writing into a socket that is already gone.
            daemon.quitting.notify_one();
            None
        }
        Request::Play { ids, index, start } => {
            play(daemon, &ids, index, start.into());
            None
        }
        Request::Enqueue { ids, next } => {
            // Filtered, because a dead id rejects the whole insert the same way
            // it rejects a whole queue.
            let dead = daemon.model.borrow().dead_ids.clone();
            let songs: Vec<String> = ids.into_iter().filter(|id| !dead.contains(id)).collect();
            if songs.is_empty() {
                return Some(Event::Error {
                    detail: "Nothing here can be streamed".into(),
                });
            }
            tracing::info!(count = songs.len(), next, "enqueueing");
            daemon.send(if next {
                Command::PlayNext { songs }
            } else {
                Command::PlayLater { songs }
            });
            None
        }
        Request::RemoveFromQueue { index } => {
            // **MusicKit will not remove the track it is playing**, and says
            // nothing when asked to: `queue.remove(current)` returns, fires no
            // event, and leaves the queue as it was. Measured on a five-track
            // queue — index 3 removed, index 0 did not, no error either time.
            // Silence would look like the click missed.
            let model = daemon.model.borrow();
            if index == model.player.queue_position && !model.player.queue.is_empty() {
                return Some(Event::Error {
                    detail: "Can't remove the track that's playing".into(),
                });
            }
            drop(model);
            daemon.send(Command::RemoveFromQueue { index });
            None
        }
        Request::MoveInQueue { from, to } => {
            // Optimistic, like the GTK client's drag: the mirror moves now and
            // MusicKit's echo confirms it, so a client redrawing from the next
            // snapshot does not see the row spring back.
            if daemon.model.borrow_mut().player.move_item(from, to) {
                daemon.send(Command::MoveInQueue { from, to });
                let (items, position) = daemon.model.borrow().queue();
                return Some(Event::Queue { items, position });
            }
            None
        }
        Request::ClearQueue => {
            tracing::info!("clearing the queue");
            daemon.send(Command::ClearQueue);
            // The mirror follows the sidecar's own event as always (rule 3);
            // this is the half MusicKit cannot know about.
            *daemon.last_queue.borrow_mut() = None;
            *daemon.pending_start.borrow_mut() = None;
            slipmat_core::session::clear();
            None
        }
        Request::Write { action, id } => {
            write(daemon, action, id);
            None
        }
        Request::Refresh => {
            refresh_library(daemon);
            None
        }
        Request::Transport(transport) => {
            // **A queue that was loaded but never started has no current item**,
            // and `play` needs one to act on — so it returns having done
            // nothing at all. That is the state every restore leaves behind,
            // and after a sidecar restart it is what makes a perfectly healthy
            // player refuse to start: press play, nothing happens, no error.
            // Opening the item first is what `jumpTo` already does for a click.
            if matches!(transport, Transport::Play | Transport::PlayPause) && needs_opening(daemon)
            {
                let index = daemon.model.borrow().player.queue_position;
                tracing::info!(index, "nothing is open yet — opening the queue first");
                daemon.send(Command::ChangeToIndex { index });
            }
            if let Some(command) = command_for(transport) {
                tracing::debug!(cmd = command.name(), "transport");
                daemon.send(command);
            }
            match transport {
                Transport::SetVolume { volume } => {
                    // **The stream, not the player.** MusicKit has a volume of
                    // its own and driving it left two independent gains for one
                    // application — Slipmat at 50% while the desktop's mixer
                    // said 100%, multiplying, with only one of them where
                    // anybody would look for it. This is the one the audio
                    // panel shows.
                    if let Some(mixer) = &daemon.mixer {
                        mixer.set(volume);
                    }
                    // Kept regardless: it is what a client is told while
                    // nothing is playing and there is no stream to ask.
                    daemon.model.borrow_mut().volume = volume;
                    // **Said at once, like a seek.** Nothing will ever echo this
                    // back, so without it the only way a client learns the new
                    // volume is the periodic snapshot — twice a second while
                    // playing, and *never* while paused. A meter that moves a
                    // beat late reads as a key that did not register, and a
                    // client stepping from the value it last saw sends the same
                    // number again.
                    daemon.publish_snapshot();
                }
                // **Adopted before MusicKit confirms it.** A seek takes a
                // moment to land, and the tick in between would otherwise
                // publish the position the track was at *before* the drag —
                // pulling the slider back under the finger that just moved it,
                // then throwing it forward when the real reading arrives.
                Transport::Seek { position_ms } => {
                    daemon.model.borrow_mut().player.seeked_to(position_ms);
                    // **Said immediately, not on the next tick.** A client that
                    // has just dragged the slider is extrapolating from where
                    // the track *was*; leaving it to do that for half a second
                    // is what snaps the handle back under the finger before it
                    // jumps forward again.
                    daemon.publish_snapshot();
                }
                _ => {}
            }
            None
        }
    }
}

/// Fetch the current track's cover, if we do not already have it.
///
/// **At most once per template.** A snapshot goes out twice a second and the
/// cover changes at most once a track; refetching on every one would be a
/// request per tick for a file already on disk.
fn fetch_artwork(daemon: &Rc<Daemon>) {
    let template = daemon
        .model
        .borrow()
        .player
        .now_playing
        .as_ref()
        .and_then(|item| item.artwork_template.clone());

    if template == *daemon.art_for.borrow() {
        return;
    }
    *daemon.art_for.borrow_mut() = template.clone();

    let Some(template) = template else {
        daemon.model.borrow_mut().art_path = None;
        return;
    };

    let art = Artwork::new(template);
    // Already on disk from an earlier play, or from the GTK client — the cache
    // is shared, which is the point of it living in core.
    if let Some(path) = artwork::cache_path(&art, artwork::ART_SIZE)
        && path.is_file()
    {
        daemon.model.borrow_mut().art_path = Some(path);
        return;
    }

    // Cleared first: a stale cover under a new track is worse than none, and
    // this is a network round trip.
    daemon.model.borrow_mut().art_path = None;
    let daemon = daemon.clone();
    tokio::task::spawn_local(async move {
        match artwork::fetch(art, artwork::ART_SIZE).await {
            Ok(path) => {
                daemon.model.borrow_mut().art_path = Some(path);
                daemon.publish_snapshot();
            }
            // Cosmetic: a missing cover is not worth telling anyone about.
            Err(err) => tracing::warn!(?err, "artwork not fetched"),
        }
    });
}

/// Change what Apple holds for this account.
///
/// Two routes out, and the client does not need to know which: adding and
/// favouriting go over REST, while removing and un-favouriting can only be done
/// by MusicKit itself — the identical REST calls answer
/// `400 Insufficient Permissions`.
fn write(daemon: &Rc<Daemon>, action: WriteAction, id: String) {
    match action {
        WriteAction::RemoveFromLibrary => {
            daemon.send(Command::RemoveFromLibrary { id });
            return;
        }
        WriteAction::Unfavorite => {
            daemon.send(Command::Unfavorite { id });
            return;
        }
        _ => {}
    }

    let Some(client) = daemon.client() else {
        daemon.publish(Event::Error {
            detail: "Not signed in yet".into(),
        });
        return;
    };
    let daemon = daemon.clone();
    tokio::task::spawn_local(async move {
        let result = match action {
            WriteAction::Favorite => client.add_to_favorites("songs", &id).await,
            WriteAction::AddToLibrary => client.add_to_library("songs", &id).await,
            _ => unreachable!("handled above"),
        };
        match result {
            // Apple answers 202 Accepted with an empty body — "acceptable, may
            // not have completed" — so this is *sent*, not done. The refresh is
            // what makes it true, which is why nothing here edits the mirror.
            Ok(()) => {
                tracing::info!(?action, %id, "library write sent");
                refresh_library(&daemon);
            }
            Err(err) => {
                tracing::warn!(?err, ?action, %id, "library write failed");
                daemon.publish(Event::Error {
                    detail: format!("{err}"),
                });
            }
        }
    });
}

/// Re-read the library from Apple and tell clients it moved.
///
/// Spawned, and at most one at a time: a client hammering a reload button must
/// not queue up four full library fetches behind it.
fn refresh_library(daemon: &Rc<Daemon>) {
    if daemon.refreshing.replace(true) {
        tracing::debug!("library refresh already running");
        return;
    }
    let Some(client) = daemon.client() else {
        daemon.refreshing.set(false);
        return;
    };

    let daemon = daemon.clone();
    tokio::task::spawn_local(async move {
        const MAX: usize = 10_000;
        let fetched = tokio::join!(
            client.all_library_songs(MAX),
            client.all_library_albums(MAX),
            client.all_library_artists(MAX),
            client.all_library_playlists(MAX),
        );
        daemon.refreshing.set(false);

        match fetched {
            (Ok(songs), Ok(albums), Ok(artists), Ok(playlists)) => {
                tracing::info!(
                    songs = songs.len(),
                    albums = albums.len(),
                    artists = artists.len(),
                    playlists = playlists.len(),
                    "library refreshed"
                );
                slipmat_core::library_cache::save(&songs, &albums, &artists, &playlists);
                let mut model = daemon.model.borrow_mut();
                model.library.tracks = songs;
                model.library.albums = albums;
                model.library.artists = artists;
                model.library.playlists = playlists;
                drop(model);
                daemon.publish(Event::LibraryChanged);
            }
            _ => tracing::warn!("library refresh failed; keeping what was cached"),
        }
    });
}

/// Tracks as rows.
fn songs(tracks: Vec<slipmat_core::music::types::Track>) -> Vec<Entry> {
    tracks.into_iter().map(Entry::Song).collect()
}

/// Search the catalog and announce what came back.
///
/// Spawned like `open_page`, and for the same reason: this is a network round
/// trip, and a client waiting on one must not stop the daemon answering anyone
/// else. The answer carries the query it belongs to, because somebody types
/// faster than Apple replies.
fn search(daemon: &Rc<Daemon>, query: String, filter: CatalogFilter, offset: usize) {
    let Some(client) = daemon.client() else {
        daemon.publish(Event::Error {
            detail: "Not signed in yet".into(),
        });
        return;
    };

    let daemon = daemon.clone();
    tokio::task::spawn_local(async move {
        let found = client
            .search(&query, filter.types(), catalog::CATALOG_LIMIT, offset)
            .await;
        match found {
            Ok(results) => {
                let (entries, paged) = catalog::catalog_rows(filter, results, offset == 0);
                tracing::info!(%query, ?filter, offset, rows = entries.len(), "searched");
                daemon.publish(Event::Results {
                    query,
                    entries,
                    offset,
                    // A short page is the last page: Apple gave us fewer than
                    // it caps at, so there is nothing behind it.
                    more: paged >= catalog::CATALOG_LIMIT as usize,
                });
            }
            Err(err) => {
                tracing::warn!(?err, %query, "search failed");
                daemon.publish(Event::Error {
                    detail: format!("{err}"),
                });
            }
        }
    });
}

/// Fetch an album, artist or playlist and announce it.
///
/// Spawned rather than awaited: this is a network round trip, and a client
/// waiting on one must not stop the daemon answering anybody else. The answer
/// arrives as an [`Event::Page`] on every subscriber, which is also what lets a
/// second client show a page the first one opened.
fn open_page(daemon: &Rc<Daemon>, kind: PageKind, id: String) {
    let Some(client) = daemon.client() else {
        daemon.publish(Event::Error {
            detail: "Not signed in yet".into(),
        });
        return;
    };

    let daemon = daemon.clone();
    tokio::task::spawn_local(async move {
        // **An artist page is their albums, not their tracks**, which is why
        // this is not one call with a different id. Catalog and library are
        // separate for the other reason: the two id spaces are not
        // interchangeable, and a catalog id 404s against `/me/library`.
        let fetched = match kind {
            PageKind::Album => client
                .album(&id)
                .await
                .map(|(album, tracks)| (Entry::Album(album), songs(tracks))),
            PageKind::LibraryAlbum => client
                .library_album(&id)
                .await
                .map(|(album, tracks)| (Entry::Album(album), songs(tracks))),
            PageKind::Playlist => client
                .playlist(&id)
                .await
                .map(|(list, tracks)| (Entry::Playlist(list), songs(tracks))),
            PageKind::LibraryPlaylist => client
                .library_playlist(&id)
                .await
                .map(|(list, tracks)| (Entry::Playlist(list), songs(tracks))),
            PageKind::Artist => client.artist_albums(&id).await.map(|(artist, albums)| {
                (
                    Entry::Artist(artist),
                    albums.into_iter().map(Entry::Album).collect(),
                )
            }),
            PageKind::LibraryArtist => {
                client
                    .library_artist_albums(&id)
                    .await
                    .map(|(artist, albums)| {
                        (
                            Entry::Artist(artist),
                            albums.into_iter().map(Entry::Album).collect(),
                        )
                    })
            }
        };

        match fetched {
            Ok((header, entries)) => daemon.publish(Event::Page {
                kind,
                id,
                header,
                entries,
            }),
            Err(err) => {
                tracing::warn!(?err, %id, "opening a page failed");
                daemon.publish(Event::Error {
                    detail: format!("{err}"),
                });
            }
        }
    });
}

/// Build a queue from ids a client drew, and start it.
///
/// The arithmetic is `slipmat_core::queue`'s, the same the GTK client uses —
/// which is the point of it living there. Two implementations would be two
/// answers to "which song did they click".
fn play(daemon: &Daemon, ids: &[String], index: usize, start: Start) {
    let visible: Vec<Option<String>> = ids.iter().cloned().map(Some).collect();
    let row = if start.reorders() {
        // Shuffle: the entry point is the random part (#147).
        random_row(ids.len())
    } else {
        index
    };

    let dead = daemon.model.borrow().dead_ids.clone();
    let (songs, start_id) = queue_from_ids(&visible, row, &dead);
    if songs.is_empty() {
        daemon.publish(Event::Error {
            detail: "Nothing here can be streamed".into(),
        });
        return;
    }

    let position = start_index(&songs, start_id.as_ref());
    tracing::info!(queue = songs.len(), position, ?start, "enqueuing");
    // Nothing to verify when the order is not ours: MusicKit reorders as it
    // loads, so no track is the *wrong* one to open on (#152).
    *daemon.pending_start.borrow_mut() = if start.reorders() {
        None
    } else {
        start_id.clone()
    };
    *daemon.last_queue.borrow_mut() = Some((songs.clone(), start_id));
    if let Some(shuffle) = start.mode(true) {
        daemon.send(Command::SetShuffle { shuffle });
    }
    daemon.send(Command::SetQueue {
        songs,
        start_position: position,
        start_playing: true,
        start_time_ms: 0,
    });
}

/// A row to open a shuffle on.
///
/// Not `rand`: one number, once per shuffled play, and a dependency for it
/// would be the largest thing in this binary's tree.
fn random_row(len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0);
    nanos % len
}

/// Put back what was playing when Slipmat last closed.
///
/// **Loaded, not started.** A daemon that makes noise because it was started is
/// worse than an app that does — nobody asked, and it may have been started by
/// a login script.
fn restore_session(daemon: &Daemon) {
    let Some(session) = slipmat_core::session::load() else {
        return;
    };
    let start = session.start.min(session.songs.len().saturating_sub(1));
    tracing::info!(
        tracks = session.songs.len(),
        start,
        position_ms = session.position_ms,
        "restoring the last session"
    );
    // Sequential: the saved order *is* what was playing, and `start` indexes
    // into it (#152).
    daemon.send(Command::SetShuffle { shuffle: false });
    let wanted = session.songs.get(start).cloned();
    *daemon.pending_start.borrow_mut() = wanted.clone();
    *daemon.last_queue.borrow_mut() = Some((session.songs.clone(), wanted));
    daemon.send(Command::SetQueue {
        songs: session.songs,
        start_position: start,
        start_playing: false,
        start_time_ms: session.position_ms,
    });
}

/// The event's kind, for a log line that stays one line.
fn event_name(event: &PlayerEvent) -> &'static str {
    match event {
        PlayerEvent::NowPlaying { .. } => "nowPlaying",
        PlayerEvent::Queue(_) => "queue",
        PlayerEvent::PlaybackState { .. } => "playbackState",
        PlayerEvent::CmdRecv { .. } => "cmdRecv",
        PlayerEvent::CmdDone { .. } => "cmdDone",
        PlayerEvent::Tokens(_) => "tokens",
        PlayerEvent::Error { .. } => "error",
        _ => "other",
    }
}

/// What the sidecar is told, if anything.
///
/// **Volume is `None` on purpose.** It is applied to the audio stream instead
/// (see `mixer`), and sending it here as well would put two gains in series on
/// one player — which is the bug that moved it in the first place, not a
/// belt-and-braces.
fn command_for(transport: Transport) -> Option<Command> {
    Some(match transport {
        Transport::Play => Command::Play,
        Transport::Pause => Command::Pause,
        Transport::PlayPause => Command::PlayPause,
        Transport::Next => Command::Next,
        Transport::Previous => Command::Previous,
        Transport::Seek { position_ms } => Command::Seek { position_ms },
        Transport::SetVolume { .. } => return None,
        Transport::SetShuffle { shuffle } => Command::SetShuffle { shuffle },
        Transport::SetRepeat { mode } => Command::SetRepeat { mode },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_transport_verb_has_a_sidecar_command() {
        // The two enums are deliberately separate — the wire is a contract with
        // clients, `Command` is one with the sidecar — so this is what stops
        // them drifting into disagreement.
        let verbs = [
            Transport::Play,
            Transport::Pause,
            Transport::PlayPause,
            Transport::Next,
            Transport::Previous,
            Transport::Seek { position_ms: 1 },
            Transport::SetVolume { volume: 0.5 },
            Transport::SetShuffle { shuffle: true },
            Transport::SetRepeat {
                mode: slipmat_core::player::protocol::RepeatMode::All,
            },
        ];
        for verb in verbs {
            // Exhaustive by construction; the point is that it compiles and
            // runs. Volume is the one that deliberately maps to nothing —
            // asserted rather than left implied, because a future edit adding
            // it back would silently put two gains in series again.
            let sent = command_for(verb);
            assert_eq!(
                sent.is_none(),
                matches!(verb, Transport::SetVolume { .. }),
                "{verb:?} mapped to {sent:?}"
            );
        }
    }
}
