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
use crate::library::Library;
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
    /// The saved queue occurrence until MusicKit reports the restored queue.
    pub restore_start: std::cell::Cell<Option<usize>>,
    /// An occurrence selected through MPRIS, restarted when that exact item arrives.
    pub restart_at: RefCell<Option<String>>,
    /// A finished command whose ending state is worth judging, once the mirror
    /// has caught up with it.
    pub after_apply: std::cell::Cell<Option<String>>,
    /// The bus. Filled in after the daemon exists, because it needs one.
    pub mpris: RefCell<Option<slipmat_core::mpris::Mpris>>,
    /// Whether a library fetch is in flight, so a reload button cannot stack
    /// four of them. The generation lets a new account refresh without waiting
    /// for the old account's request to finish.
    pub refreshing: std::cell::Cell<Option<u64>>,
    /// Changes whenever authorization crosses an account boundary.
    pub authorization_generation: std::cell::Cell<u64>,
    /// The authorization generation whose automatic freshness check started.
    pub automatic_refresh: std::cell::Cell<Option<u64>>,
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

    fn library_client(&self) -> Option<Client> {
        let model = self.model.borrow();
        let tokens = model.tokens.as_ref()?;
        if model.stage != Stage::Ready || !tokens.authorized || tokens.music_user_token.is_none() {
            return None;
        }
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
        restore_start: std::cell::Cell::new(None),
        restart_at: RefCell::new(None),
        after_apply: std::cell::Cell::new(None),
        mpris: RefCell::new(None),
        refreshing: std::cell::Cell::new(None),
        authorization_generation: std::cell::Cell::new(0),
        automatic_refresh: std::cell::Cell::new(None),
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
        daemon.restart_at.borrow_mut().take();
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
    if daemon.restore_start.get().is_some() {
        daemon.restore_start.set(Some(position));
    }
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
    let signed_out = matches!(
        &event,
        PlayerEvent::HookReady {
            authorized: false,
            ..
        } | PlayerEvent::Authorization { authorized: false }
            | PlayerEvent::SignedOut
    );
    match &event {
        PlayerEvent::HookReady { authorized, .. } => {
            daemon.restarts.set(0);
            set_authorization(daemon, *authorized);
        }
        PlayerEvent::Authorization { authorized } => set_authorization(daemon, *authorized),
        PlayerEvent::SignedOut => set_authorization(daemon, false),
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
        // Older installed sidecars still report MusicKit's persisted gain.
        // The desktop stream owns volume now, so accepting this would let a
        // hidden second gain overwrite the number every client displays.
        PlayerEvent::Volume { .. } => {}
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
            maybe_refresh_library(daemon);
        }
        _ => {}
    }

    if signed_out {
        return;
    }

    let reported_queue = match &event {
        PlayerEvent::Queue(queue) | PlayerEvent::NowPlaying { queue, .. } => Some(queue),
        _ => None,
    };
    if daemon.restore_start.get().is_some()
        && reported_queue.is_some_and(|queue| !heal::is_last_queue(daemon, &queue.items))
    {
        tracing::debug!("discarded queue event from before session restoration");
        return;
    }

    let queue_changed = matches!(
        event,
        PlayerEvent::Queue(_) | PlayerEvent::NowPlaying { .. }
    );
    daemon.model.borrow_mut().player.apply(&event);

    if queue_changed {
        restore_queue_selection(daemon);
    }
    // The position goes back once there is an item to seek within, which is
    // what `nowPlayingItemDidChange` means. Do this before persisting the event
    // so its temporary zero cannot replace the saved position.
    if matches!(event, PlayerEvent::NowPlaying { .. }) {
        if let Some(command) = heal::resume_position(daemon) {
            daemon.send(command);
        }
        if let Some(command) = restart_selected_occurrence(daemon) {
            daemon.send(command);
        }
    }
    if queue_changed {
        let (items, position) = daemon.model.borrow().queue();
        tracing::debug!(len = items.len(), position, "queue changed");
        daemon.publish(Event::Queue { items, position });
        // On every track change, because shutdown is the moment that might not
        // run — a SIGKILL, a session ending badly.
        save_session(daemon);
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

fn set_authorization(daemon: &Rc<Daemon>, authorized: bool) {
    let stage = if authorized {
        Stage::Ready
    } else {
        Stage::SignedOut
    };
    if daemon.model.borrow().stage != stage {
        daemon
            .authorization_generation
            .set(daemon.authorization_generation.get().saturating_add(1));
        daemon.refreshing.set(None);
    }

    if !authorized {
        clear_account_state(daemon);
        return;
    }

    let restore = !daemon.restored.replace(true);
    daemon.model.borrow_mut().stage = Stage::Ready;
    daemon.publish(Event::Stage(Stage::Ready));
    daemon.send(Command::Hide);
    // Once per run: the hook re-attaches on every navigation, and a second
    // restore would throw away whatever is playing by then.
    if restore {
        restore_session(daemon);
    }
    maybe_refresh_library(daemon);
}

fn clear_account_state(daemon: &Daemon) {
    let mut model = daemon.model.borrow_mut();
    model.clear_account_state();
    model.stage = Stage::SignedOut;
    drop(model);

    daemon.last_queue.borrow_mut().take();
    daemon.pending_start.borrow_mut().take();
    daemon.healed.set(false);
    daemon.resume_at.set(None);
    daemon.restore_start.set(None);
    daemon.restart_at.borrow_mut().take();
    daemon.after_apply.take();
    daemon.art_for.borrow_mut().take();
    slipmat_core::library_cache::clear();
    slipmat_core::session::clear();

    daemon.publish(Event::Stage(Stage::SignedOut));
    daemon.publish(Event::Queue {
        items: Vec::new(),
        position: 0,
    });
    daemon.publish_snapshot();
    daemon.publish(Event::LibraryChanged);
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
            daemon.restart_at.borrow_mut().take();
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
            sort,
            reverse,
        } => {
            let model = daemon.model.borrow();
            let (entries, total) = model
                .library
                .browse(view, &query, offset, limit, sort, reverse);
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
            daemon.resume_at.set(None);
            daemon.restore_start.set(None);
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
            route_transport(daemon, transport);
            None
        }
    }
}

