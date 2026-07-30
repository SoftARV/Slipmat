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
    /// Whether the tracks before the current one are folded away. Read by the
    /// drop handlers, which decide where a line may be drawn — see
    /// [`drop_below`].
    pub collapsed: Rc<Cell<bool>>,
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
        //
        // **The payload does not name the track**, and cannot: the store holds
        // only what is drawn, so all a row knows about itself is where it sits
        // now. A visible position captured here is stale by the time the drop
        // lands seconds later — a track boundary alone is enough to shift the
        // list — so the component is told to remember *which* track this is at
        // drag start, and resolves it again at drop. The payload exists only to
        // say the drag is ours.
        let drag = gtk::DragSource::new();
        drag.set_actions(gtk::gdk::DragAction::MOVE);
        let item = list_item.clone();
        let track_row = is_track.clone();
        drag.connect_prepare(move |_, _, _| {
            if !track_row.get() {
                return None;
            }
            let position = item.position();
            emit(QueueViewInput::DragStarted(position));
            Some(gtk::gdk::ContentProvider::for_value(&position.to_value()))
        });
        // Live feedback, which is the whole of what the old drag was missing:
        // the row travels with the pointer as a picture of itself, and the one
        // it left behind dims so it reads as in transit rather than duplicated.
        //
        // The dimming goes on the *list item* rather than on `root`, because a
        // `GtkWidgetPaintable` renders its widget live rather than snapshotting
        // it — dimming the widget the icon is made from dims the icon too, and
        // the thing under the pointer all but disappears.
        let picture = root.clone();
        let dimmed = list_item.clone();
        drag.connect_drag_begin(move |source, _| {
            let paintable = gtk::WidgetPaintable::new(Some(&picture));
            source.set_icon(Some(&paintable), picture.width() / 2, picture.height() / 2);
            if let Some(row) = dimmed.child().and_then(|c| c.parent()) {
                row.add_css_class("queue-dragging");
            }
        });
        let dimmed = list_item.clone();
        drag.connect_drag_end(move |_, _, _| {
            if let Some(row) = dimmed.child().and_then(|c| c.parent()) {
                row.remove_css_class("queue-dragging");
            }
        });
        root.add_controller(drag);

        let drop = gtk::DropTarget::new(u32::static_type(), gtk::gdk::DragAction::MOVE);
        // Where it would land, drawn as a line on the edge the row would take.
        // Without this a drag says only "somewhere in this list".
        let edge = root.clone();
        let item = list_item.clone();
        let track_row = is_track.clone();
        drop.connect_motion(move |_, _, y| {
            if !track_row.get() {
                return gtk::gdk::DragAction::empty();
            }
            mark_edge(&edge, Some(drop_below(&edge, &item, y)));
            gtk::gdk::DragAction::MOVE
        });
        let edge = root.clone();
        drop.connect_leave(move |_| mark_edge(&edge, None));
        let edge = root.clone();
        let item = list_item.clone();
        let track_row = is_track.clone();
        drop.connect_drop(move |_, value, _, y| {
            mark_edge(&edge, None);
            if value.get::<u32>().is_err() || !track_row.get() {
                return false;
            }
            emit(QueueViewInput::Dropped {
                over: item.position(),
                below: drop_below(&edge, &item, y),
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

    fn bind(&mut self, widgets: &mut Self::Widgets, root: &mut Self::Root) {
        // A recycled widget can arrive still wearing what a drag left on it: a
        // row highlighted as a drop target may be unbound before `leave` or
        // `drop` runs, and a track boundary mid-drag is enough to do it. Cleared
        // here like every other property, and for the same reason.
        //
        // **Our own widget only.** This reached `root.parent()` for the dimming
        // class, and that aborts the app: during a splice GTK is recycling list
        // item widgets, so `parent()` can hand back one whose last reference is
        // the temporary you were given — dropping it finalises the widget, which
        // emits `unbind` on the factory, which asks relm4 for the item that
        // `bind` is *still holding mutably*. "RefCell already borrowed", from a
        // line that only removes a CSS class. The dimming needs no clearing
        // anyway: `drag_begin` and `drag_end` always pair.
        mark_edge(root, None);
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
                // **"Earlier", not "played".** What this folds away is
                // everything before the queue's cursor, which is not the same
                // thing: `play_entries` enqueues the whole list and starts at
                // the clicked row, so clicking track 300 of a playlist puts 299
                // tracks behind the disclosure that nobody has heard. A
                // restored session says it too, having played nothing at all.
                widgets.hint.set_label(&match hidden {
                    1 => "1 earlier".to_owned(),
                    n => format!("{n} earlier"),
                });
                widgets.hint.set_tooltip_text(Some(if *expanded {
                    "Hide the tracks before this one"
                } else {
                    "Show the tracks before this one"
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

/// The same, except above the current track while the history is collapsed.
///
/// A row dropped there files itself into the past and disappears behind the
/// disclosure, so the drag reads as a delete. `earliest_visible_slot` refuses
/// the move; this is the half that keeps the promise honest, by drawing the
/// line where the drop will actually land rather than where it cannot.
fn drop_below(row: &gtk::Box, item: &gtk::ListItem, y: f64) -> bool {
    below_middle(row, y)
        || shared().is_some_and(|s| s.collapsed.get() && s.playing.get() == Some(item.position()))
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

/// Which row holds keyboard focus, if any is on screen.
///
/// Focus sits either on the `GtkListItemWidget` — GTK's own wrapper, the parent
/// of our root — or on a button inside the row, so both are asked. Used to
/// decide whether an edit is one that would cost the scroll position; see
/// `QueueView::apply`.
pub fn focused_row(view: &gtk::ListView, bound: &Bound) -> Option<u32> {
    let window = view.root().and_downcast::<gtk::Window>()?;
    let focused = gtk::prelude::GtkWindowExt::focus(&window)?;
    bound.borrow().iter().find_map(|row| {
        let root = row.item.child()?;
        let holds = focused == root
            || focused.is_ancestor(&root)
            || root.parent().is_some_and(|parent| parent == focused);
        holds.then(|| row.item.position())
    })
}

/// Repaint every row that is on screen, from where it sits **now**.
///
/// Called after any structural edit as well as on a track change: a move
/// renumbers the rows between its ends without re-binding them, so their
/// markers are stale even though their contents are right.
pub fn repaint(bound: &Bound, playing: Option<u32>) {
    // **Collected before anything is touched.** A GTK setter can move focus,
    // and focus moving can make `ListView` re-materialise a row — which binds,
    // which takes `borrow_mut()` on this same cell. Holding the iteration
    // borrow across a setter would turn that into an abort rather than a
    // repaint, and an abort is not a failure this app is allowed to have.
    let rows: Vec<(gtk::Image, gtk::Button, u32)> = bound
        .borrow()
        .iter()
        .map(|row| (row.icon.clone(), row.remove.clone(), row.item.position()))
        .collect();
    for (icon, remove, position) in rows {
        apply_playing(&icon, &remove, playing == Some(position));
    }
}
