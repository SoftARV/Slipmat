// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The one-item menu a playlist tile raises: pin it, or unpin it.
//!
//! The picker is where you manage pins deliberately. This is for the moment you
//! are already looking at a playlist and decide it belongs in the sidebar —
//! which is a different moment, and the one where a dialog is in the way.

use relm4::gtk;
use relm4::gtk::prelude::*;

use super::super::{AppModel, AppMsg};
use crate::components::grid_item::TileMenuRequest;

impl AppModel {
    /// Show the pin menu over a playlist tile.
    ///
    /// One item, and its label is the whole design: a menu that offers "Add to
    /// the Sidebar" for something already there is a menu that lies about the
    /// state of things — the same rule the row menu follows for Favourite.
    ///
    /// Built fresh each click and parented to the tile, because the grid
    /// recycles the widget underneath a popover that is already open.
    pub(in crate::app) fn show_tile_menu(&self, req: TileMenuRequest) {
        let pinned = self.settings.pinned_playlists.contains(&req.playlist_id);

        let menu = gtk::gio::Menu::new();
        menu.append(
            Some(if pinned {
                "_Remove from the Sidebar"
            } else {
                "_Add to the Sidebar"
            }),
            Some("tile.toggle-pin"),
        );

        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        popover.set_has_arrow(false);
        popover.set_halign(gtk::Align::Start);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(req.at.0, req.at.1, 1, 1)));
        popover.set_parent(&req.over);

        // The action lives on the popover, so it goes away with it rather than
        // accumulating on the window one right-click at a time.
        let actions = gtk::gio::SimpleActionGroup::new();
        let action = gtk::gio::SimpleAction::new("toggle-pin", None);
        let id = req.playlist_id.clone();
        let sender = self.menu_sender.clone();
        action.connect_activate(move |_, _| {
            sender.input(AppMsg::SetPinned {
                id: id.clone(),
                pinned: !pinned,
            });
        });
        actions.add_action(&action);
        popover.insert_action_group("tile", Some(&actions));

        // GTK closes a `PopoverMenu` *before* activating the item you clicked,
        // so unparenting on close would destroy the action group first and the
        // click would do nothing. Deferred by one main-loop turn — the same trap
        // `row_menu` documents.
        popover.connect_closed(|popover| {
            let popover = popover.clone();
            gtk::glib::idle_add_local_once(move || popover.unparent());
        });
        popover.popup();
    }
}
