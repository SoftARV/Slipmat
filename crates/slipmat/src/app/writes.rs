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

use super::AppModel;
use crate::components::TrackOverride;

impl AppModel {
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
