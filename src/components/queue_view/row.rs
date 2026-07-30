// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! One row of the queue: a track, or the disclosure that hides the played ones.
//!
//! Two rules shape everything here, and both are the same rule twice.
//!
//! **A row never holds a position.** It reads `ListItem::position()` at the
//! moment something happens. A queue is edited in place, so surviving rows are
//! renumbered without being re-bound — an index captured at bind time quietly
//! starts naming a different track, and a drag then moves the wrong one. GTK
//! keeps `position` correct for exactly this reason.
//!
//! **Every handler is connected once, in `setup`.** They outlive every track
//! that passes through the widget, because none of them closes over anything
//! that changes: the position comes from the list item, and everything else
//! comes from [`Shared`]. That removes the whole class of bug the old row had to
//! defend against by disconnecting and reconnecting six handlers per bind.
//!
//! What `bind` does is therefore only drawing — and it sets **every** property,
//! because the widget it is handed was showing a different track a moment ago
//! and anything left alone keeps that track's value.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use relm4::gtk::prelude::*;
use relm4::typed_view::list::RelmListItem;
use relm4::{RelmWidgetExt, gtk, view};

use super::QueueViewInput;
use crate::music::types::format_duration;

/// What a row draws. **No id and no index** — deliberately.
///
/// The store used to hold both, and both were read: that is how a stale `at`
/// came to name the wrong track. Which track sits at a visible position is a
/// question for `QueueView::rows`, which is rebuilt on every sync and is the
/// only thing entitled to answer it.
#[derive(Debug, Clone, PartialEq)]
pub enum QueueItem {
    /// The disclosure standing in for the tracks already played.
    History { hidden: usize, expanded: bool },
    Track {
        title: String,
        artist: String,
        duration_ms: u64,
    },
}

/// A row that is bound and on screen, and the two widgets its marker touches.
///
/// `ListView` recycles rows, so most of the queue has no widget at any moment.
/// The marker is therefore moved by repainting these directly rather than by
/// editing the store — **any** structural edit is a chance to lose the scroll
/// position (#6), and the marker moves on every track change.
pub struct BoundRow {
    pub item: gtk::ListItem,
    icon: gtk::Image,
    remove: gtk::Button,
}

pub type Bound = Rc<RefCell<Vec<BoundRow>>>;

/// What every row needs and no row owns.
///
/// A thread-local rather than a field on the item, because `setup` is a static
/// function with no component behind it — and because putting it here is what
/// lets the store hold nothing but what is drawn. There is exactly one queue in
/// the app; a second would need this to become per-component state.
pub struct Shared {
    pub sender: relm4::Sender<QueueViewInput>,
    /// The **visible position** of the playing row, not its id: a queue may hold
    /// the same track twice (#88), so an id marks both copies.
    pub playing: Rc<Cell<Option<u32>>>,
    pub bound: Bound,
}

thread_local! {
    static SHARED: RefCell<Option<Rc<Shared>>> = const { RefCell::new(None) };
}

pub fn install(shared: Rc<Shared>) {
    SHARED.with(|s| *s.borrow_mut() = Some(shared));
}

fn shared() -> Option<Rc<Shared>> {
    SHARED.with(|s| s.borrow().clone())
}

fn emit(msg: QueueViewInput) {
    if let Some(shared) = shared() {
        shared.sender.emit(msg);
    }
}

pub struct QueueItemWidgets {
    /// The two faces. A `GtkBox` with one child hidden, **not** a `GtkStack`:
    /// a stack measures its hidden children on both axes, so the disclosure's
    /// width would become every row's minimum width.
    track: gtk::Box,
    history: gtk::Box,
    chevron: gtk::Image,
    hint: gtk::Label,
    icon: gtk::Image,
    title: gtk::Label,
    artist: gtk::Label,
    duration: gtk::Label,
    remove: gtk::Button,
    /// Whether this widget is currently a track, read by the drag and drop
    /// handlers — which are connected once and so cannot be told at connect
    /// time. The disclosure is neither draggable nor a drop site.
    is_track: Rc<Cell<bool>>,
    /// The row's own list item, so its **current** position is what every
    /// handler reports. See the module header.
    item: gtk::ListItem,
}

impl RelmListItem for QueueItem {
    type Root = gtk::Box;
    type Widgets = QueueItemWidgets;

