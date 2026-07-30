// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! What a library write does to the rows on screen.
//!
//! Adding, removing, favouriting and un-favouriting are all **optimistic**: the
//! row changes the moment the command goes out, because a menu that waits on a
//! round trip reads as broken. That is only honest if the change can be taken
//! back, so the settle path here is as much a part of the feature as the send.
//!
//! The reason these live together is that they change together. A track's
//! favourite flag is held in four places — `all_tracks`, the list store's own
//! clone, the row's `RowFacts`, and the star widget — and every bug in this
//! area has been one of them left behind (see issue #47, which proposes
//! collapsing them onto a shared cell as `CurrentTrack` already is).

use super::{AppModel, SearchScope, WriteUndo};
use crate::components::TrackOverride;

impl AppModel {
    /// Drop a track the library no longer holds, without rebuilding the list.
    ///
    /// Called only once the sidecar confirms, so a refused removal never takes
    /// a row off screen. `TypedListView::remove` keeps the scroll position —
    /// a rebuild here would throw the reader back to the top, which is the same
    /// complaint pagination had.
    pub(super) fn drop_removed_track(&mut self, catalog_id: &str) {
        // **Index first.** `visible_entries` is derived from `all_tracks`, so
        // asking it where the row is *after* the retain always answers `None`
        // — the model has already forgotten it. That left the row on screen
        // until a manual refresh, which is exactly what this function exists to
        // avoid.
        //
        // Only the Songs list mirrors `all_tracks`; a catalog search showing
        // the same song is still a valid result and keeps its row, so it is
        // only asked in library scope.
        let row = (self.scope() == SearchScope::Library)
            .then(|| {
                self.visible_entries()
                    .iter()
                    .position(|e| e.catalog_id() == Some(catalog_id))
            })
            .flatten();

        let before = self.all_tracks.len();
        self.all_tracks
            .retain(|t| t.catalog_id.as_deref() != Some(catalog_id));
        if self.all_tracks.len() == before {
            return; // not a library track — nothing to take off the list
        }

        if let Some(index) = row {
            // **Before the edit, not after it.** The row menu is parented to
            // the row, so closing it hands focus back there — and `GtkListView`
            // discards the scroll position when the focused row is the one
            // removed. See `components::drop_focus` for the measurement; this
            // replaced an anchor-and-restore that guessed the top row from the
            // mean row height and only ever corrected the jump after it showed.
            crate::components::drop_focus(&self.library.view);
            self.library.remove(index as u32);
            // The widgets it owned are gone with it.
            self.library_icons.borrow_mut().remove(catalog_id);
        }
        tracing::info!(
            catalog_id,
            removed_row = row.is_some(),
            "track left the library"
        );
    }

    /// Settle one library write against the id it was for.
    ///
    /// On success a removal is the moment the row may leave the list. On
    /// failure the optimistic change is put back and said out loud — without
    /// this the row keeps a change that never happened, which is not
    /// hypothetical: pointed at a sidecar too old to know the command, every
    /// removal answered `unknown-command`, the toast said it was happening, the
    /// row agreed, and the library on the user's phone did not move.
    pub(super) fn settle_library_write(&mut self, kind: &str, id: &str, ok: bool, detail: &str) {
        // Keyed by id, so two writes in flight cannot be confused for each
        // other — see `PendingWrite`.
        let Some(pending) = self.pending_writes.remove(id) else {
            tracing::debug!(kind, id, ok, "library write with no pending record");
            return;
        };

        if ok {
            if matches!(pending.undo, WriteUndo::InLibrary(_)) {
                self.drop_removed_track(&pending.catalog_id);
            }
            return;
        }

        tracing::warn!(kind, id, %detail, "library write refused; row put back");
        match pending.undo {
            WriteUndo::InLibrary(was) => {
                self.set_in_library(&pending.catalog_id, was);
                self.toast("Couldn't remove it from your library");
            }
            WriteUndo::Favorite(was) => {
                self.set_favorite(&pending.catalog_id, was);
                self.toast("Couldn't remove that favourite");
            }
        }
    }

    /// Record what has happened to a track, once.
    ///
    /// This used to be four assignments — `all_tracks`, the list store's clone,
    /// the row's `RowFacts`, and the star widget — and every bug in this area
    /// was one of them left behind. Now there is a single shared cell that rows
    /// consult at bind, so the only other thing to do is repaint what is
    /// already on screen; a row that is off screen will read the truth when it
    /// comes back.
    fn record(&mut self, catalog_id: &str, apply: impl Fn(&mut TrackOverride)) {
        apply(
            self.row_overrides
                .borrow_mut()
                .entry(catalog_id.to_owned())
                .or_default(),
        );
    }

    /// Record library membership, so the row menu stops offering "Add to
    /// Library" for something already saved — or starts offering it again for
    /// something just removed.
    ///
    /// No repaint: membership has no mark of its own on a row, unlike the star.
    /// It is read when the menu is built, and the menu is built from the facts
    /// a row published at bind — which now come from the shared cell.
    pub(super) fn set_in_library(&mut self, catalog_id: &str, in_library: bool) {
        self.record(catalog_id, |over| over.in_library = Some(in_library));
        self.repaint_row(catalog_id);
    }

    /// Record a favourite and repaint the star.
    pub(super) fn set_favorite(&mut self, catalog_id: &str, on: bool) {
        self.record(catalog_id, |over| over.favorite = Some(on));
        self.repaint_row(catalog_id);
    }

    /// Bring the rows currently on screen up to date.
    ///
    /// Only the visible ones, and only their widgets — everything else reads
    /// the shared cell when it next binds. Every list is asked, because the same
    /// song can be on a page and in the results behind it.
    fn repaint_row(&self, catalog_id: &str) {
        // The fetched values are only a fallback for fields nothing has been
        // recorded against, and the row's own copy is the best one to hand.
        let fetched = self
            .all_tracks
            .iter()
            .find(|t| t.catalog_id.as_deref() == Some(catalog_id));
        let (favorite, in_library) = crate::components::overridden(
            &self.row_overrides,
            Some(catalog_id),
            fetched.is_some_and(|t| t.favorite),
            fetched.is_some_and(|t| t.in_library),
        );
        let lists =
            std::iter::once(&self.library_icons).chain(self.pages.iter().map(|p| p.registry()));
        for registry in lists {
            if let Some(w) = registry.borrow().get(catalog_id) {
                w.refresh(favorite, in_library);
            }
        }
    }
}
