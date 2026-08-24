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

/// Which row the menu is for, and what it may offer.
///
/// A struct rather than four more parameters: they are all facts about one row,
/// and read as a list of unlabelled booleans at the call site otherwise.
pub struct Target<'a> {
    /// Where the row sat in the queue when the menu was opened.
    pub at: usize,
    /// What it was — the half that survives the queue moving. See [`show`].
    pub id: &'a str,
    /// The catalog id, if Apple gave one. `None` means the album and artist
    /// cannot be looked up, so they are not offered.
    pub catalog_id: Option<&'a str>,
    /// False for the track that is playing and for the one already next.
    pub movable: bool,
    /// False for the track that is playing.
    pub removable: bool,
}

/// Show the menu for a queue row.
///
/// **Takes the track's identity, never its position on screen.** A popover
/// waits on a person, and the list moves while it is open — a gapless advance
/// alone renumbers every row once the played track folds away. A position
/// captured here and spent on the click that follows names a different track,
/// which is the trap `row.rs` exists to avoid and the one place a widget cannot
/// avoid it by reading `ListItem::position()` at event time.
///
/// So this carries `(at, id)`, the same pair the row activation sends: the
/// position says which copy of a duplicated track was meant, and the id lets
/// the queue check whether it has moved since (#88).
///
/// `movable` is false for the track that is playing and for the one already
/// next; `removable` is false for the track that is playing, because the row's
/// own remove button is deliberately insensitive there — removing what you are
/// listening to is a stop dressed up as an edit. Two affordances for one act
/// must not disagree about when it is allowed.
pub fn show(
    sender: &relm4::Sender<QueueViewInput>,
    target: Target<'_>,
    over: &gtk::Widget,
    x: f64,
    y: f64,
) {
    let Target {
        at,
        id,
        catalog_id,
        movable,
        removable,
    } = target;
    let menu = gtk::gio::Menu::new();

    let queue = gtk::gio::Menu::new();
    if movable {
        queue.append(Some("Play _Next"), Some("queue-row.play-next"));
    }
    if removable {
        queue.append(Some("_Remove from Queue"), Some("queue-row.remove"));
    }
    if queue.n_items() > 0 {
        menu.append_section(None, &queue);
    }

    // A second section: these leave the queue and go somewhere else in the app,
    // which is a different kind of act from reordering it. Only when the track
    // has a catalog id to look them up with — a menu item that can only fail is
    // worse than one that is not there.
    if catalog_id.is_some() {
        let browse = gtk::gio::Menu::new();
        browse.append(Some("Go to _Album"), Some("queue-row.album"));
        browse.append(Some("Go to A_rtist"), Some("queue-row.artist"));
        menu.append_section(None, &browse);
    }

    let popover = gtk::PopoverMenu::from_model(Some(&menu));
    popover.set_has_arrow(false);
    popover.set_halign(gtk::Align::Start);
    popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
    popover.set_parent(over);

    // The actions live on the popover, so they go away with it rather than
    // accumulating on the window one right-click at a time.
    let actions = gtk::gio::SimpleActionGroup::new();
    for (name, msg) in [
        (
            "play-next",
            QueueViewInput::PlayNext {
                at,
                id: id.to_owned(),
            },
        ),
        (
            "remove",
            QueueViewInput::RemoveTrack {
                at,
                id: id.to_owned(),
            },
        ),
        (
            "album",
            QueueViewInput::GoTo {
                id: catalog_id.unwrap_or_default().to_owned(),
                album: true,
            },
        ),
        (
            "artist",
            QueueViewInput::GoTo {
                id: catalog_id.unwrap_or_default().to_owned(),
                album: false,
            },
        ),
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