    fn setup(list_item: &gtk::ListItem) -> (Self::Root, Self::Widgets) {
        crate::components::count_widget("queue row");

        view! {
            root = gtk::Box {
                set_orientation: gtk::Orientation::Vertical,

                #[name = "track"]
                gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_spacing: 10,
                    set_margin_all: 6,

                    #[name = "icon"]
                    gtk::Image {
                        set_pixel_size: 16,
                        set_valign: gtk::Align::Center,
                    },

                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_valign: gtk::Align::Center,
                        set_hexpand: true,
                        // A queue pane is 206px wide in a 460px window, so the
                        // text column has to be allowed to become narrow. An
                        // ellipsizing label's *minimum* width is the ellipsis,
                        // but its natural width is the whole title — and a
                        // scroller left on its default policy allocates the
                        // natural one and grows a horizontal scrollbar instead.
                        // The policy is set on the scroller; the ellipsize
                        // below is the half that lets the text give way.

                        #[name = "title"]
                        gtk::Label {
                            set_xalign: 0.0,
                            set_ellipsize: gtk::pango::EllipsizeMode::End,
                            set_single_line_mode: true,
                        },

                        #[name = "artist"]
                        gtk::Label {
                            set_xalign: 0.0,
                            set_ellipsize: gtk::pango::EllipsizeMode::End,
                            set_single_line_mode: true,
                            add_css_class: "caption",
                            add_css_class: "dim-label",
                        },
                    },

                    #[name = "duration"]
                    gtk::Label {
                        set_valign: gtk::Align::Center,
                        add_css_class: "numeric",
                        add_css_class: "dim-label",
                    },

                    #[name = "remove"]
                    gtk::Button {
                        set_icon_name: "list-remove-symbolic",
                        set_tooltip_text: Some("Remove from queue"),
                        set_valign: gtk::Align::Center,
                        add_css_class: "flat",
                        add_css_class: "circular",
                        // Must not take focus. Focus is what loses the scroll
                        // position when the row is removed (#6) — though the
                        // focus that matters is the row's own, so this is a
                        // tidiness rather than the fix. See `drop_focus`.
                        set_focus_on_click: false,
                    },
                },

                // The disclosure. Its own face rather than a track row wearing
                // different labels, because it is a control and not an item.
                #[name = "history"]
                gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_spacing: 10,
                    set_margin_all: 6,
                    set_visible: false,

                    #[name = "chevron"]
                    gtk::Image {
                        set_pixel_size: 16,
                        set_valign: gtk::Align::Center,
                        add_css_class: "dim-label",
                    },

                    #[name = "hint"]
                    gtk::Label {
                        set_xalign: 0.0,
                        set_hexpand: true,
                        set_ellipsize: gtk::pango::EllipsizeMode::End,
                        set_single_line_mode: true,
                        add_css_class: "caption",
                        add_css_class: "dim-label",
                    },
                },
            }
        }

        let is_track = Rc::new(Cell::new(true));

        // Remove. The position is read now, not captured at bind.
        let item = list_item.clone();
        remove.connect_clicked(move |_| emit(QueueViewInput::RemoveAt(item.position())));

        // Right-click anywhere on the row. Secondary button only: a gesture
        // that claims any button would eat the click that plays the track.
        let menu = gtk::GestureClick::new();
        menu.set_button(gtk::gdk::BUTTON_SECONDARY);
        let item = list_item.clone();
        let over = root.clone();
        let track_row = is_track.clone();
        menu.connect_pressed(move |_, _, x, y| {
            if track_row.get() {
                emit(QueueViewInput::Menu {
                    at: item.position(),
                    over: over.clone().upcast(),
                    x,
                    y,
                });
            }
        });
        root.add_controller(menu);

        // `MOVE`, not `COPY`: the row is going somewhere, not being duplicated.
        // The payload is the row's own visible position, which is all the other
        // end needs to name it.
        let drag = gtk::DragSource::new();
        drag.set_actions(gtk::gdk::DragAction::MOVE);
        let item = list_item.clone();
        let track_row = is_track.clone();
        drag.connect_prepare(move |_, _, _| {
            track_row
                .get()
                .then(|| gtk::gdk::ContentProvider::for_value(&item.position().to_value()))
        });
        // Live feedback, which is the whole of what the old drag was missing:
        // the row travels with the pointer as a picture of itself, and the one
        // it left behind dims so it reads as in transit rather than duplicated.
        let picture = root.clone();
        drag.connect_drag_begin(move |source, _| {
            let paintable = gtk::WidgetPaintable::new(Some(&picture));
            source.set_icon(Some(&paintable), picture.width() / 2, picture.height() / 2);
            picture.add_css_class("queue-dragging");
        });
        let picture = root.clone();
        drag.connect_drag_end(move |_, _, _| picture.remove_css_class("queue-dragging"));
        root.add_controller(drag);

        let drop = gtk::DropTarget::new(u32::static_type(), gtk::gdk::DragAction::MOVE);
        // Where it would land, drawn as a line on the edge the row would take.
        // Without this a drag says only "somewhere in this list".
        let edge = root.clone();
        let track_row = is_track.clone();
        drop.connect_motion(move |_, _, y| {
            if !track_row.get() {
                return gtk::gdk::DragAction::empty();
            }
            mark_edge(&edge, Some(below_middle(&edge, y)));
            gtk::gdk::DragAction::MOVE
        });
        let edge = root.clone();
        drop.connect_leave(move |_| mark_edge(&edge, None));
        let edge = root.clone();
        let item = list_item.clone();
        let track_row = is_track.clone();
        drop.connect_drop(move |_, value, _, y| {
            mark_edge(&edge, None);
            let Ok(from) = value.get::<u32>() else {
                return false;
            };
            if !track_row.get() {
                return false;
            }
            emit(QueueViewInput::Dropped {
                from,
                over: item.position(),
                below: below_middle(&edge, y),
            });
            true
        });
        root.add_controller(drop);

        (
            root,
            QueueItemWidgets {
                track,
                history,
                chevron,
                hint,
                icon,
                title,
                artist,
                duration,
                remove,
                is_track,
                item: list_item.clone(),
            },
        )
    }

    fn bind(&mut self, widgets: &mut Self::Widgets, _root: &mut Self::Root) {
        match self {
            Self::Track {
                title,
                artist,
                duration_ms,
            } => {
                widgets.is_track.set(true);
                widgets.track.set_visible(true);
                widgets.history.set_visible(false);
                widgets.title.set_label(title);
                widgets.artist.set_label(artist);
                widgets.duration.set_label(&format_duration(*duration_ms));

                let playing = shared()
                    .and_then(|s| s.playing.get())
                    .is_some_and(|at| at == widgets.item.position());
                apply_playing(&widgets.icon, &widgets.remove, playing);
            }
            Self::History { hidden, expanded } => {
                widgets.is_track.set(false);
                widgets.track.set_visible(false);
                widgets.history.set_visible(true);
                widgets.chevron.set_icon_name(Some(if *expanded {
                    "pan-down-symbolic"
                } else {
                    "pan-end-symbolic"
                }));
                widgets.hint.set_label(&match hidden {
                    1 => "1 played".to_owned(),
                    n => format!("{n} played"),
                });
                widgets.hint.set_tooltip_text(Some(if *expanded {
                    "Hide the tracks already played"
                } else {
                    "Show the tracks already played"
                }));
            }
        }

        // Published so the marker can move without the store being touched.
        if let Some(shared) = shared() {
            shared.bound.borrow_mut().push(BoundRow {
                item: widgets.item.clone(),
                icon: widgets.icon.clone(),
                remove: widgets.remove.clone(),
            });
        }
    }

    fn unbind(&mut self, widgets: &mut Self::Widgets, _root: &mut Self::Root) {
        // No handlers to disconnect — they are all connected once and read the
        // position they need at event time. Only the publication is undone.
        if let Some(shared) = shared() {
            shared
                .bound
                .borrow_mut()
                .retain(|row| row.item != widgets.item);
        }
    }
}

