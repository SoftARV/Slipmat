// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Putting playback back when MusicKit does not do what it was asked.
//!
//! Three separate failures, each found the hard way in the GTK client and each
//! costing a silent dead player if it is missed:
//!
//! * a `play` that completes and produces no audio,
//! * a queue that opens on a track other than the one that was named,
//! * a position lost to the reload that fixed the first two.

use slipmat_core::player::protocol::{Command, Item};

use crate::serve::Daemon;

/// A `play` that completed and produced no playing.
///
/// The sidecar reports the state it ended in, so this is the difference between
/// a command that failed — which would have errored — and one that worked on
/// something that cannot make sound. Only the second is worth recovering from,
/// and only once: a second attempt that also does nothing is a real failure and
/// should look like one rather than looping.
pub fn play_did_nothing(daemon: &Daemon, cmd: &str) {
    if !matches!(cmd, "play" | "playPause") {
        return;
    }
    // **Still working towards audio is not failure.** `Loading`, `Waiting` and
    // `Stalled` are ordinary a fraction of a second after a play, and judging
    // them cost a real playback in the GTK client: a heal fired mid-load, which
    // stopped the track, which produced another non-playing `play`.
    let (busy, playing) = {
        let model = daemon.model.borrow();
        (
            model.player.state.is_busy(),
            model.player.state.is_playing(),
        )
    };
    if busy {
        return;
    }
    if playing {
        daemon.healed.set(false);
        return;
    }
    if daemon.healed.get() {
        tracing::warn!("play still produced no playback after reloading the track");
        return;
    }
    daemon.healed.set(true);
    reseat_current(daemon, "play produced no playback");
}

/// Reload the track MusicKit is already on, and remember where we were in it.
fn reseat_current(daemon: &Daemon, why: &str) {
    let (index, len, position_ms) = {
        let model = daemon.model.borrow();
        (
            model.player.queue_position,
            model.player.queue.len(),
            model.player.interpolated_position_ms(),
        )
    };
    if len == 0 || index >= len {
        return;
    }
    tracing::info!(why, index, position_ms, "reloading the current track");
    daemon.send(Command::ChangeToIndex { index });
    // **Not sent yet.** A seek needs a current item to seek within, and the
    // reload has not produced one. `nowPlayingItemDidChange` is when there is
    // something to seek in; `resume_position` sends it then.
    daemon
        .resume_at
        .set((position_ms > 0).then_some(position_ms));
}

/// Put the position back once the reloaded track is actually current.
pub fn resume_position(daemon: &Daemon) {
    let Some(position_ms) = daemon.resume_at.take() else {
        return;
    };
    tracing::info!(position_ms, "restoring the position after a reload");
    daemon.send(Command::Seek { position_ms });
}

/// Confirm MusicKit opened on the track that was named, and correct it if not.
///
/// Only for queues built in our own order — a shuffled one has no wrong track
/// to correct to, because MusicKit reorders as it loads (#152).
pub fn verify_start(daemon: &Daemon) {
    let Some(wanted) = daemon.pending_start.borrow().clone() else {
        return;
    };

    let model = daemon.model.borrow();
    if model.player.queue.is_empty() {
        return; // the queue has not arrived yet; try again on the next event
    }

    // **Wait for the queue we actually sent.** The mirror holds the previous
    // one for a few milliseconds after `setQueue`, and playing the same list
    // twice means both have the same ids — so "is a queue loaded" cannot tell
    // them apart. Compared sorted, because with shuffle on the order is
    // deliberately not ours.
    if let Some((sent, _)) = daemon.last_queue.borrow().as_ref() {
        let mut theirs: Vec<String> = model.player.queue.iter().filter_map(id_of).collect();
        let mut ours = sent.clone();
        theirs.sort_unstable();
        ours.sort_unstable();
        if theirs != ours {
            return;
        }
    }

    // The queue arrives before the item does, and in that gap `queue_position`
    // names no track. Correcting there starts a load over the one `setQueue` is
    // still running and rejects its play.
    if model.player.now_playing.is_none() || model.player.state.is_busy() {
        return;
    }

    // One shot either way: acting or giving up both clear the flag, so a
    // correction can never bounce against MusicKit's own echo.
    drop(model);
    daemon.pending_start.replace(None);

    let model = daemon.model.borrow();
    let Some(index) = model
        .player
        .queue
        .iter()
        .position(|item| id_of(item).as_deref() == Some(wanted.as_str()))
    else {
        tracing::debug!(%wanted, "chosen track is not in MusicKit's queue");
        return;
    };
    if index == model.player.queue_position {
        return; // already right
    }
    let from = model.player.queue_position;
    drop(model);

    tracing::info!(
        from,
        to = index,
        "MusicKit started the wrong track; correcting"
    );
    daemon.send(Command::ChangeToIndex { index });
}

fn id_of(item: &Item) -> Option<String> {
    item.catalog_id.clone().or_else(|| item.id.clone())
}
