// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Turning a click into a request.
//!
//! **Most of what used to be here is gone**, and that is the point of the
//! split. Building the queue, healing one MusicKit refused, verifying it opened
//! on the right track, remembering dead ids, saving the session — all of it
//! belonged to whoever holds the sidecar, and none of it belonged twice. What
//! is left is the part that is genuinely this client's: which rows the person
//! is looking at, and which one they clicked.

use super::AppModel;
use slipmat_core::entry::Entry;
use slipmat_core::ipc::{PlayMode, Request};

impl AppModel {
    /// The id of the track the player is on, if any.
    ///
    /// From the snapshot rather than the queue, so it always agrees with the
    /// title beside it. Asking the queue while holding an older snapshot is how
    /// a notification came to announce the song that had just finished.
    pub(super) fn playing_catalog_id(&self) -> Option<String> {
        self.mirror.snap.track_id.clone()
    }

    /// Ask the daemon to play this list, starting at `row`.
    ///
    /// **The ids go, not an index into something remembered for us.** The rows
    /// are on screen here; sending them back is what keeps the daemon from
    /// having to mirror this window's scroll position — and what lets a second
    /// client be looking at something else entirely.
    ///
    /// Non-song rows keep their place as `None` so `row` still means what it
    /// meant on screen: an album heading between two tracks is a row a person
    /// counted past.
    pub(super) fn play_entries(&mut self, entries: &[Entry], row: usize, start: PlayMode) {
        let ids: Vec<String> = entries
            .iter()
            .filter_map(|e| e.catalog_id().map(str::to_owned))
            .collect();
        if ids.is_empty() {
            self.toast("Nothing here can be streamed");
            return;
        }
        // The index into the *sendable* list, which is what the daemon will
        // count in. Rows without an id drop out of both.
        let index = entries
            .iter()
            .take(row)
            .filter(|e| e.catalog_id().is_some())
            .count();

        tracing::info!(rows = ids.len(), index, ?start, "asking to play");
        self.ask(Request::Play { ids, index, start });
    }

    /// Where a clicked queue row is now. See [`crate::mirror::Mirror::index_at`].
    pub(super) fn queue_index_at(&self, at: usize, id: &str) -> Option<usize> {
        self.mirror.index_at(at, id)
    }
}