pub(crate) fn route_transport(daemon: &Rc<Daemon>, transport: Transport) {
    if matches!(
        transport,
        Transport::Next | Transport::Previous | Transport::Seek { .. }
    ) {
        daemon.restart_at.borrow_mut().take();
        daemon.resume_at.set(None);
        daemon.restore_start.set(None);
    }
    // A restored queue has no current item, so MusicKit's `play` does nothing
    // until the current queue entry has been opened.
    if matches!(transport, Transport::Play | Transport::PlayPause) && needs_opening(daemon) {
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
            let volume = volume.clamp(0.0, 1.0);
            if let Some(mixer) = &daemon.mixer {
                mixer.set(volume);
            }
            // Keep the requested value while no stream exists, and publish it
            // at once so paused clients can step from the new value.
            daemon.model.borrow_mut().volume = volume;
            daemon.publish_snapshot();
        }
        // Adopt a seek before MusicKit confirms it so the next position tick
        // cannot pull a client's slider back to the old position.
        Transport::Seek { position_ms } => {
            daemon.model.borrow_mut().player.seeked_to(position_ms);
            daemon.publish_snapshot();
        }
        _ => {}
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
        .current_item()
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
fn maybe_refresh_library(daemon: &Rc<Daemon>) {
    let generation = daemon.authorization_generation.get();
    if daemon.automatic_refresh.get() == Some(generation) {
        return;
    }
    if refresh_library(daemon) {
        daemon.automatic_refresh.set(Some(generation));
    }
}

fn refresh_library(daemon: &Rc<Daemon>) -> bool {
    let Some(client) = daemon.library_client() else {
        return false;
    };
    let Some(generation) = begin_library_refresh(daemon) else {
        tracing::debug!("library refresh already running");
        return true;
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
        if daemon.authorization_generation.get() != generation {
            tracing::debug!(generation, "discarded stale library refresh");
            return;
        }

        match fetched {
            (Ok(songs), Ok(albums), Ok(artists), Ok(playlists)) => {
                let next = Library {
                    tracks: songs,
                    albums,
                    artists,
                    playlists,
                };
                if commit_library(&daemon, generation, next, |library| {
                    slipmat_core::library_cache::save(
                        &library.tracks,
                        &library.albums,
                        &library.artists,
                        &library.playlists,
                    )
                }) {
                    let library = &daemon.model.borrow().library;
                    tracing::info!(
                        songs = library.tracks.len(),
                        albums = library.albums.len(),
                        artists = library.artists.len(),
                        playlists = library.playlists.len(),
                        "library refreshed"
                    );
                }
            }
            _ => tracing::warn!("library refresh failed; keeping what was cached"),
        }
        finish_library_refresh(&daemon, generation);
    });
    true
}

