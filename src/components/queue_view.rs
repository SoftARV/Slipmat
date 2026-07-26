// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! What's playing next — MusicKit's queue, as a sidebar.
//!
//! Two rules, both learned the hard way and both re-learned inside this file:
//!
//! 1. **Address MusicKit's queue, never a position we derived.** Its queue and
//!    our rows are not the same list.
//! 2. **Identify a track by its id, not its index.** Rows emit an id; `app.rs`
//!    resolves it against the live queue at send time. The one exception is
//!    activation, where GTK hands us a position that we turn straight back into
//!    an id before anything else can run.
//!
//! Like the library, this is a `ListView` and not a `ListBox`: a 500-track
//! queue is 500 live widgets otherwise, on screen at the same time as the
//! library's 541.

use relm4::adw::prelude::*;
use relm4::prelude::*;
use relm4::typed_view::list::{RelmListItem, TypedListView};
use relm4::{Component, ComponentParts, ComponentSender, adw, gtk, view};

use crate::components::{CurrentTrack, RowRegistry, current_track, row_registry};
use crate::music::types::format_duration;

/// The widgets a queue row publishes while it is on screen.
#[derive(Debug, Clone)]
pub struct QueueRowWidgets {
    icon: gtk::Image,
    remove: gtk::Button,
}

/// One queue entry, flattened from the sidecar's view of MusicKit's queue.
#[derive(Debug, Clone, PartialEq)]
pub struct QueueEntry {
    /// MusicKit's id for this item — the only stable handle on a row.
    pub id: String,
    pub title: String,
    pub artist: String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone)]
pub struct QueueItem {
    entry: QueueEntry,
    /// Who is playing, shared with every other row. Read at `bind`, never
    /// stored per-item — a per-item flag has to be changed in the model, and
    /// any model edit costs the scroll position (see `CurrentTrack`).
    current: CurrentTrack,
    /// Cloned into every item so a row's remove button has somewhere to send.
    /// `setup` is a static function with no sender, and the button must know
    /// *which* track it belongs to — which is only known at bind time.
    sender: relm4::Sender<QueueViewInput>,
    /// Where this row publishes its widgets while on screen, so the marker can
    /// move without editing the model. See `RowRegistry`.
    registry: RowRegistry<QueueRowWidgets>,
}

pub struct QueueItemWidgets {
    icon: gtk::Image,
    title: gtk::Label,
    artist: gtk::Label,
    duration: gtk::Label,
    remove: gtk::Button,
    /// The current click handler, disconnected on unbind. Widgets are recycled,
    /// so without this the handlers stack up and one click removes several
    /// unrelated tracks.
    handler: Option<gtk::glib::SignalHandlerId>,
}

impl RelmListItem for QueueItem {
    type Root = gtk::Box;
    type Widgets = QueueItemWidgets;

    fn setup(_item: &gtk::ListItem) -> (Self::Root, Self::Widgets) {
        view! {
            root = gtk::Box {
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

                    #[name = "title"]
                    gtk::Label {
                        set_xalign: 0.0,
                        set_ellipsize: gtk::pango::EllipsizeMode::End,
                        set_use_markup: false,
                    },

                    #[name = "artist"]
                    gtk::Label {
                        set_xalign: 0.0,
                        set_ellipsize: gtk::pango::EllipsizeMode::End,
                        set_use_markup: false,
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
                    // Must not take focus: removing the row then destroys the
                    // focused widget, and GTK scrolls to wherever focus lands —
                    // which is what sent the list back to the top.
                    set_focus_on_click: false,
                },
            }
        }

        (
            root,
            QueueItemWidgets {
                icon,
                title,
                artist,
                duration,
                remove,
                handler: None,
            },
        )
    }