/// Whether the pointer is in the lower half of a row, so a drop reads as
/// "between these two" rather than "onto this one".
fn below_middle(row: &gtk::Box, y: f64) -> bool {
    y > f64::from(row.height()) / 2.0
}

/// Draw, or clear, the line a drop would land on.
fn mark_edge(row: &gtk::Box, below: Option<bool>) {
    row.remove_css_class("queue-drop-above");
    row.remove_css_class("queue-drop-below");
    match below {
        Some(true) => row.add_css_class("queue-drop-below"),
        Some(false) => row.add_css_class("queue-drop-above"),
        None => {}
    }
}

/// Paint a row's marker. Shared by the bind path and the live-update path so
/// the two cannot drift apart.
pub fn apply_playing(icon: &gtk::Image, remove: &gtk::Button, playing: bool) {
    icon.set_icon_name(Some(if playing {
        "media-playback-start-symbolic"
    } else {
        "audio-x-generic-symbolic"
    }));
    icon.set_css_classes(if playing { &["accent"] } else { &["dim-label"] });
    // Removing the track you are listening to is a stop dressed up as an edit;
    // skip is the button for that.
    remove.set_sensitive(!playing);
}

/// Repaint every row that is on screen, from where it sits **now**.
///
/// Called after any structural edit as well as on a track change: a move
/// renumbers the rows between its ends without re-binding them, so their
/// markers are stale even though their contents are right.
pub fn repaint(bound: &Bound, playing: Option<u32>) {
    for row in bound.borrow().iter() {
        apply_playing(&row.icon, &row.remove, playing == Some(row.item.position()));
    }
}
