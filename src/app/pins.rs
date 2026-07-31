// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Playlists pinned to the sidebar — what they mean, and what clicking one does.
//!
//! Getting to a playlist you actually listen to is two navigations every time,
//! and they are the thing most likely to be opened daily. A pin makes it one
//! click from anywhere (#133).
//!
//! **A pin is an id, not a copy.** The name is looked up against the library
//! every time the rows are built, so renaming a playlist on your phone renames
//! the row, and nothing here can go quietly out of date. The cost is that a pin
//! whose playlist has been deleted resolves to nothing — see `stale_pins`.

use relm4::ComponentSender;

use super::pages::Arrival;
use super::view::SidebarRow;
use super::{AppModel, AppMsg};
use crate::components::detail_page::PageKind;

/// What a pin says when the library has never produced its playlist.
///
/// Never the id: `p.EYWrg13SzrKxYBb` tells nobody anything. Seen briefly at
/// startup before the cache is read, and permanently for a pin whose playlist
/// was deleted elsewhere — which is what phase five prunes.
pub(super) const UNAVAILABLE: &str = "Unavailable";

impl AppModel {
    /// What a sidebar row selection means.
    ///
    /// The sidebar reports a *position*; this is the only place that turns one
    /// back into an action, because a pin and a section sit in the same list and
    /// do entirely different things — one changes what the pane shows, the other
    /// pushes a page on top of it.
    pub(super) fn sidebar_row_chosen(&mut self, index: i32, sender: &ComponentSender<Self>) {
        // A position with no row is not an error worth reporting: `ListBox`
        // reports a selection while rows are being rebuilt underneath it.
        let Some(row) = usize::try_from(index)
            .ok()
            .and_then(|i| self.sidebar_rows.get(i))
            .cloned()
        else {
            return;
        };

        match row {
            SidebarRow::Section(view) => {
                // **Popped here, not in `SetView`.** That arm returns early when
                // the section has not changed, which is right for its other
                // callers and wrong for this one: choosing "All" while a pinned
                // playlist is open changes no section, so nothing popped and the
                // grid stayed hidden behind the page until you pressed Back.
                // Choosing a section always means "show me that", pushed page or
                // not.
                self.pop_to_results();
                sender.input(AppMsg::SetView(view));
            }
            // A destination, not a drill-down: replaces whatever was open rather
            // than stacking on top of it, and draws no back button.
            SidebarRow::Pinned(id) => {
                self.pop_to_results();
                self.open_page(
                    PageKind::LibraryPlaylist(id),
                    sender,
                    Arrival::FromTheSidebar,
                );
            }
        }
    }

    /// Put the right name on every pinned row.
    ///
    /// **Labels, not rows.** The sidebar is built before the library cache is
    /// read, so pins are drawn nameless and have to be filled in afterwards —
    /// and again whenever the library reloads, so renaming a playlist elsewhere
    /// renames the row. Rebuilding the rows would do it too, and would clear the
    /// sidebar's selection on every library load: the failure 285b542 removed.
    pub(super) fn refresh_pin_names(&self) {
        for (id, label) in &self.pin_labels {
            label.set_label(self.pinned_name(id).unwrap_or(UNAVAILABLE));
        }
    }

    /// The name to draw for a pin, or `None` if the library has never heard of
    /// it.
    ///
    /// `None` covers two different situations that look identical here and must
    /// not be conflated: a playlist deleted elsewhere, and a library that has
    /// not finished loading. Only the first is a stale pin, and only a *loaded*
    /// library can tell them apart — which is why nothing is pruned from here.
    pub(super) fn pinned_name(&self, id: &str) -> Option<&str> {
        self.playlists
            .iter()
            .find(|playlist| playlist.id == id)
            .map(|playlist| playlist.name.as_str())
    }
}