    fn bind(&mut self, widgets: &mut Self::Widgets, _root: &mut Self::Root) {
        // Set everything: this widget was showing another track a moment ago,
        // and whatever is left unset keeps that track's value.
        widgets.title.set_label(&self.entry.title);
        widgets.artist.set_label(&self.entry.artist);
        widgets
            .duration
            .set_label(&format_duration(self.entry.duration_ms));

        let playing = self.current.borrow().as_deref() == Some(self.entry.id.as_str());
        apply_playing(&widgets.icon, &widgets.remove, playing);

        self.registry.borrow_mut().insert(
            self.entry.id.clone(),
            QueueRowWidgets {
                icon: widgets.icon.clone(),
                remove: widgets.remove.clone(),
            },
        );

        if let Some(old) = widgets.handler.take() {
            widgets.remove.disconnect(old);
        }
        let sender = self.sender.clone();
        let id = self.entry.id.clone();
        widgets.handler = Some(widgets.remove.connect_clicked(move |_| {
            sender.emit(QueueViewInput::Remove(id.clone()));
        }));
    }

    fn unbind(&mut self, widgets: &mut Self::Widgets, _root: &mut Self::Root) {
        // The widget is about to be handed to a different track; a stale
        // handler would remove this one instead.
        if let Some(old) = widgets.handler.take() {
            widgets.remove.disconnect(old);
        }
        self.registry.borrow_mut().remove(&self.entry.id);
    }
}

