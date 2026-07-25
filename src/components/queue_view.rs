// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! What's playing next — MusicKit's queue, shown natively.
//!
//! Two rules here, both learned the hard way in M5 and both re-learned in the
//! first version of this file:
//!
//! 1. **Address MusicKit's queue, never a position we derived.** Its queue and
//!    our library list are not the same list.
//! 2. **Identify a track by its id, not its index.** An index is only valid
//!    until the queue changes, and the queue changes under you — removing an
//!    item shifts everything below it. The row that is playing is the row whose
//!    *id* matches, and the row you click is whatever it has *become*, which is
//!    what `DynamicIndex` is for.

use relm4::adw::prelude::*;
use relm4::factory::{FactoryComponent, FactoryVecDeque};
use relm4::prelude::DynamicIndex;
use relm4::{Component, ComponentParts, ComponentSender, FactorySender, RelmWidgetExt, adw, gtk};

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

// --- rows -------------------------------------------------------------------

#[derive(Debug)]
pub struct QueueRow {
    entry: QueueEntry,
    playing: bool,
}

#[derive(Debug, Clone)]
pub enum QueueRowInput {
    /// The id of the track MusicKit is currently on.
    NowPlaying(Option<String>),
}

/// Both carry the track's **id**, not its row position. The row knows which
/// track it is; only `app.rs` can say where that track currently sits in
/// MusicKit's queue, and only at the moment the command is sent.
#[derive(Debug)]
pub enum QueueRowOutput {
    Jump(String),
    Remove(String),
}

#[relm4::factory(pub)]
impl FactoryComponent for QueueRow {
    type Init = QueueEntry;
    type Input = QueueRowInput;
    type Output = QueueRowOutput;
    type CommandOutput = ();
    type ParentWidget = gtk::ListBox;

    view! {
        adw::ActionRow {
            // Plain text, not markup — "Blood, Sweat & 3 Years" must render.
            set_use_markup: false,
            set_title: &self.entry.title,
            set_subtitle: &self.entry.artist,
            set_activatable: true,

            add_prefix = &gtk::Image {
                set_pixel_size: 16,
                #[watch]
                set_icon_name: Some(if self.playing {
                    "media-playback-start-symbolic"
                } else {
                    "audio-x-generic-symbolic"
                }),
                #[watch]
                set_css_classes: if self.playing { &["accent"] } else { &["dim-label"] },
            },

            add_suffix = &gtk::Label {
                set_label: &format_duration(self.entry.duration_ms),
                add_css_class: "numeric",
                add_css_class: "dim-label",
            },

            add_suffix = &gtk::Button {
                set_icon_name: "list-remove-symbolic",
                set_tooltip_text: Some("Remove from queue"),
                set_valign: gtk::Align::Center,
                add_css_class: "flat",
                add_css_class: "circular",
                // Do NOT take focus on click. Clicking focuses the button, and
                // removing the row then destroys the focused widget — GTK moves
                // focus to the first focusable row and the ScrolledWindow
                // scrolls to reveal it, which is why removing a track jumped
                // the list to the top. Still reachable by Tab.
                set_focus_on_click: false,
                // Removing the track you are listening to is a stop dressed up
                // as an edit; skip is the button for that.
                #[watch]
                set_sensitive: !self.playing,
                connect_clicked[sender, id = self.entry.id.clone()] => move |_| {
                    sender.output(QueueRowOutput::Remove(id.clone())).ok();
                },
            },

            connect_activated[sender, id = self.entry.id.clone()] => move |_| {
                sender.output(QueueRowOutput::Jump(id.clone())).ok();
            },
        }
    }

    fn init_model(entry: Self::Init, _index: &DynamicIndex, _s: FactorySender<Self>) -> Self {
        Self {
            entry,
            playing: false,
        }
    }

    fn update(&mut self, msg: Self::Input, _sender: FactorySender<Self>) {
        match msg {
            QueueRowInput::NowPlaying(id) => {
                self.playing = id.is_some_and(|id| id == self.entry.id);
            }
        }
    }
}

// --- the dialog -------------------------------------------------------------

pub struct QueueView {
    rows: FactoryVecDeque<QueueRow>,
    count: usize,
    /// Ids of what is on screen, so a change can be applied as an edit rather
    /// than a rebuild.
    shown: Vec<String>,
    playing: Option<String>,
}

