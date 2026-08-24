// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The socket, the sidecar, and the loop that keeps them agreeing.

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::{Context, Result};
use slipmat_core::entry::Entry;
use slipmat_core::ipc::{self, Event, PageKind, Request, Stage, Transport};
use slipmat_core::music::client::Client;
use slipmat_core::player::protocol::{Command, Event as PlayerEvent};
use slipmat_core::player::{Incoming, sidecar};
use slipmat_core::queue::{Start, queue_from_ids, start_index, unresolvable_ids};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;

use crate::state::Model;

/// How many events a slow client may fall behind before it is dropped.
///
/// A client that cannot keep up with a 500ms tick is not going to catch up, and
/// holding the backlog for it would make every other client's memory its
/// problem. `broadcast` tells it how far it lagged; it can ask for a snapshot.
const BACKLOG: usize = 64;

/// Position ticks while playing. The same cadence the GTK client uses.
const TICK_MS: u64 = 500;

pub struct Daemon {
    pub model: RefCell<Model>,
    /// Replaced on every respawn, so everything holding an `Rc<Daemon>` keeps
    /// talking to whichever sidecar is alive now.
    pub sidecar: RefCell<sidecar::Handle>,
    pub events: broadcast::Sender<Event>,
    /// Whether the session has been put back since the current sidecar started.
    pub restored: std::cell::Cell<bool>,
    /// Consecutive failed starts, for the backoff. Reset once MusicKit attaches.
    pub restarts: std::cell::Cell<u32>,
    /// The last queue we asked for, and the track we aimed at. Kept for the
    /// dead-track retry, which has to rebuild it without the ids Apple refused.
    pub last_queue: RefCell<Option<(Vec<String>, Option<String>)>>,
}

impl Daemon {
    fn send(&self, cmd: Command) {
        self.sidecar.borrow().send(cmd);
    }

    fn publish(&self, event: Event) {
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
        sidecar: RefCell::new(handle),
        events,
        restored: std::cell::Cell::new(false),
        restarts: std::cell::Cell::new(0),
        last_queue: RefCell::new(None),
    });

    // Accept loop: one task per client, each holding a handle to the daemon.
    let accepting = daemon.clone();
    tokio::task::spawn_local(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let daemon = accepting.clone();
                    tokio::task::spawn_local(async move {
                        if let Err(err) = client(stream, daemon).await {
                            tracing::debug!(?err, "client gone");
                        }
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
        loop {
            tick.tick().await;
            if ticking.model.borrow().player.state.is_playing() {
                ticking.publish_snapshot();
            }
        }
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

        // Written from the live mirror, not from what was on disk: a crash
        // mid-track should come back to that track, not to last night's.
        save_session(&daemon);

        let attempt = daemon.restarts.get();
        daemon.restarts.set(attempt + 1);
        let delay = sidecar::restart_delay(attempt);
        tracing::warn!(attempt = attempt + 1, ?delay, "restarting the sidecar");
        daemon.publish(Event::Stage(Stage::Connecting));
        tokio::time::sleep(delay).await;

        match sidecar::spawn() {
            Ok((handle, rx)) => {
                *daemon.sidecar.borrow_mut() = handle;
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

fn on_event(daemon: &Daemon, event: PlayerEvent) {
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
        daemon.publish(Event::Queue { items, position });
        // On every track change, because shutdown is the moment that might not
        // run — a SIGKILL, a session ending badly.
        save_session(daemon);
    }
    daemon.publish_snapshot();
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
        Request::Queue => {
            let (items, position) = daemon.model.borrow().queue();
            Some(Event::Queue { items, position })
        }
        Request::JumpTo { index } => {
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
        Request::Open { kind, id } => {
            // Answered off this task: opening a page is a network round trip,
            // and a client waiting on one must not stop the daemon answering
            // everyone else.
            open_page(daemon, kind, id);
            None
        }
        Request::Play { ids, index, start } => {
            play(daemon, &ids, index, start.into());
            None
        }
        Request::Transport(transport) => {
            let command = command_for(transport);
            tracing::debug!(cmd = command.name(), "transport");
            daemon.send(command);
            if let Transport::SetVolume { volume } = transport {
                // MusicKit does not report volume back, so this is the only
                // record of it — the same reason the GTK client keeps its own.
                daemon.model.borrow_mut().volume = volume;
            }
            None
        }
    }
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
        let fetched = match kind {
            PageKind::Album => client
                .library_album(&id)
                .await
                .map(|(album, tracks)| (album.name, tracks)),
            PageKind::Playlist => client
                .library_playlist(&id)
                .await
                .map(|(list, tracks)| (list.name, tracks)),
            // An artist page is their albums, not their tracks — the same
            // shape the GTK client shows.
            PageKind::Artist => client
                .album(&id)
                .await
                .map(|(album, tracks)| (album.name, tracks)),
        };

        match fetched {
            Ok((title, tracks)) => daemon.publish(Event::Page {
                kind,
                id,
                title,
                entries: tracks.into_iter().map(Entry::Song).collect(),
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

fn command_for(transport: Transport) -> Command {
    match transport {
        Transport::Play => Command::Play,
        Transport::Pause => Command::Pause,
        Transport::PlayPause => Command::PlayPause,
        Transport::Next => Command::Next,
        Transport::Previous => Command::Previous,
        Transport::Seek { position_ms } => Command::Seek { position_ms },
        Transport::SetVolume { volume } => Command::SetVolume { volume },
        Transport::SetShuffle { shuffle } => Command::SetShuffle { shuffle },
        Transport::SetRepeat { mode } => Command::SetRepeat { mode },
    }
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
            // Panics on an unmapped verb; the point is that it compiles
            // exhaustively and runs without one.
            let _ = command_for(verb);
        }
    }
}