fn commit_library(
    daemon: &Daemon,
    generation: u64,
    next: Library,
    persist: impl FnOnce(&Library) -> bool,
) -> bool {
    if daemon.authorization_generation.get() != generation {
        return false;
    }
    if daemon.model.borrow().library == next {
        tracing::info!("library unchanged");
        return false;
    }
    if !persist(&next) {
        tracing::warn!("library refresh could not be persisted; keeping the cache");
        return false;
    }
    daemon.model.borrow_mut().library = next;
    daemon.publish(Event::LibraryChanged);
    true
}

fn begin_library_refresh(daemon: &Daemon) -> Option<u64> {
    let generation = daemon.authorization_generation.get();
    if daemon.refreshing.get() == Some(generation) {
        return None;
    }
    daemon.refreshing.set(Some(generation));
    daemon.publish(Event::LibraryRefreshing { refreshing: true });
    Some(generation)
}

fn finish_library_refresh(daemon: &Daemon, generation: u64) -> bool {
    let current = daemon.authorization_generation.get() == generation;
    if daemon.refreshing.get() == Some(generation) {
        daemon.refreshing.set(None);
        if current {
            daemon.publish(Event::LibraryRefreshing { refreshing: false });
        }
    }
    current
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
    daemon.resume_at.set(None);
    daemon.restore_start.set(None);

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
    {
        let mut model = daemon.model.borrow_mut();
        model.player.queue_position = start;
        model.player.position_ms = session.position_ms;
    }
    daemon.restore_start.set(Some(start));
    daemon
        .resume_at
        .set((session.position_ms > 0).then_some(session.position_ms));
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

fn restore_queue_selection(daemon: &Daemon) {
    let Some(start) = daemon.restore_start.get() else {
        return;
    };
    let model = daemon.model.borrow();
    if model.player.queue.is_empty() || !heal::is_last_queue(daemon, &model.player.queue) {
        return;
    }
    let has_now_playing = model.player.now_playing.is_some();
    let len = model.player.queue.len();
    drop(model);

    if has_now_playing {
        daemon.restore_start.set(None);
    } else {
        daemon.model.borrow_mut().player.queue_position = start.min(len - 1);
    }
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

fn restart_selected_occurrence(daemon: &Daemon) -> Option<Command> {
    let target = daemon.restart_at.borrow().clone()?;
    let model = daemon.model.borrow();
    let arrived = model
        .player
        .now_playing
        .as_ref()
        .is_some_and(|item| item.occurrence_id == target);
    let still_queued = model
        .player
        .queue
        .iter()
        .any(|item| item.occurrence_id == target);
    drop(model);

    if arrived {
        daemon.restart_at.borrow_mut().take();
        return Some(Command::Seek { position_ms: 0 });
    }
    if !still_queued {
        daemon.restart_at.borrow_mut().take();
    }
    None
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
    use slipmat_core::music::types::{Artwork, Track, TrackId};
    use slipmat_core::player::protocol::{Item, Queue, RepeatMode, Tokens};

    fn daemon() -> Rc<Daemon> {
        let (events, _) = broadcast::channel(8);
        Rc::new(Daemon {
            model: RefCell::new(Model::new()),
            sidecar: RefCell::new(None),
            events,
            restored: std::cell::Cell::new(false),
            restarts: std::cell::Cell::new(0),
            last_queue: RefCell::new(None),
            pending_start: RefCell::new(None),
            healed: std::cell::Cell::new(false),
            resume_at: std::cell::Cell::new(None),
            restore_start: std::cell::Cell::new(None),
            restart_at: RefCell::new(None),
            after_apply: std::cell::Cell::new(None),
            mpris: RefCell::new(None),
            refreshing: std::cell::Cell::new(None),
            authorization_generation: std::cell::Cell::new(0),
            automatic_refresh: std::cell::Cell::new(None),
            art_for: RefCell::new(None),
            clients: std::cell::Cell::new(0),
            idle: std::cell::Cell::new(false),
            wake: tokio::sync::Notify::new(),
            mixer: None,
            quitting: tokio::sync::Notify::new(),
        })
    }

    fn usable_tokens() -> Tokens {
        Tokens {
            developer_token: "developer".into(),
            music_user_token: Some("user".into()),
            storefront: "us".into(),
            authorized: true,
        }
    }

    fn refresh_events(events: &mut broadcast::Receiver<Event>) -> Vec<bool> {
        let mut refreshing = Vec::new();
        while let Ok(event) = events.try_recv() {
            if let Event::LibraryRefreshing { refreshing: state } = event {
                refreshing.push(state);
            }
        }
        refreshing
    }

    fn library_with_track(title: &str) -> crate::library::Library {
        crate::library::Library {
            tracks: vec![Track {
                date_added: String::new(),
                year: String::new(),
                favorite: false,
                in_library: true,
                library_id: None,
                id: TrackId(title.into()),
                catalog_id: Some(title.into()),
                title: title.into(),
                artist: String::new(),
                album: String::new(),
                duration_ms: 0,
                track_number: 0,
                artwork: None,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn a_legacy_musickit_volume_event_cannot_replace_stream_volume() {
        let daemon = daemon();
        daemon.model.borrow_mut().volume = 0.75;

        on_event(&daemon, PlayerEvent::Volume { volume: 0.0 });

        assert_eq!(daemon.model.borrow().volume, 0.75);
    }

    #[test]
    fn authorization_completion_makes_the_daemon_ready() {
        let daemon = daemon();
        daemon.model.borrow_mut().stage = Stage::SignedOut;

        on_event(&daemon, PlayerEvent::Authorization { authorized: true });

        assert_eq!(daemon.model.borrow().stage, Stage::Ready);
    }

    #[test]
    fn tokens_wait_for_readiness_before_refreshing_the_library() {
        let daemon = daemon();
        let mut events = daemon.events.subscribe();
        let local = tokio::task::LocalSet::new();
        let _entered = local.enter();

        on_event(&daemon, PlayerEvent::Tokens(usable_tokens()));

        assert_eq!(daemon.refreshing.get(), None);
        assert!(refresh_events(&mut events).is_empty());

        on_event(&daemon, PlayerEvent::Authorization { authorized: true });
        on_event(&daemon, PlayerEvent::Authorization { authorized: true });
        on_event(&daemon, PlayerEvent::Tokens(usable_tokens()));

        assert_eq!(daemon.refreshing.get(), Some(1));
        assert_eq!(refresh_events(&mut events), [true]);
    }

    #[test]
    fn readiness_waits_for_tokens_before_refreshing_the_library() {
        let daemon = daemon();
        let mut events = daemon.events.subscribe();
        let local = tokio::task::LocalSet::new();
        let _entered = local.enter();

        on_event(&daemon, PlayerEvent::Authorization { authorized: true });

        assert_eq!(daemon.refreshing.get(), None);
        assert!(refresh_events(&mut events).is_empty());

        on_event(&daemon, PlayerEvent::Tokens(usable_tokens()));
        on_event(&daemon, PlayerEvent::Tokens(usable_tokens()));
        refresh_library(&daemon);
        on_event(
            &daemon,
            PlayerEvent::LibraryWrite {
                kind: "remove".into(),
                id: "song".into(),
                ok: true,
                detail: String::new(),
            },
        );

        assert_eq!(daemon.refreshing.get(), Some(1));
        assert_eq!(refresh_events(&mut events), [true]);
    }

    #[test]
    fn an_automatic_trigger_coalesces_with_an_in_flight_manual_refresh() {
        let daemon = daemon();
        daemon.model.borrow_mut().stage = Stage::Ready;
        daemon.model.borrow_mut().tokens = Some(usable_tokens());
        let mut events = daemon.events.subscribe();
        let local = tokio::task::LocalSet::new();
        let _entered = local.enter();

        assert!(refresh_library(&daemon));
        on_event(&daemon, PlayerEvent::Tokens(usable_tokens()));
        assert_eq!(daemon.automatic_refresh.get(), Some(0));

        finish_library_refresh(&daemon, 0);
        on_event(&daemon, PlayerEvent::Tokens(usable_tokens()));

        assert_eq!(daemon.refreshing.get(), None);
        assert_eq!(refresh_events(&mut events), [true, false]);
    }

    #[test]
    fn an_unchanged_library_is_not_persisted_or_published() {
        let daemon = daemon();
        daemon.model.borrow_mut().library = crate::library::Library::default();
        let persisted = std::cell::Cell::new(0);
        let mut events = daemon.events.subscribe();

        assert!(!commit_library(
            &daemon,
            0,
            crate::library::Library::default(),
            |_| {
                persisted.set(persisted.get() + 1);
                true
            },
        ));

        assert_eq!(persisted.get(), 0);
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn a_changed_library_is_persisted_then_published_once() {
        let daemon = daemon();
        daemon.model.borrow_mut().library = crate::library::Library::default();
        let persisted = std::cell::Cell::new(0);
        let mut events = daemon.events.subscribe();

        assert!(commit_library(
            &daemon,
            0,
            library_with_track("new"),
            |_| {
                persisted.set(persisted.get() + 1);
                true
            },
        ));

        assert_eq!(persisted.get(), 1);
        assert_eq!(daemon.model.borrow().library.tracks[0].title, "new");
        assert!(matches!(events.try_recv(), Ok(Event::LibraryChanged)));
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn a_persistence_failure_keeps_the_known_good_library() {
        let daemon = daemon();
        daemon.model.borrow_mut().library = library_with_track("old");
        let mut events = daemon.events.subscribe();

        assert!(!commit_library(
            &daemon,
            0,
            library_with_track("new"),
            |_| false,
        ));

        assert_eq!(daemon.model.borrow().library.tracks[0].title, "old");
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn a_stale_library_result_changes_nothing() {
        let daemon = daemon();
        daemon.model.borrow_mut().library = library_with_track("current");
        daemon.authorization_generation.set(1);
        daemon.refreshing.set(Some(1));
        let mut events = daemon.events.subscribe();

        assert!(!commit_library(
            &daemon,
            0,
            library_with_track("stale"),
            |_| panic!("a stale result must not reach persistence"),
        ));

        assert_eq!(daemon.model.borrow().library.tracks[0].title, "current");
        assert_eq!(daemon.refreshing.get(), Some(1));
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn stale_refresh_cannot_finish_or_release_the_current_guard() {
        let daemon = daemon();
        assert_eq!(daemon.authorization_generation.get(), 0);

        on_event(&daemon, PlayerEvent::Authorization { authorized: false });
        let signed_out = daemon.authorization_generation.get();
        on_event(&daemon, PlayerEvent::SignedOut);
        assert_eq!(daemon.authorization_generation.get(), signed_out);

        on_event(&daemon, PlayerEvent::Authorization { authorized: true });
        let current = daemon.authorization_generation.get();
        assert!(current > signed_out);
        on_event(&daemon, PlayerEvent::Authorization { authorized: true });
        assert_eq!(daemon.authorization_generation.get(), current);

        let mut events = daemon.events.subscribe();
        daemon.refreshing.set(Some(signed_out));
        assert_eq!(begin_library_refresh(&daemon), Some(current));
        assert!(matches!(
            events.try_recv(),
            Ok(Event::LibraryRefreshing { refreshing: true })
        ));
        assert_eq!(begin_library_refresh(&daemon), None);
        assert!(!finish_library_refresh(&daemon, signed_out));
        assert!(events.try_recv().is_err());
        assert_eq!(daemon.refreshing.get(), Some(current));
        assert!(finish_library_refresh(&daemon, current));
        assert!(matches!(
            events.try_recv(),
            Ok(Event::LibraryRefreshing { refreshing: false })
        ));
        assert_eq!(daemon.refreshing.get(), None);
    }

    fn populate_account_state(daemon: &Daemon) {
        let item = Item {
            occurrence_id: "old:1".into(),
            id: Some("old-library-id".into()),
            catalog_id: Some("old-catalog-id".into()),
            title: "Old song".into(),
            ..Default::default()
        };
        let mut model = daemon.model.borrow_mut();
        model.volume = 0.75;
        model.dead_ids.insert("globally-dead".into());
        model.tokens = Some(Tokens {
            developer_token: "developer".into(),
            music_user_token: Some("user".into()),
            storefront: "us".into(),
            authorized: true,
        });
        model.library.tracks.push(Track {
            date_added: String::new(),
            year: String::new(),
            favorite: false,
            in_library: true,
            library_id: Some("old-library-id".into()),
            id: TrackId("old-library-id".into()),
            catalog_id: Some("old-catalog-id".into()),
            title: "Old song".into(),
            artist: "Old artist".into(),
            album: "Old album".into(),
            duration_ms: 180_000,
            track_number: 1,
            artwork: None,
        });
        model.player.state = PlaybackState::Playing;
        model.player.now_playing = Some(item.clone());
        model.player.queue = vec![item];
        model.player.queue_position = 4;
        model.player.position_disagrees = true;
        model.player.position_ms = 42_000;
        model.player.duration_ms = 180_000;
        model.player.shuffle = true;
        model.player.repeat = RepeatMode::All;
        model.art_path = Some("/tmp/old-art".into());
        drop(model);

        daemon
            .last_queue
            .replace(Some((vec!["old-catalog-id".into()], None)));
        daemon.pending_start.replace(Some("old-catalog-id".into()));
        daemon.healed.set(true);
        daemon.resume_at.set(Some(42_000));
        daemon.restore_start.set(Some(4));
        daemon.restart_at.replace(Some("old:1".into()));
        daemon.after_apply.set(Some("play".into()));
        daemon.art_for.replace(Some("old-art-template".into()));
    }

    fn assert_account_state_is_empty(daemon: &Daemon) {
        let model = daemon.model.borrow();
        assert_eq!(model.stage, Stage::SignedOut);
        assert!(model.tokens.is_none());
        assert!(model.library.tracks.is_empty());
        assert!(model.player.queue.is_empty());
        assert!(model.player.now_playing.is_none());
        assert_eq!(model.player.state, PlaybackState::None);
        assert_eq!(model.player.queue_position, 0);
        assert!(!model.player.position_disagrees);
        assert_eq!(model.player.position_ms, 0);
        assert_eq!(model.player.duration_ms, 0);
        assert!(!model.player.shuffle);
        assert_eq!(model.player.repeat, RepeatMode::None);
        assert!(model.art_path.is_none());
        assert_eq!(model.volume, 0.75);
        assert!(model.dead_ids.contains("globally-dead"));
        drop(model);

        assert!(daemon.last_queue.borrow().is_none());
        assert!(daemon.pending_start.borrow().is_none());
        assert!(!daemon.healed.get());
        assert!(daemon.resume_at.get().is_none());
        assert!(daemon.restore_start.get().is_none());
        assert!(daemon.restart_at.borrow().is_none());
        assert!(daemon.after_apply.take().is_none());
        assert!(daemon.art_for.borrow().is_none());
    }

    fn assert_signed_out_events(events: &mut broadcast::Receiver<Event>) {
        assert!(matches!(
            events.try_recv(),
            Ok(Event::Stage(Stage::SignedOut))
        ));
        assert!(matches!(
            events.try_recv(),
            Ok(Event::Queue { items, position }) if items.is_empty() && position == 0
        ));
        assert!(matches!(
            events.try_recv(),
            Ok(Event::Snapshot(snapshot))
                if snapshot.track_id.is_none()
                    && snapshot.title.is_empty()
                    && !snapshot.playing
        ));
        assert!(matches!(events.try_recv(), Ok(Event::LibraryChanged)));
    }

    #[test]
    fn sign_out_clears_account_state_and_notifies_clients() {
        let daemon = daemon();
        let mut events = daemon.events.subscribe();
        populate_account_state(&daemon);

        on_event(&daemon, PlayerEvent::Authorization { authorized: false });

        assert_account_state_is_empty(&daemon);
        assert_signed_out_events(&mut events);

        on_event(&daemon, PlayerEvent::SignedOut);

        assert_account_state_is_empty(&daemon);
        assert_signed_out_events(&mut events);
    }

    #[test]
    fn sign_out_confirmation_is_a_cleanup_backstop() {
        let daemon = daemon();
        let mut events = daemon.events.subscribe();
        populate_account_state(&daemon);

        on_event(&daemon, PlayerEvent::SignedOut);

        assert_account_state_is_empty(&daemon);
        assert_signed_out_events(&mut events);
    }

    #[test]
    fn sign_out_removes_account_persistence() {
        const CHILD: &str = "SLIPMAT_SIGN_OUT_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let daemon = daemon();
            populate_account_state(&daemon);
            let model = daemon.model.borrow();
            slipmat_core::library_cache::save(&model.library.tracks, &[], &[], &[]);
            drop(model);
            slipmat_core::session::save(&slipmat_core::session::Session {
                songs: vec!["old-catalog-id".into()],
                start: 0,
                position_ms: 42_000,
            });
            let library = slipmat_core::paths::cache_dir()
                .expect("test cache directory")
                .join("library.json");
            let session = slipmat_core::paths::state_dir()
                .expect("test state directory")
                .join("session.json");
            assert!(library.exists());
            assert!(session.exists());

            on_event(&daemon, PlayerEvent::SignedOut);

            assert!(!library.exists());
            assert!(!session.exists());
            return;
        }

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("slipmat-sign-out-{}-{unique}", std::process::id()));
        let status = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "serve::tests::sign_out_removes_account_persistence",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("HOME", root.join("home"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("XDG_STATE_HOME", root.join("state"))
            .status()
            .expect("run isolated sign-out test");
        let _ = std::fs::remove_dir_all(root);
        assert!(status.success());
    }

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

    #[test]
    fn selected_occurrence_is_restarted_only_after_it_arrives() {
        let daemon = daemon();
        let first = slipmat_core::player::protocol::Item {
            occurrence_id: "run:1".into(),
            ..Default::default()
        };
        let second = slipmat_core::player::protocol::Item {
            occurrence_id: "run:2".into(),
            ..Default::default()
        };
        daemon.model.borrow_mut().player.queue = vec![first.clone(), second.clone()];
        daemon.restart_at.replace(Some("run:2".into()));
        daemon.model.borrow_mut().player.now_playing = Some(first);

        assert!(restart_selected_occurrence(&daemon).is_none());
        assert_eq!(daemon.restart_at.borrow().as_deref(), Some("run:2"));

        daemon.model.borrow_mut().player.now_playing = Some(second);
        assert!(matches!(
            restart_selected_occurrence(&daemon),
            Some(Command::Seek { position_ms: 0 })
        ));
        assert!(daemon.restart_at.borrow().is_none());
    }

    #[test]
    fn a_deferred_position_is_consumed_once() {
        let daemon = daemon();
        daemon.resume_at.set(Some(55_000));

        assert!(matches!(
            heal::resume_position(&daemon),
            Some(Command::Seek {
                position_ms: 55_000
            })
        ));
        assert!(heal::resume_position(&daemon).is_none());
        assert_eq!(daemon.model.borrow().player.position_ms, 55_000);
    }

    #[test]
    fn play_keeps_the_restored_seek_but_navigation_cancels_it() {
        let daemon = daemon();
        daemon.model.borrow_mut().player.queue = vec![Item::default()];
        daemon.resume_at.set(Some(55_000));
        daemon.restore_start.set(Some(0));

        route_transport(&daemon, Transport::Play);

        assert_eq!(daemon.resume_at.get(), Some(55_000));
        assert_eq!(daemon.restore_start.get(), Some(0));

        route_transport(&daemon, Transport::Next);

        assert!(daemon.resume_at.get().is_none());
        assert!(daemon.restore_start.get().is_none());
    }

    #[test]
    fn a_selected_queue_item_can_start_an_artwork_fetch() {
        let daemon = daemon();
        let template = "https://example.test/restored/{w}x{h}.{f}";
        daemon.model.borrow_mut().player.queue = vec![Item {
            artwork_template: Some(template.into()),
            ..Default::default()
        }];
        let local = tokio::task::LocalSet::new();
        let _entered = local.enter();

        fetch_artwork(&daemon);

        assert_eq!(daemon.art_for.borrow().as_deref(), Some(template));
        assert!(daemon.model.borrow().art_path.is_none());
    }

    #[test]
    fn a_restored_session_keeps_its_complete_paused_projection() {
        const CHILD: &str = "SLIPMAT_RESTORE_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let restored = daemon();
            let session = slipmat_core::session::Session {
                songs: vec!["song-a".into(), "song-b".into(), "song-a".into()],
                start: 2,
                position_ms: 55_000,
            };
            slipmat_core::session::save(&session);

            let template = "https://example.test/{w}x{h}.{f}";
            let art = Artwork::new(template);
            let art_path = slipmat_core::artwork::cache_path(&art, slipmat_core::artwork::ART_SIZE)
                .expect("artwork cache path");
            std::fs::create_dir_all(art_path.parent().expect("artwork directory"))
                .expect("create artwork directory");
            std::fs::write(&art_path, b"cached artwork").expect("cache artwork");

            restore_session(&restored);

            assert_eq!(restored.model.borrow().player.queue_position, 2);
            assert_eq!(restored.model.borrow().player.position_ms, 55_000);
            assert_eq!(restored.resume_at.get(), Some(55_000));

            let stale = Item {
                occurrence_id: "old:1".into(),
                catalog_id: Some("old-song".into()),
                title: "Old song".into(),
                ..Default::default()
            };
            on_event(
                &restored,
                PlayerEvent::NowPlaying {
                    item: Some(stale.clone()),
                    queue: Queue {
                        position: 0,
                        items: vec![stale.clone()],
                        ..Default::default()
                    },
                },
            );
            assert!(restored.model.borrow().player.queue.is_empty());
            assert_eq!(restored.resume_at.get(), Some(55_000));
            let saved = slipmat_core::session::load().expect("original session");
            assert_eq!(saved.songs, session.songs);
            assert_eq!(saved.start, session.start);
            assert_eq!(saved.position_ms, session.position_ms);

            let items = vec![
                Item {
                    occurrence_id: "run:1".into(),
                    catalog_id: Some("song-a".into()),
                    title: "Song A".into(),
                    duration_ms: 180_000,
                    artwork_template: Some(template.into()),
                    ..Default::default()
                },
                Item {
                    occurrence_id: "run:2".into(),
                    catalog_id: Some("song-b".into()),
                    title: "Song B".into(),
                    duration_ms: 200_000,
                    ..Default::default()
                },
                Item {
                    occurrence_id: "run:3".into(),
                    catalog_id: Some("song-a".into()),
                    title: "Song A".into(),
                    duration_ms: 180_000,
                    artwork_template: Some(template.into()),
                    ..Default::default()
                },
            ];
            let queue = Queue {
                position: -1,
                items: items.clone(),
                ..Default::default()
            };
            on_event(&restored, PlayerEvent::Queue(queue.clone()));
            on_event(
                &restored,
                PlayerEvent::NowPlaying {
                    item: Some(stale.clone()),
                    queue: Queue {
                        position: 0,
                        items: vec![stale],
                        ..Default::default()
                    },
                },
            );

            let snapshot = restored.model.borrow().snapshot();
            assert_eq!(restored.model.borrow().player.queue_position, 2);
            assert_eq!(restored.restore_start.get(), Some(2));
            assert_eq!(snapshot.title, "Song A");
            assert_eq!(snapshot.position_ms, 55_000);
            assert_eq!(snapshot.duration_ms, 180_000);
            assert_eq!(
                snapshot.art_path.as_deref(),
                Some(art_path.to_str().unwrap())
            );
            assert!(!snapshot.playing);
            let saved = slipmat_core::session::load().expect("saved restored session");
            assert_eq!(saved.songs, session.songs);
            assert_eq!(saved.start, session.start);
            assert_eq!(saved.position_ms, session.position_ms);

            on_event(
                &restored,
                PlayerEvent::NowPlaying {
                    item: Some(items[2].clone()),
                    queue: Queue {
                        position: 2,
                        items,
                        ..Default::default()
                    },
                },
            );

            assert_eq!(restored.model.borrow().player.position_ms, 55_000);
            assert!(restored.resume_at.get().is_none());
            assert!(restored.restore_start.get().is_none());
            let saved = slipmat_core::session::load().expect("saved resumed session");
            assert_eq!(saved.songs, session.songs);
            assert_eq!(saved.start, session.start);
            assert_eq!(saved.position_ms, session.position_ms);
            assert!(!restored.model.borrow().snapshot().playing);

            slipmat_core::session::save(&slipmat_core::session::Session {
                songs: vec!["song-a".into()],
                start: 0,
                position_ms: 0,
            });
            let zero = daemon();
            restore_session(&zero);
            assert!(zero.resume_at.get().is_none());
            return;
        }

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("slipmat-restore-{}-{unique}", std::process::id()));
        let status = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "serve::tests::a_restored_session_keeps_its_complete_paused_projection",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("HOME", root.join("home"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("XDG_STATE_HOME", root.join("state"))
            .status()
            .expect("run isolated restoration test");
        let _ = std::fs::remove_dir_all(root);
        assert!(status.success());
    }
}