#[derive(Debug)]
pub enum QueueViewInput {
    Sync {
        entries: Vec<QueueEntry>,
        playing: Option<String>,
    },
    /// Bring the current track into view — done when the dialog opens, not on
    /// every update, or it would fight the user scrolling.
    ScrollToPlaying,
    Jump(String),
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
        adw::Dialog {
            set_title: "Queue",
            set_content_width: 480,
            set_content_height: 620,

            #[wrap(Some)]
            set_child = &adw::ToolbarView {
                add_top_bar = &adw::HeaderBar {
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

                    #[wrap(Some)]
                    set_child = &adw::Clamp {
                        #[local_ref]
                        queue_list -> gtk::ListBox {
                            set_selection_mode: gtk::SelectionMode::None,
                            set_valign: gtk::Align::Start,
                            set_margin_all: 12,
                            add_css_class: "boxed-list",
                        },
                    },
                },
            },
        }
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let rows = FactoryVecDeque::builder()
            .launch(gtk::ListBox::default())
            .forward(sender.input_sender(), |out| match out {
                QueueRowOutput::Jump(id) => QueueViewInput::Jump(id),
                QueueRowOutput::Remove(id) => QueueViewInput::Remove(id),
            });

        let model = QueueView {
            rows,
            count: 0,
            shown: Vec::new(),
            playing: None,
        };
        let queue_list = model.rows.widget();
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
                // Where the user is looking, captured before anything changes.
                let scrolled_to = widgets.scroller.vadjustment().value();

                if self.apply(entries) != Applied::Unchanged && scrolled_to > 0.0 {
                    // Restore after ANY change, not just a rebuild. Removing a
                    // row moved the scroll too — via focus, not via rebuilding
                    // — and guessing which mutations disturb it has already
                    // been wrong once. Re-asserting the offset the user was at
                    // is a no-op when nothing moved it.
                    //
                    // Deferred, because rows have no height until they have
                    // been allocated and the adjustment's `upper` is stale
                    // until then.
                    let adj = widgets.scroller.vadjustment();
                    gtk::glib::idle_add_local_once(move || {
                        let max = (adj.upper() - adj.page_size()).max(0.0);
                        adj.set_value(scrolled_to.min(max));
                    });
                }

                if playing != self.playing {
                    self.playing = playing.clone();
                    self.rows.broadcast(QueueRowInput::NowPlaying(playing));
                }
            }
            QueueViewInput::ScrollToPlaying => {
                let Some(index) = self.playing_index() else {
                    return;
                };
                // Deferred: when the dialog is first presented the rows have
                // not been allocated yet, so their positions are all zero.
                let list = widgets.queue_list.clone();
                let scroller = widgets.scroller.clone();
                gtk::glib::idle_add_local_once(move || {
                    if let Some(row) = list.row_at_index(index as i32) {
                        // `allocation()` and `translate_coordinates()` are
                        // both deprecated since GTK 4.12; `compute_bounds` is
                        // the supported way to ask where a child sits.
                        let y = row
                            .compute_bounds(&list)
                            .map(|bounds| f64::from(bounds.y()))
                            .unwrap_or(0.0);
                        let adj = scroller.vadjustment();
                        // A third of a page above, so the track has context
                        // rather than sitting jammed against the top edge.
                        adj.set_value((y - adj.page_size() / 3.0).max(0.0));
                    }
                });
            }
            QueueViewInput::Jump(id) => {
                let _ = sender.output(QueueViewOutput::Jump(id));
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

    /// Bring the rows in line with `entries`, editing rather than rebuilding
    /// wherever possible.
    ///
    /// A rebuild resets the scroll position, which is unacceptable for the
    /// commonest change by far — removing one track, while looking at the queue.
    /// So a single removal is applied as a single removal.
    fn apply(&mut self, entries: Vec<QueueEntry>) -> Applied {
        let ids: Vec<String> = entries.iter().map(|e| e.id.clone()).collect();
        if ids == self.shown {
            return Applied::Unchanged;
        }

        if let Some(removed) = single_removal(&self.shown, &ids) {
            tracing::debug!(index = removed, "queue: removing one row in place");
            self.shown = ids;
            self.count = entries.len();
            self.rows.guard().remove(removed);
            return Applied::Edited;
        }

        // Worth a line: if removals keep landing here rather than on the fast
        // path, the queue MusicKit reports after an edit is not simply the old
        // one minus an item, and the fast path needs widening.
        tracing::debug!(
            was = self.shown.len(),
            now = ids.len(),
            "queue: rebuilding rows"
        );
        self.shown = ids;
        self.count = entries.len();
        let mut rows = self.rows.guard();
        rows.clear();
        for entry in entries {
            rows.push_back(entry);
        }
        Applied::Rebuilt
    }
}

/// What `apply` did, so the caller knows whether the scroll needs restoring.
#[derive(Debug, PartialEq, Eq)]
enum Applied {
    Unchanged,
    /// Rows edited in place; scroll position survives.
    Edited,
    /// Every row recreated; scroll position is gone.
    Rebuilt,
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
    // Everything after the removed element must line up, or this is not a
    // simple removal.
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
        // Same length, so this must not be mistaken for an edit.
        assert_eq!(
            single_removal(&ids(&["a", "b", "c"]), &ids(&["c", "b", "a"])),
            None
        );
    }

    #[test]
    fn a_different_queue_of_length_minus_one_is_not_a_removal() {
        // Correct length, wrong contents — applying this as a removal would
        // leave the rows out of step with the real queue.
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
