// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Turning a click into a queue, and keeping ours honest against MusicKit's.
//!
//! Rule 3 lives here: MusicKit owns the queue and Rust mirrors it, so a click
//! sends the **whole** visible list in one `setQueue` and names its starting
//! track **by id**. Nearly every function below exists because some earlier
//! version carried an *index* across a filter, a network round trip or a user
//! action — and started the wrong song.

use super::AppModel;
use crate::components::track_row::apply_row_state;
use slipmat_core::entry::Entry;
use slipmat_core::player::protocol::Command;
use slipmat_core::queue::{
    Start, holds, index_at, playable_rows, queue_from, start_index, unresolvable_ids,
};

impl AppModel {
    /// Remember the queue and where we are in it.
    ///
    /// Called on every track change *and* on shutdown, deliberately. Shutdown
    /// is the only moment the position is accurate, but it is also the one that
    /// might not run — a crash, a SIGKILL, a session ending badly. Saving on
    /// each track change means the worst case is restoring the right track at
    /// its start rather than restoring nothing at all.
    pub(super) fn save_session(&self) {
        let songs: Vec<String> = self
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
            start: self
                .player
                .queue_position
                .min(songs.len().saturating_sub(1)),
            position_ms: self.player.position_ms,
            songs,
        });
    }

    /// Put back what was playing when the app last closed.
    ///
    /// Loaded **paused**, and the position is applied only once MusicKit
    /// confirms it is holding the queue we asked for.
    pub(super) fn restore_session(&mut self) {
        let Some(session) = slipmat_core::session::load() else {
            return;
        };
        let start = session.start.min(session.songs.len() - 1);
        let wanted = session.songs.get(start).cloned();

        tracing::info!(
            tracks = session.songs.len(),
            start,
            position_ms = session.position_ms,
            "restoring the last session"
        );

        self.pending_start = wanted.clone();
        self.last_queue = Some((session.songs.clone(), wanted));
        // The saved order *is* what was playing, and `start` indexes into it.
        // Reshuffling would land the position on a different track.
        self.send(Command::SetShuffle { shuffle: false });
        self.send(Command::SetQueue {
            songs: session.songs,
            start_position: start,
            // Loaded, not started.
            start_playing: false,
            // Carried in the descriptor rather than seeked afterwards: a seek
            // needs a current item to seek *within*, and a queue loaded without
            // playing does not have one.
            start_time_ms: session.position_ms,
        });
    }

    /// The catalog id of the track MusicKit is on, if any.
    pub(super) fn playing_catalog_id(&self) -> Option<String> {
        self.player
            .now_playing
            .as_ref()
            .and_then(|i| i.catalog_id.clone().or_else(|| i.id.clone()))
    }

    /// Which row a shuffled queue opens on (#147).
    ///
    /// The entry point is the random part; the order stays MusicKit's.
    /// Shuffling the ids here would work once and leave the player sequential.
    pub(super) fn shuffle_start(&self, entries: &[Entry]) -> usize {
        let rows = playable_rows(entries, &self.dead_ids);
        if rows.is_empty() {
            // Nothing here can be streamed. `play_entries` says so a moment
            // later; any row will do until it does.
            return 0;
        }
        let pick = relm4::gtk::glib::random_int_range(0, rows.len() as i32) as usize;
        rows[pick]
    }

    /// Enqueue a list and start at `row`, per rule 3: the whole thing goes to
    /// MusicKit in one `setQueue`, and the starting track is named by id.
    ///
    /// Shared by the results list and every pushed page — one enqueue path, so
    /// a fix to it cannot land on one list and miss the other. `start` is what
    /// makes that true of the shuffle mode as well: see [`Start`].
    pub(super) fn play_entries(&mut self, entries: &[Entry], row: usize, start: Start) {
        let (songs, start_id) = queue_from(entries, row, &self.dead_ids);
        if songs.is_empty() {
            self.toast("Nothing here can be streamed");
            return;
        }

        // Rule 3: clicking a track in a list MusicKit already holds is a move
        // within that queue, not a reason to rebuild it and lose the gapless
        // buffer.
        if let Some(wanted) = &start_id
            && holds(&self.player.queue, &songs)
            && let Some(index) = self.queue_index_of(wanted)
        {
            tracing::info!(index, "already loaded; moving within the queue");
            // A button naming a mode still names it on the same queue; a row
            // click does not.
            if let Some(shuffle) = start.mode(false) {
                self.send(Command::SetShuffle { shuffle });
            }
            // Nothing pending: there is no new queue to verify against.
            self.pending_start = None;
            self.send(Command::ChangeToIndex { index });
            return;
        }

        // Before the queue, so MusicKit's shuffle applies as it loads — and
        // unconditionally, so the mode is never inherited.
        if let Some(shuffle) = start.mode(true) {
            self.send(Command::SetShuffle { shuffle });
        }

        // Not `start`: that is the mode. This is where in the list it begins.
        let position = start_index(&songs, start_id.as_ref());
        tracing::info!(queue = songs.len(), position, "enqueuing");
        // Nothing to verify when the order is not ours: MusicKit reorders as it
        // loads, so no track is the *wrong* one to open on.
        self.pending_start = if start.reorders() {
            None
        } else {
            start_id.clone()
        };
        self.last_queue = Some((songs.clone(), start_id));
        self.send(Command::SetQueue {
            songs,
            start_position: position,
            start_playing: true,
            start_time_ms: 0,
        });
    }

    /// Handle MusicKit's all-or-nothing `NOT_FOUND` by dropping the ids it
    /// named and trying again.
    ///
    /// Returns true when it took ownership of the error, so the caller doesn't
    /// also toast a message the user can do nothing about.
    pub(super) fn retry_without_dead_tracks(&mut self, detail: &str) -> bool {
        let dead = unresolvable_ids(detail);
        if dead.is_empty() {
            return false;
        }
        let Some((songs, wanted)) = self.last_queue.take() else {
            return false;
        };

        let newly_dead = dead
            .iter()
            .filter(|id| !self.dead_ids.contains(*id))
            .count();
        self.dead_ids.extend(dead);
        if newly_dead > 0 {
            // Remember them, so the next run starts already knowing.
            slipmat_core::unplayable::save(&self.dead_ids);
        }

        // Nothing new: the retry already happened and failed again. Stop, or we
        // loop forever on an error we cannot parse our way out of.
        if newly_dead == 0 {
            tracing::warn!("queue still unresolvable after dropping known-dead ids");
            return false;
        }

        // If the track we were aiming at is itself newly dead, aim at the next
        // surviving track *after* it in the original order — not at the top of
        // the list, which is where falling back to index 0 would land.
        let from = songs
            .iter()
            .position(|s| Some(s) == wanted.as_ref())
            .unwrap_or(0);
        let wanted = songs[from..]
            .iter()
            .find(|id| !self.dead_ids.contains(*id))
            .cloned();

        let retry: Vec<String> = songs
            .into_iter()
            .filter(|id| !self.dead_ids.contains(id))
            .collect();

        if retry.is_empty() {
            self.toast("None of these tracks are available to stream");
            return true;
        }

        let start = start_index(&retry, wanted.as_ref());
        tracing::info!(
            dropped = newly_dead,
            queue = retry.len(),
            start,
            "retrying queue without unresolvable tracks"
        );
        self.mark_dead_tracks_unplayable();
        self.pending_start = wanted.clone();
        self.last_queue = Some((retry.clone(), wanted));
        self.send(Command::SetQueue {
            songs: retry,
            start_position: start,
            start_playing: true,
            start_time_ms: 0,
        });
        true
    }

    /// Reflect newly-refused tracks in the list **without rebuilding it**.
    ///
    /// This fires on the first play of a session — exactly when the user is
    /// looking at the row they just clicked — so a rebuild here is what sent
    /// the library back to the top, once per run. Rows consult the shared set
    /// at bind, so updating it covers everything off screen; the rows that are
    /// on screen are repainted directly.
    ///
    /// `all_tracks` keeps its catalog ids: playability is now a question for
    /// `dead_rows`, and blanking the id would also lose the handle the queue
    /// builder needs.
    pub(super) fn mark_dead_tracks_unplayable(&mut self) {
        *self.dead_rows.borrow_mut() = self.dead_ids.clone();

        let playing = self.playing_catalog_id();
        let registry = self.library_icons.borrow();
        for id in &self.dead_ids {
            if let Some(w) = registry.get(id) {
                apply_row_state(&w.icon, &w.root, Some(id) == playing.as_ref(), false);
            }
        }
    }

    /// Check that MusicKit actually landed on the track we asked for, and
    /// correct it if not.
    ///
    /// `setQueue` takes a *position*, but the queue MusicKit builds is not
    /// always the list we handed it — it drops repeats and anything it decides
    /// it cannot use, and every position after such an item then refers to a
    /// different track. Observed directly: 531 ids sent, `queue_len=530` back,
    /// and playback one track further down than the row that was clicked.
    ///
    /// No amount of arithmetic on our side can fix that, because the
    /// discrepancy happens inside MusicKit. So we check its own queue for the
    /// id we wanted and jump if we are not on it — `changeToMediaAtIndex`, not
    /// a second `setQueue`, so the queue is not rebuilt and gapless survives.
    pub(super) fn verify_start(&mut self) {
        let Some(wanted) = self.pending_start.clone() else {
            return;
        };
        if self.player.queue.is_empty() {
            return; // queue hasn't arrived yet; try again on the next event
        }

        let id_of = |item: &slipmat_core::player::protocol::Item| {
            item.catalog_id.clone().or_else(|| item.id.clone())
        };

        // **Wait for the queue we actually sent.** The mirror still holds the
        // previous one for a few milliseconds after `setQueue`, and playing the
        // same playlist twice means both have the same length and the same
        // ids — so "is a queue loaded" is not enough to tell them apart. An
        // earlier version corrected 3ms after sending, against the old queue,
        // and jumped to whatever sat at that index. Compare *sorted* ids: with
        // shuffle on, MusicKit's order is deliberately not ours.
        if let Some((sent, _)) = &self.last_queue {
            let mut theirs: Vec<String> = self.player.queue.iter().filter_map(id_of).collect();
            let mut ours = sent.clone();
            theirs.sort_unstable();
            ours.sort_unstable();
            if theirs != ours {
                return; // not our queue yet
            }
        }

        // The queue arrives before the item does, and in that gap
        // `queue_position` names no track. Correcting there starts a load over
        // the one `setQueue` is still running and rejects its play. Returning
        // keeps `pending_start`, so the item's arrival retries this.
        if self.player.now_playing.is_none() || self.player.state.is_busy() {
            return;
        }

        // One shot either way: acting or giving up both clear the flag, so a
        // correction can never bounce against MusicKit's own echo.
        self.pending_start = None;
        let Some(index) = self
            .player
            .queue
            .iter()
            .position(|item| id_of(item).as_deref() == Some(wanted.as_str()))
        else {
            tracing::debug!(%wanted, "chosen track is not in MusicKit's queue");
            return;
        };

        if index == self.player.queue_position {
            return; // already right
        }
        tracing::info!(
            from = self.player.queue_position,
            to = index,
            "MusicKit started the wrong track; correcting"
        );
        self.send(Command::ChangeToIndex { index });
    }

    /// Where a track sits in MusicKit's queue *right now*.
    ///
    /// Resolved at send time rather than carried from the row, because our row
    /// order and MusicKit's queue can drift — and a stale position does not
    /// fail loudly, it removes or plays the wrong track, or gets rejected with
    /// INVALID_ARGUMENTS once it runs off the end.
    /// Where a clicked row is in MusicKit's queue, from where it *was* and what
    /// it was. See [`index_at`].
    pub(super) fn queue_index_at(&self, at: usize, id: &str) -> Option<usize> {
        index_at(&self.player.queue, at, id)
    }

    pub(super) fn queue_index_of(&self, id: &str) -> Option<usize> {
        index_at(&self.player.queue, usize::MAX, id)
    }
}
