// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! What's playing next — MusicKit's queue, shown natively.
//!
//! The indices here are **MusicKit's own**, straight from the queue events, not
//! positions we derived. That distinction is the whole lesson of M5: our list
//! and MusicKit's queue are not the same list, and anything computed against
//! ours is wrong the moment MusicKit drops or collapses an item. Jumping and
//! removing therefore address the queue as MusicKit reports it.

use relm4::adw::prelude::*;
use relm4::factory::{FactoryComponent, FactoryVecDeque};
use relm4::prelude::DynamicIndex;
use relm4::{Component, ComponentParts, ComponentSender, FactorySender, RelmWidgetExt, adw, gtk};

use crate::music::types::format_duration;

/// One queue entry, flattened from the sidecar's view of MusicKit's queue.
#[derive(Debug, Clone, PartialEq)]
pub struct QueueEntry {
    pub title: String,
    pub artist: String,
    pub duration_ms: u64,
    pub playing: bool,
}

// --- rows -------------------------------------------------------------------

#[derive(Debug)]
pub struct QueueRow {
    entry: QueueEntry,
    index: usize,
}

#[derive(Debug)]
pub enum QueueRowOutput {
    Jump(usize),
    Remove(usize),
}

#[relm4::factory(pub)]
impl FactoryComponent for QueueRow {
    type Init = (usize, QueueEntry);
    type Input = ();
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
                set_icon_name: Some(if self.entry.playing {
                    "media-playback-start-symbolic"
                } else {
                    "audio-x-generic-symbolic"
                }),
                set_css_classes: if self.entry.playing {
                    &["accent"]
                } else {
                    &["dim-label"]
                },
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
                // Removing the track you are listening to is a stop dressed up
                // as an edit; skip is the button for that.
                set_sensitive: !self.entry.playing,
                connect_clicked[sender, index = self.index] => move |_| {
                    sender.output(QueueRowOutput::Remove(index)).ok();
                },
            },

            connect_activated[sender, index = self.index] => move |_| {
                sender.output(QueueRowOutput::Jump(index)).ok();
            },
        }
    }

    fn init_model(init: Self::Init, _i: &DynamicIndex, _s: FactorySender<Self>) -> Self {
        Self {
            index: init.0,
            entry: init.1,
        }
    }
}

// --- the dialog -------------------------------------------------------------

pub struct QueueView {
    rows: FactoryVecDeque<QueueRow>,
    count: usize,
    /// What we last rendered, so a position tick or an unrelated event does not
    /// rebuild a list the user might be scrolling.
    last: Vec<QueueEntry>,
}

#[derive(Debug)]
pub enum QueueViewInput {
    Sync(Vec<QueueEntry>),
    Jump(usize),
    Remove(usize),
}

#[derive(Debug)]
pub enum QueueViewOutput {
    /// Index into **MusicKit's** queue.
    Jump(usize),
    Remove(usize),
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
                QueueRowOutput::Jump(i) => QueueViewInput::Jump(i),
                QueueRowOutput::Remove(i) => QueueViewInput::Remove(i),
            });

        let model = QueueView {
            rows,
            count: 0,
            last: Vec::new(),
        };
        let queue_list = model.rows.widget();
        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            QueueViewInput::Sync(entries) => {
                // The queue is pushed on every metadata change, most of which
                // do not touch it. Rebuilding regardless would fight anyone
                // scrolling a 500-track queue.
                if entries == self.last {
                    return;
                }
                self.last = entries.clone();
                self.count = entries.len();

                let mut rows = self.rows.guard();
                rows.clear();
                for (index, entry) in entries.into_iter().enumerate() {
                    rows.push_back((index, entry));
                }
            }
            QueueViewInput::Jump(index) => {
                let _ = sender.output(QueueViewOutput::Jump(index));
            }
            QueueViewInput::Remove(index) => {
                let _ = sender.output(QueueViewOutput::Remove(index));
            }
        }
    }
}
