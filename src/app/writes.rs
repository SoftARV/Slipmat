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

use relm4::gtk;
use relm4::gtk::prelude::*;

use super::{AppModel, Entry, SearchScope, WriteUndo};
use crate::music::types::Track;

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

        // The scrolled window behind the list, so the position can be put back
        // when GTK discards it. See the note further down.
        let probe = self
            .library
            .view
            .ancestor(gtk::ScrolledWindow::static_type())
            .and_then(|w| w.downcast::<gtk::ScrolledWindow>().ok())
            .map(|sw| sw.vadjustment());
        if let Some(index) = row {
            self.library.remove(index as u32);
            // The widgets it owned are gone with it.
            self.library_icons.borrow_mut().remove(catalog_id);

            // The same restore the queue uses — see `restore_scroll_after_edit`
            // for the measurements behind it (#6). Anchored on the row that was
            // at the top, derived from the pixel offset over the mean row
            // height; the rows here are uniform, so that lands exactly.
            if let Some(adj) = probe {
                let rows = self.library.len().max(1) as f64;
                let row_height = adj.upper() / rows;
                if row_height > 0.0 {
                    let top_item = (adj.value() / row_height).floor() as u32;
                    crate::components::restore_scroll_after_edit(
                        &self.library.view,
                        &adj,
                        top_item,
                    );
                }
            }
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

    /// Correct the copy of a track held **inside the list store**.
    ///
    /// `TypedListView` items own a clone of the entry, taken when the rows were
    /// built, and `RelmListItem::bind` reads that clone every time a recycled
    /// widget is reused. So a change applied only to `all_tracks` and to the
    /// widget on screen is undone the moment the row scrolls out and back.
    ///
    /// Linear, because the store is not indexed by id and a library is a few
    /// hundred rows — the scan costs less than keeping a second index honest.
    pub(super) fn patch_stored_row(&mut self, catalog_id: &str, patch: impl Fn(&mut Track)) {
        for index in 0..self.library.len() {
            let Some(item) = self.library.get(index) else {
                continue;
            };
            let mut item = item.borrow_mut();
            if let Entry::Song(track) = &mut item.entry
                && track.catalog_id.as_deref() == Some(catalog_id)
            {
                patch(track);
                break;
            }
        }
    }

    /// Record library membership locally, so the row menu stops offering "Add
    /// to Library" for something just saved — or starts offering it again for
    /// something just removed.
    ///
    /// No repaint: membership has no mark of its own on a row, unlike the
    /// favourite star. It is read at menu-build time, which is why updating the
    /// model is enough.
    pub(super) fn set_in_library(&mut self, catalog_id: &str, in_library: bool) {
        // As in `set_favorite`: the stored clone is what a rebind reads.
        self.patch_stored_row(catalog_id, |track| track.in_library = in_library);
        for track in &mut self.all_tracks {
            if track.catalog_id.as_deref() == Some(catalog_id) {
                track.in_library = in_library;
            }
        }
        for entry in &mut self.catalog {
            if let Entry::Song(track) = entry
                && track.catalog_id.as_deref() == Some(catalog_id)
            {
                track.in_library = in_library;
            }
        }
        for page in &mut self.pages {
            page.set_in_library(catalog_id, in_library);
        }
        // And the live rows, so the menu stops offering a removal that has
        // already happened.
        let lists =
            std::iter::once(&self.library_icons).chain(self.pages.iter().map(|p| p.registry()));
        for registry in lists {
            if let Some(w) = registry.borrow().get(catalog_id) {
                w.set_in_library(in_library, None);
            }
        }
    }

    /// Record a favourite locally and repaint the row, without rebuilding the
    /// list — a rebuild would throw away the scroll position, and this is the
    /// same discipline as the play marker.
    pub(super) fn set_favorite(&mut self, catalog_id: &str, on: bool) {
        // The list store keeps its **own clone** of each entry, made when the
        // rows were built. Updating `all_tracks` and the visible widget is not
        // enough: scroll away and back and the row re-binds from that clone,
        // and the star returns. Correcting the stored item is what makes the
        // change survive recycling — and it is why this looked like the write
        // had failed when it had not.
        self.patch_stored_row(catalog_id, |track| track.favorite = on);
        for track in &mut self.all_tracks {
            if track.catalog_id.as_deref() == Some(catalog_id) {
                track.favorite = on;
            }
        }
        for page in &mut self.pages {
            page.set_favorite(catalog_id, on);
        }
        // Every list, for the same reason `set_row_playing` asks every list:
        // the track may be on a page and in the results behind it.
        let lists =
            std::iter::once(&self.library_icons).chain(self.pages.iter().map(|p| p.registry()));
        for registry in lists {
            if let Some(w) = registry.borrow().get(catalog_id) {
                // Star *and* facts: the menu reads the facts, so repainting
                // alone left a just-un-starred row still offering to un-star it.
                w.set_favorite(on);
            }
        }
    }
}
