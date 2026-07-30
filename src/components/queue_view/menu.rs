// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! A queue row's context menu.
//!
//! Its own menu rather than `app/row_menu.rs`, because a row in the queue is a
//! different thing from a row in a list: it is already in the queue, so
//! "Add to Queue" is meaningless and "Play Next" means *move this*, not insert a
//! second copy. The account actions are deliberately absent — a queue is a
//! running order, not a place to edit your library from.
//!
//! Built imperatively and per click for the same reason the library's is: a
//! `ListView` recycles the widget underneath the popover while it is open, so it
//! is parented, shown and unparented per click rather than living in the tree.

use relm4::gtk;
use relm4::gtk::prelude::*;

use super::QueueViewInput;

/// Show the menu for the row at visible position `at`.
///
/// `movable` is false for the track that is playing and for the one already
/// next: an item that offers to move something where it already is reads as
/// broken. Offering nothing at all is not a risk — the other three are always
/// there.
pub fn show(
    sender: &relm4::Sender<QueueViewInput>,
    at: u32,
    over: &gtk::Widget,
    x: f64,
    y: f64,
    movable: bool,
) {
    let menu = gtk::gio::Menu::new();

    let queue = gtk::gio::Menu::new();
    if movable {
        queue.append(Some("Play _Next"), Some("queue-row.play-next"));
    }
    queue.append(Some("_Remove from Queue"), Some("queue-row.remove"));
    menu.append_section(None, &queue);

    // A second section: these leave the queue and go somewhere else in the app,
    // which is a different kind of act from reordering it.
    let browse = gtk::gio::Menu::new();
    browse.append(Some("Go to _Album"), Some("queue-row.album"));
    browse.append(Some("Go to A_rtist"), Some("queue-row.artist"));
    menu.append_section(None, &browse);

    let popover = gtk::PopoverMenu::from_model(Some(&menu));
    popover.set_has_arrow(false);
    popover.set_halign(gtk::Align::Start);
    popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
    popover.set_parent(over);

    // The actions live on the popover, so they go away with it rather than
    // accumulating on the window one right-click at a time.
    let actions = gtk::gio::SimpleActionGroup::new();
    for (name, msg) in [
        ("play-next", QueueViewInput::PlayNextAt(at)),
        ("remove", QueueViewInput::RemoveAt(at)),
        ("album", QueueViewInput::GoTo { at, album: true }),
        ("artist", QueueViewInput::GoTo { at, album: false }),
    ] {
        let action = gtk::gio::SimpleAction::new(name, None);
        let sender = sender.clone();
        // One message per item, cloned into the closure because an action can
        // fire more than once before the popover closes.
        let msg = std::cell::RefCell::new(Some(msg));
        action.connect_activate(move |_, _| {
            if let Some(msg) = msg.borrow_mut().take() {
                sender.emit(msg);
            }
        });
        actions.add_action(&action);
    }
    popover.insert_action_group("queue-row", Some(&actions));

    // Unparent on close, or it leaks and keeps the row widget alive after the
    // list has recycled it — but **not during** the close. GTK closes a
    // `PopoverMenu` *before* activating the item that was clicked, so
    // unparenting here tears down the action group a moment too early and every
    // item silently does nothing. Deferring to an idle lets the activation land.
    popover.connect_closed(|p| {
        let p = p.clone();
        gtk::glib::idle_add_local_once(move || p.unparent());
    });
    popover.popup();
}
