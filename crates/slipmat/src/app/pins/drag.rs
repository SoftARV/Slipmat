// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Dragging a pinned row to reorder it.
//!
//! Split out of `wiring` because it is a pin's concern rather than the window's,
//! and because `wiring` was one line over its budget — which is the ratchet
//! working rather than a number to raise.
//!
//! The same shape as the queue's rows (`components::queue_view::row`), and the
//! two share their CSS. What is *not* shared is the hard part of that one: these
//! rows are real and permanent for as long as the pins are unchanged, so a
//! captured index cannot go stale under them the way a virtualised list's can.

use relm4::ComponentSender;
use relm4::gtk;
use relm4::gtk::prelude::*;

use super::super::{AppModel, AppMsg};

/// Make one pinned row draggable, and a target for the others.
///
/// The same shape as the queue's rows (`queue_view::row`), minus the parts that
/// only a virtualised list needs: these rows are real and permanent for as long
/// as the pins are unchanged, so a captured index cannot go stale under them.
pub(in crate::app) fn attach(
    row: &gtk::ListBoxRow,
    index: usize,
    sender: &ComponentSender<AppModel>,
) {
    let drag = gtk::DragSource::new();
    drag.set_actions(gtk::gdk::DragAction::MOVE);
    // The payload says "this drag is ours, and it started here". Nothing reads
    // it as a widget, so an index is enough.
    let position = index as u32;
    drag.connect_prepare(move |_, _, _| {
        Some(gtk::gdk::ContentProvider::for_value(&position.to_value()))
    });
    // The row travels with the pointer as a picture of itself, and the one it
    // left behind dims — so it reads as in transit rather than duplicated.
    let picture = row.clone();
    drag.connect_drag_begin(move |source, _| {
        let paintable = gtk::WidgetPaintable::new(Some(&picture));
        source.set_icon(Some(&paintable), picture.width() / 2, picture.height() / 2);
        picture.add_css_class("dragging");
    });
    let dimmed = row.clone();
    drag.connect_drag_end(move |_, _, _| dimmed.remove_css_class("dragging"));
    row.add_controller(drag);

    let drop = gtk::DropTarget::new(u32::static_type(), gtk::gdk::DragAction::MOVE);
    // Where it would land, drawn on the edge the row would take. Without this a
    // drag says only "somewhere in this list".
    let edge = row.clone();
    drop.connect_motion(move |_, _, y| {
        mark_edge(&edge, Some(below_middle(&edge, y)));
        gtk::gdk::DragAction::MOVE
    });
    let edge = row.clone();
    drop.connect_leave(move |_| mark_edge(&edge, None));
    let edge = row.clone();
    let sender = sender.clone();
    drop.connect_drop(move |_, value, _, y| {
        mark_edge(&edge, None);
        let Ok(from) = value.get::<u32>() else {
            return false;
        };
        sender.input(AppMsg::MovePin {
            from: from as usize,
            slot: super::drop_slot(index, below_middle(&edge, y)),
        });
        true
    });
    row.add_controller(drop);
}

/// Whether the pointer is in the lower half of a row, so a drop reads as
/// "between these two" rather than "onto this one".
fn below_middle(row: &gtk::ListBoxRow, y: f64) -> bool {
    y > f64::from(row.height()) / 2.0
}

/// Draw, or clear, the line a drop would land on.
fn mark_edge(row: &gtk::ListBoxRow, below: Option<bool>) {
    row.remove_css_class("drop-above");
    row.remove_css_class("drop-below");
    match below {
        Some(true) => row.add_css_class("drop-below"),
        Some(false) => row.add_css_class("drop-above"),
        None => {}
    }
}
