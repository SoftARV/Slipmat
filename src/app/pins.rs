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

use super::view::SidebarRow;
use super::{AppModel, AppMsg};
use crate::components::detail_page::PageKind;

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
            SidebarRow::Section(view) => sender.input(AppMsg::SetView(view)),
            SidebarRow::Pinned(id) => {
                self.push_page(PageKind::LibraryPlaylist(id), sender);
            }
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