/// Paint a row's marker. Shared so the bind path and the live-update path
/// cannot drift apart.
fn apply_playing(icon: &gtk::Image, remove: &gtk::Button, playing: bool) {
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

// --- the sidebar ------------------------------------------------------------

pub struct QueueView {
    list: TypedListView<QueueItem, gtk::NoSelection>,
    count: usize,
    shown: Vec<String>,
    playing: Option<String>,
    registry: RowRegistry<QueueRowWidgets>,
    current: CurrentTrack,
}

#[derive(Debug)]
pub enum QueueViewInput {
    Sync {
        entries: Vec<QueueEntry>,
        playing: Option<String>,
    },
    /// Bring the current track into view — on open, not on every update, or it
    /// would fight the user scrolling.
    ScrollToPlaying,
    /// A row was activated. Carries a position into the visible model, which is
    /// resolved to an id immediately.
    Activated(u32),
    Remove(String),
}

#[derive(Debug)]
pub enum QueueViewOutput {
    /// The id of the track to act on. `app.rs` resolves it against MusicKit's
    /// live queue.
    Jump(String),
    Remove(String),
    /// Empty the queue and stop.
    Clear,
}

#[relm4::component(pub)]
impl Component for QueueView {
    type Init = ();
    type Input = QueueViewInput;
    type Output = QueueViewOutput;
    type CommandOutput = ();

    view! {
        adw::ToolbarView {

            add_top_bar = &adw::HeaderBar {
                // The queue is the rightmost pane while it is open, so the
                // window controls live here. The content header hides its own
                // when this is showing, so they never appear twice.
                set_show_end_title_buttons: true,

                #[wrap(Some)]
                set_title_widget = &adw::WindowTitle {
                    set_title: "Queue",
                    #[watch]
                    set_subtitle: &match model.count {
                        0 => String::new(),
                        1 => "1 track".to_owned(),
                        n => format!("{n} tracks"),
                    },
                },

                // Only when there is something to clear. A destructive action
                // that does nothing is worse than no button.
                pack_start = &gtk::Button {
                    set_icon_name: "user-trash-symbolic",
                    set_tooltip_text: Some("Clear the queue"),
                    add_css_class: "flat",
                    #[watch]
                    set_visible: model.count > 0,
                    connect_clicked[sender] => move |_| {
                        let _ = sender.output(QueueViewOutput::Clear);
                    },
                },
            },

            #[wrap(Some)]
            set_content = &gtk::Stack {
                // An empty queue is a state, not an empty list. A blank panel
                // reads as broken.
                add_named[Some("empty")] = &adw::StatusPage {
                    set_icon_name: Some("view-list-symbolic"),
                    set_title: "Nothing queued",
                    set_description: Some("Play something and it will show up here."),
                },

                #[name = "scroller"]
                add_named[Some("queue")] = &gtk::ScrolledWindow {
                    set_vexpand: true,

                    #[local_ref]
                    queue_list -> gtk::ListView {
                        set_single_click_activate: true,
                        add_css_class: "navigation-sidebar",
                    },
                },

                // After the children — naming one before it is added warns and
                // does nothing.
                #[watch]
                set_visible_child_name: if model.count == 0 { "empty" } else { "queue" },
            },
        }
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let list: TypedListView<QueueItem, gtk::NoSelection> = TypedListView::new();

        let activate = sender.clone();
        list.view.connect_activate(move |_, position| {
            activate.input(QueueViewInput::Activated(position));
        });

        let model = QueueView {
            list,
            count: 0,
            shown: Vec::new(),
            playing: None,
            registry: row_registry(),
            current: current_track(),
        };
        let queue_list = &model.list.view;
        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        msg: Self::Input,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match msg {
            QueueViewInput::Sync { entries, playing } => {
                // Moving the marker no longer touches the model, so the only
                // thing left that can disturb the scroll is a real structural
                // change — a removal, or a new queue. Capture the offset only
                // for those, and only restore it when one actually happened.
                //
                // (An earlier version restored on every sync, including marker
                // changes, and re-asserting an offset against an `upper` that
                // was still settling is what made the list twitch.)
                // Anchor on an ITEM, not on a pixel offset.
                //
                // Restoring the adjustment's value keeps losing: on a model
                // change `ListView` briefly reports a much smaller content
                // height and clamps `value` to near zero, and whatever we set
                // afterwards is clamped again. Two attempts at timing that were
                // both wrong.
                //
                // The topmost visible row is a stable thing to hold on to, and
                // `scroll_to` is ListView's own operation on it — no
                // adjustment arithmetic, no race with measurement.
                let anchor = self.topmost_visible();
                let structural = self.apply(entries, playing, sender.input_sender().clone());

                if structural
                    && let Some(anchor) = anchor
                    && let Some(position) = self.shown.iter().position(|id| *id == anchor)
                {
                    let list = widgets.queue_list.clone();
                    // Deferred one tick: the store has changed, but the view
                    // has not been through a layout pass yet.
                    gtk::glib::idle_add_local_once(move || {
                        list.scroll_to(position as u32, gtk::ListScrollFlags::NONE, None);
                    });
                }
            }
            QueueViewInput::ScrollToPlaying => {
                if let Some(index) = self.playing_index() {
                    widgets
                        .queue_list
                        .scroll_to(index as u32, gtk::ListScrollFlags::NONE, None);
                }
            }
            QueueViewInput::Activated(position) => {
                // Straight from position to id, before anything can change.
                if let Some(item) = self.list.get_visible(position) {
                    let id = item.borrow().entry.id.clone();
                    let _ = sender.output(QueueViewOutput::Jump(id));
                }
            }
            QueueViewInput::Remove(id) => {
                let _ = sender.output(QueueViewOutput::Remove(id));
            }
        }
        self.update_view(widgets, sender);
    }
}

impl QueueView {
    fn playing_index(&self) -> Option<usize> {
        let playing = self.playing.as_ref()?;
        self.shown.iter().position(|id| id == playing)
    }

    /// Bring the rows in line with `entries`. Returns whether the *structure*
    /// changed — a removal or a rebuild — as opposed to just the marker.
    ///
    /// Only a structural change can move the scroll: the marker is applied to
    /// widgets, never to the model.
    fn apply(
        &mut self,
        entries: Vec<QueueEntry>,
        playing: Option<String>,
        sender: relm4::Sender<QueueViewInput>,
    ) -> bool {
        let ids: Vec<String> = entries.iter().map(|e| e.id.clone()).collect();
        let structural = ids != self.shown;

        if structural {
            if let Some(removed) = single_removal(&self.shown, &ids) {
                tracing::debug!(index = removed, "queue: removing one row in place");
                self.list.remove(removed as u32);
            } else {
                tracing::debug!(
                    was = self.shown.len(),
                    now = ids.len(),
                    "queue: rebuilding rows"
                );
                let registry = self.registry.clone();
                // These rows are going away; none of their widgets are ours.
                registry.borrow_mut().clear();
                // Rows read the marker from here at bind time, so it only has
                // to be current before they are built.
                let current = self.current.clone();
                *current.borrow_mut() = playing.clone();
                self.list.clear();
                self.list
                    .extend_from_iter(entries.into_iter().map(|entry| QueueItem {
                        entry,
                        sender: sender.clone(),
                        registry: registry.clone(),
                        current: current.clone(),
                    }));
                self.shown = ids;
                self.count = self.shown.len();
                self.playing = playing;
                return true;
            }
            self.shown = ids;
            self.count = self.shown.len();
        }

        // Move the marker by replacing only the two rows it affects. Mutating
        // an item in place does nothing — the store emits no change, so the row
        // is never told to re-bind.
        if playing != self.playing {
            // The shared cell first, so any row bound from here on is right...
            *self.current.borrow_mut() = playing.clone();
            // ...then the two rows on screen right now, if they are.
            if let Some(old) = self.playing.take() {
                self.set_row_playing(&old, false);
            }
            if let Some(new) = &playing {
                self.set_row_playing(new, true);
            }
            self.playing = playing;
        }
        structural
    }

    /// The id of the topmost row currently on screen.
    ///
    /// The registry holds exactly the bound rows, so the smallest queue
    /// position among them is the row at the top of the viewport. That is the
    /// thing to keep still across an edit.
    fn topmost_visible(&self) -> Option<String> {
        let registry = self.registry.borrow();
        self.shown
            .iter()
            .find(|id| registry.contains_key(*id))
            .cloned()
    }

    /// Repaint one row's marker. Touches a widget, never the model.
    fn set_row_playing(&self, id: &str, playing: bool) {
        if let Some(w) = self.registry.borrow().get(id) {
            apply_playing(&w.icon, &w.remove, playing);
        }
    }
}

/// If `new` is `old` with exactly one element taken out, return its position.
///
/// Deliberately conservative: anything else — a reorder, an insertion, a whole
/// new queue — returns `None` and gets a rebuild. Being wrong here would
/// desynchronise the rows from the queue, which is worse than a scroll jump.
fn single_removal(old: &[String], new: &[String]) -> Option<usize> {
    if new.len() + 1 != old.len() {
        return None;
    }
    let at = old
        .iter()
        .zip(new.iter())
        .position(|(a, b)| a != b)
        .unwrap_or(new.len());
    (old[at + 1..] == new[at..]).then_some(at)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn detects_a_removal_from_the_middle() {
        assert_eq!(
            single_removal(&ids(&["a", "b", "c"]), &ids(&["a", "c"])),
            Some(1)
        );
    }

    #[test]
    fn detects_a_removal_from_either_end() {
        assert_eq!(
            single_removal(&ids(&["a", "b", "c"]), &ids(&["b", "c"])),
            Some(0)
        );
        assert_eq!(
            single_removal(&ids(&["a", "b", "c"]), &ids(&["a", "b"])),
            Some(2)
        );
    }

    #[test]
    fn a_reorder_is_not_a_removal() {
        assert_eq!(
            single_removal(&ids(&["a", "b", "c"]), &ids(&["c", "b", "a"])),
            None
        );
    }

    #[test]
    fn a_different_queue_of_length_minus_one_is_not_a_removal() {
        assert_eq!(
            single_removal(&ids(&["a", "b", "c"]), &ids(&["x", "y"])),
            None
        );
    }

    #[test]
    fn two_removals_fall_back_to_a_rebuild() {
        assert_eq!(
            single_removal(&ids(&["a", "b", "c", "d"]), &ids(&["a", "d"])),
            None
        );
    }
}
