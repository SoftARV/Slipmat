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

use crate::music::types::format_duration;

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
    playing: bool,
    /// Cloned into every item so a row's remove button has somewhere to send.
    /// `setup` is a static function with no sender, and the button must know
    /// *which* track it belongs to — which is only known at bind time.
    sender: relm4::Sender<QueueViewInput>,
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

        widgets.icon.set_icon_name(Some(if self.playing {
            "media-playback-start-symbolic"
        } else {
            "audio-x-generic-symbolic"
        }));
        widgets.icon.set_css_classes(if self.playing {
            &["accent"]
        } else {
            &["dim-label"]
        });

        // Removing the track you are listening to is a stop dressed up as an
        // edit; skip is the button for that.
        widgets.remove.set_sensitive(!self.playing);

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
    }
}

// --- the sidebar ------------------------------------------------------------

pub struct QueueView {
    list: TypedListView<QueueItem, gtk::NoSelection>,
    count: usize,
    shown: Vec<String>,
    playing: Option<String>,
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
}

#[relm4::component(pub)]
impl Component for QueueView {
    type Init = ();
    type Input = QueueViewInput;
    type Output = QueueViewOutput;
    type CommandOutput = ();

    view! {
        adw::ToolbarView {
            set_width_request: 340,

            add_top_bar = &adw::HeaderBar {
                set_show_end_title_buttons: false,

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
            },

            #[wrap(Some)]
            #[name = "scroller"]
            set_content = &gtk::ScrolledWindow {
                set_vexpand: true,

                #[local_ref]
                queue_list -> gtk::ListView {
                    set_single_click_activate: true,
                    add_css_class: "navigation-sidebar",
                },
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
                let scrolled_to = widgets.scroller.vadjustment().value();
                let changed = self.apply(entries, playing, sender.input_sender().clone());

                // Re-assert where the user was. A rebuild drops the scroll, and
                // guessing which mutations disturb it has been wrong before;
                // re-applying the same offset is a no-op when nothing moved it.
                if changed && scrolled_to > 0.0 {
                    let adj = widgets.scroller.vadjustment();
                    gtk::glib::idle_add_local_once(move || {
                        let max = (adj.upper() - adj.page_size()).max(0.0);
                        adj.set_value(scrolled_to.min(max));
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

    /// Bring the rows in line with `entries`. Returns whether anything changed.
    ///
    /// Rebuilding the store is far cheaper than it was: it holds data, not
    /// widgets, and `ListView` only materialises the handful of rows on screen.
    /// A single removal is still applied as a single removal, because that is
    /// the one case where the scroll position visibly matters.
    fn apply(
        &mut self,
        entries: Vec<QueueEntry>,
        playing: Option<String>,
        sender: relm4::Sender<QueueViewInput>,
    ) -> bool {
        let ids: Vec<String> = entries.iter().map(|e| e.id.clone()).collect();
        let same_tracks = ids == self.shown;
        let same_playing = playing == self.playing;

        if same_tracks && same_playing {
            return false;
        }
        self.playing = playing;

        if same_playing && let Some(removed) = single_removal(&self.shown, &ids) {
            tracing::debug!(index = removed, "queue: removing one row in place");
            self.shown = ids;
            self.count = entries.len();
            self.list.remove(removed as u32);
            return true;
        }

        tracing::debug!(
            was = self.shown.len(),
            now = ids.len(),
            "queue: rebuilding rows"
        );
        self.shown = ids;
        self.count = entries.len();
        let playing = self.playing.clone();
        self.list.clear();
        self.list
            .extend_from_iter(entries.into_iter().map(|entry| QueueItem {
                playing: Some(&entry.id) == playing.as_ref(),
                entry,
                sender: sender.clone(),
            }));
        true
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
