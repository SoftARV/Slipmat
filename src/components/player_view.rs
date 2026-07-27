// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The expanded player: the Now Playing bar opened out into a drawer.
//!
//! A separate component from [`super::now_playing`] rather than a second mode
//! of it, for two reasons. That file is already at its size budget and this is
//! not a small view; and the two are genuinely different shapes — the bar is a
//! strip that must survive being 400px wide, this is a page that assumes room.
//!
//! What they share is deliberate: the same [`Snapshot`] in, the same
//! [`NowPlayingOutput`] out. The transport here cannot drift from the
//! transport there, because they are the same messages handled by the same
//! reducer arms. Anything else would be two players disagreeing about one
//! MusicKit.

use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;
use relm4::prelude::*;

use super::cover::Cover;
use super::now_playing::{NowPlayingOutput, Repeat, Snapshot};
use crate::music::types::format_duration;

pub struct PlayerView {
    snap: Snapshot,
    cover: Cover,
    /// True while the user is dragging the scrubber, so incoming positions do
    /// not yank the handle out from under them — the same rule the bar follows.
    scrubbing: bool,
    /// Bumped per drag movement; only the newest commit is honoured.
    scrub_gen: u64,
}

/// How long the scrubber waits after the last movement before seeking.
const SCRUB_COMMIT_MS: u64 = 250;

#[derive(Debug)]
pub enum PlayerViewInput {
    Sync(Box<Snapshot>),
    Artwork(Option<std::path::PathBuf>),
    Scrub(f64),
    /// Only the newest scrub commits — the same generation trick the bar's seek
    /// uses, and for the same reason: dragging emits continuously and every
    /// intermediate value would be a seek MusicKit has to service.
    ScrubDone(u64, f64),
    PlayPause,
    Next,
    Previous,
    Shuffle(bool),
    /// Cycle to the next repeat mode. No payload: the mirror says what is
    /// current, and this view must not have an opinion of its own (rule 3).
    RepeatCycle,
}

#[relm4::component(pub)]
impl SimpleComponent for PlayerView {
    type Init = ();
    type Input = PlayerViewInput;
    type Output = NowPlayingOutput;

    view! {
        #[name = "root"]
        gtk::Box {
            set_orientation: gtk::Orientation::Horizontal,
            add_css_class: "np-sheet",
            set_vexpand: true,

            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_halign: gtk::Align::Center,
                set_valign: gtk::Align::Center,
                set_hexpand: true,
                set_spacing: 24,
                set_margin_all: 32,

                #[name = "art_slot"]
                gtk::Box {
                    set_halign: gtk::Align::Center,
                },

                gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_halign: gtk::Align::Center,
                    set_spacing: 4,

                    gtk::Label {
                        add_css_class: "title-1",
                        set_ellipsize: gtk::pango::EllipsizeMode::End,
                        set_max_width_chars: 34,
                        set_use_markup: false,
                        #[watch]
                        set_label: &model.snap.title,
                    },
                    gtk::Label {
                        add_css_class: "title-4",
                        add_css_class: "dim-label",
                        set_ellipsize: gtk::pango::EllipsizeMode::End,
                        set_max_width_chars: 40,
                        set_use_markup: false,
                        #[watch]
                        set_label: &model.subtitle(),
                    },
                },

                gtk::Box {
                    set_spacing: 10,
                    set_halign: gtk::Align::Center,
                    set_hexpand: true,

                    gtk::Label {
                        add_css_class: "numeric",
                        add_css_class: "caption",
                        #[watch]
                        set_label: &format_duration(model.snap.position_ms),
                    },

                    #[name = "scrubber"]
                    gtk::Scale {
                        set_hexpand: true,
                        set_draw_value: false,
                        #[watch]
                        set_range: (0.0, model.snap.duration_ms.max(1) as f64),
                        #[watch]
                        set_sensitive: model.snap.duration_ms > 0,
                        connect_change_value[sender] => move |_, _, v| {
                            sender.input(PlayerViewInput::Scrub(v));
                            gtk::glib::Propagation::Proceed
                        },
                    },

                    gtk::Label {
                        add_css_class: "numeric",
                        add_css_class: "caption",
                        #[watch]
                        set_label: &format_duration(
                            model.snap.duration_ms.saturating_sub(model.snap.position_ms),
                        ),
                    },
                },

                gtk::Box {
                    set_spacing: 12,
                    set_halign: gtk::Align::Center,

                    gtk::ToggleButton {
                        set_icon_name: "media-playlist-shuffle-symbolic",
                        set_tooltip_text: Some("Shuffle"),
                        add_css_class: "flat",
                        add_css_class: "circular",
                        #[watch]
                        set_active: model.snap.shuffle,
                        connect_toggled[sender] => move |b| {
                            sender.input(PlayerViewInput::Shuffle(b.is_active()));
                        },
                    },

                    gtk::Button {
                        set_icon_name: "media-skip-backward-symbolic",
                        add_css_class: "flat",
                        add_css_class: "circular",
                        #[watch]
                        set_sensitive: model.snap.has_previous,
                        connect_clicked[sender] => move |_| {
                            sender.input(PlayerViewInput::Previous);
                        },
                    },

                    gtk::Button {
                        add_css_class: "suggested-action",
                        add_css_class: "circular",
                        set_width_request: 56,
                        set_height_request: 56,
                        #[watch]
                        set_icon_name: if model.snap.playing {
                            "media-playback-pause-symbolic"
                        } else {
                            "media-playback-start-symbolic"
                        },
                        #[watch]
                        set_sensitive: model.snap.active,
                        connect_clicked[sender] => move |_| {
                            sender.input(PlayerViewInput::PlayPause);
                        },
                    },

                    gtk::Button {
                        set_icon_name: "media-skip-forward-symbolic",
                        add_css_class: "flat",
                        add_css_class: "circular",
                        #[watch]
                        set_sensitive: model.snap.has_next,
                        connect_clicked[sender] => move |_| {
                            sender.input(PlayerViewInput::Next);
                        },
                    },

                    gtk::Button {
                        add_css_class: "flat",
                        add_css_class: "circular",
                        set_tooltip_text: Some("Repeat"),
                        #[watch]
                        set_icon_name: match model.snap.repeat {
                            Repeat::One => "media-playlist-repeat-song-symbolic",
                            _ => "media-playlist-repeat-symbolic",
                        },
                        #[watch]
                        set_opacity: if matches!(model.snap.repeat, Repeat::Off) { 0.5 } else { 1.0 },
                        connect_clicked[sender] => move |_| {
                            sender.input(PlayerViewInput::RepeatCycle);
                        },
                    },
                },
            },

