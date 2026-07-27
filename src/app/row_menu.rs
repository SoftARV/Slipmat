// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The row context menu: what it offers, and what each item sends.
//!
//! Built imperatively rather than in `view!` because it is per-row and
//! transient — a `ListView` recycles the widget underneath it while it is open,
//! so the popover is parented, shown, and unparented per click rather than
//! living in the tree.
//!
//! Each item appears only when it would do something. Offering "Add to Library"
//! for a track already saved, or "Favourite" for one already starred, is a menu
//! that lies about the state of things.

use relm4::gtk;
use relm4::gtk::prelude::*;

use super::{AppModel, AppMsg, LibraryAction};
use crate::components::track_row::RowMenuRequest;

impl AppModel {
    /// Show a row's context menu where it was clicked.
    ///
    /// Built fresh each time and parented to the row: a single long-lived
    /// popover would have to be re-parented on every click anyway, and a
    /// `ListView` recycles the widget under it while it is open.
    pub(super) fn show_row_menu(&self, req: RowMenuRequest) {
        let menu = gtk::gio::Menu::new();

        let queue = gtk::gio::Menu::new();
        queue.append(Some("Play _Next"), Some("row.play-next"));
        queue.append(Some("Add to _Queue"), Some("row.play-later"));
        menu.append_section(None, &queue);

        // A second section, because these leave the app and change the user's
        // account — a different kind of act from reordering a queue.
        //
        // Each item appears only when it would do something. Offering "Add to
        // Library" for a track read *out of* the library, or "Favourite" for
        // one already starred, is a menu that lies about the state of things.
        let account = gtk::gio::Menu::new();
        if req.in_library {
            // Only when we hold the library id — the catalog id will not do,
            // and offering a removal we cannot perform is worse than not
            // offering one.
            if req.library_id.is_some() {
                account.append(
                    Some("_Remove from Library"),
                    Some("row.remove-from-library"),
                );
            }
        } else {
            account.append(Some("Add to _Library"), Some("row.add-to-library"));
        }
        // Both directions now. The removals go out through the sidecar rather
        // than `music/client.rs`: over REST this exact favourites path answers
        // "400 Insufficient Permissions", and library removal has no documented
        // endpoint at all. MusicKit's own client does both (#34).
        if req.favorite {
            account.append(Some("Remove _Favourite"), Some("row.unfavorite"));
        } else {
            account.append(Some("_Favourite"), Some("row.favorite"));
        }
        if account.n_items() > 0 {
            menu.append_section(None, &account);
        }

        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        popover.set_has_arrow(false);
        popover.set_halign(gtk::Align::Start);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(req.at.0, req.at.1, 1, 1)));
        popover.set_parent(&req.over);

        // The actions live on the popover, so they go away with it rather than
        // accumulating on the window one right-click at a time.
        let actions = gtk::gio::SimpleActionGroup::new();
        for (name, next) in [("play-next", true), ("play-later", false)] {
            let action = gtk::gio::SimpleAction::new(name, None);
            let id = req.catalog_id.clone();
            let sender = self.menu_sender.clone();
            action.connect_activate(move |_, _| {
                sender.input(AppMsg::Enqueue {
                    catalog_id: id.clone(),
                    next,
                });
            });
            actions.add_action(&action);
        }

        // The two removals. Separate from the adds below because they take a
        // different route entirely — the sidecar, not HTTP — and because
        // removal needs the *library* id while un-favouriting needs the
        // *catalog* one.
        {
            let action = gtk::gio::SimpleAction::new("remove-from-library", None);
            let library_id = req.library_id.clone();
            let catalog_id = req.catalog_id.clone();
            let sender = self.menu_sender.clone();
            action.connect_activate(move |_, _| {
                if let Some(library_id) = library_id.clone() {
                    sender.input(AppMsg::RemoveFromLibrary {
                        library_id,
                        catalog_id: catalog_id.clone(),
                    });
                }
            });
            actions.add_action(&action);

            let action = gtk::gio::SimpleAction::new("unfavorite", None);
            let catalog_id = req.catalog_id.clone();
            let sender = self.menu_sender.clone();
            action.connect_activate(move |_, _| {
                sender.input(AppMsg::Unfavorite {
                    catalog_id: catalog_id.clone(),
                });
            });
            actions.add_action(&action);
        }

        for (name, what) in [
            ("add-to-library", LibraryAction::AddToLibrary),
            ("favorite", LibraryAction::Favorite),
        ] {
            let action = gtk::gio::SimpleAction::new(name, None);
            let id = req.catalog_id.clone();
            let sender = self.menu_sender.clone();
            action.connect_activate(move |_, _| {
                sender.input(AppMsg::LibraryWrite {
                    catalog_id: id.clone(),
                    action: what,
                });
            });
            actions.add_action(&action);
        }
        popover.insert_action_group("row", Some(&actions));

        // Unparent on close, or it leaks and keeps the row widget alive after
        // the list has recycled it out from under us — but **not during** the
        // close.
        //
        // GTK closes a PopoverMenu *before* activating the item you clicked. So
        // unparenting here tore down the action group a moment before the
        // action fired, and every menu item silently did nothing: no command
        // left Rust, and the sidecar logged nothing because nothing was sent.
        // Deferring to an idle lets the activation land first.
        popover.connect_closed(|p| {
            let p = p.clone();
            gtk::glib::idle_add_local_once(move || p.unparent());
        });
        popover.popup();
    }
}