            gtk::Separator { set_orientation: gtk::Orientation::Vertical },

            #[local_ref]
            queue -> adw::ToolbarView {
                set_width_request: 360,
            },
        }
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let model = PlayerView {
            snap: Snapshot::default(),
            cover: Cover::new(240),
            scrubbing: false,
            scrub_gen: 0,
        };
        let queue = QUEUE_SLOT
            .with(|q| q.borrow().clone())
            .expect("the queue widget must be handed over before the player view is built");
        let widgets = view_output!();
        model.cover.attach_first(&widgets.art_slot);
        model.cover.square("audio-x-generic-symbolic");
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            PlayerViewInput::Sync(snap) => {
                // While dragging, the position is the user's, not the player's.
                let position = if self.scrubbing {
                    self.snap.position_ms
                } else {
                    snap.position_ms
                };
                self.snap = *snap;
                self.snap.position_ms = position;
            }
            PlayerViewInput::Artwork(path) => match path {
                Some(path) => self.cover.set_file(&path),
                None => self.cover.square("audio-x-generic-symbolic"),
            },
            PlayerViewInput::Scrub(v) => {
                self.scrubbing = true;
                self.snap.position_ms = v as u64;
                // Debounced: a drag emits on every motion event, and seeking on
                // each one would have MusicKit re-buffering continuously.
                self.scrub_gen = self.scrub_gen.wrapping_add(1);
                let generation = self.scrub_gen;
                let sender = sender.clone();
                gtk::glib::timeout_add_local_once(
                    std::time::Duration::from_millis(SCRUB_COMMIT_MS),
                    move || sender.input(PlayerViewInput::ScrubDone(generation, v)),
                );
            }
            PlayerViewInput::ScrubDone(generation, v) => {
                // A later drag supersedes this one.
                if generation != self.scrub_gen {
                    return;
                }
                self.scrubbing = false;
                let _ = sender.output(NowPlayingOutput::Seek(v as u64));
            }
            PlayerViewInput::PlayPause => {
                let _ = sender.output(NowPlayingOutput::PlayPause);
            }
            PlayerViewInput::Next => {
                let _ = sender.output(NowPlayingOutput::Next);
            }
            PlayerViewInput::Previous => {
                let _ = sender.output(NowPlayingOutput::Previous);
            }
            PlayerViewInput::Shuffle(on) => {
                let _ = sender.output(NowPlayingOutput::SetShuffle(on));
            }
            PlayerViewInput::RepeatCycle => {
                // Cycles through the three modes; the mirror decides what is
                // next, exactly as the bar's button does.
                let next = match self.snap.repeat {
                    Repeat::Off => Repeat::All,
                    Repeat::All => Repeat::One,
                    Repeat::One => Repeat::Off,
                };
                let _ = sender.output(NowPlayingOutput::SetRepeat(next));
            }
        }
    }
}

thread_local! {
    /// Where the queue widget is left for `init` to collect.
    ///
    /// relm4's `view!` builds the widget tree before the model exists, and the
    /// queue is a sibling component owned by the app — there is no init payload
    /// that can carry a `&Widget` through. Handing it over on this cell keeps
    /// the queue a *moved* component rather than a second implementation, which
    /// is what issue #18 asked for.
    static QUEUE_SLOT: std::cell::RefCell<Option<adw::ToolbarView>> =
        const { std::cell::RefCell::new(None) };
}

/// Lend the queue widget to the player view being built next.
pub fn hand_over_queue(queue: adw::ToolbarView) {
    QUEUE_SLOT.with(|q| *q.borrow_mut() = Some(queue));
}

impl PlayerView {
    fn subtitle(&self) -> String {
        match (self.snap.artist.is_empty(), self.snap.album.is_empty()) {
            (false, false) => format!("{} — {}", self.snap.artist, self.snap.album),
            (false, true) => self.snap.artist.clone(),
            (true, false) => self.snap.album.clone(),
            (true, true) => String::new(),
        }
    }
}
