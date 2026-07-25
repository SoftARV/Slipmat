// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The persistent Now Playing bar — the thing that makes this an app rather
//! than a handshake.
//!
//! It owns no state of its own beyond what it is told: `app.rs` pushes a
//! `Snapshot` derived from `PlayerState` (which is itself a mirror of the
//! sidecar, rule 3), and this component renders it and emits intent back up.
//! It never talks to the sidecar directly.

use std::path::PathBuf;

use relm4::adw::prelude::*;
use relm4::{ComponentParts, ComponentSender, RelmWidgetExt, SimpleComponent, gtk};

use crate::music::types::format_duration;

/// Everything the bar needs, flattened out of `PlayerState` at the boundary.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Snapshot {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub playing: bool,
    pub busy: bool,
    pub has_next: bool,
    pub has_previous: bool,
    pub active: bool,
}

#[derive(Debug)]
pub struct NowPlaying {
    snap: Snapshot,
    artwork: Option<PathBuf>,
    volume: f64,
    /// True while the user is dragging the seek slider. State updates must not
    /// yank the handle out from under them — the single most annoying bug a
    /// music player can have.
    scrubbing: bool,
}

#[derive(Debug)]
pub enum NowPlayingInput {
    Sync(Box<Snapshot>),
    ArtworkReady(Option<PathBuf>),
    /// Pointer went down on the slider — stop syncing its value from state.
    ScrubStarted,
    /// The slider moved, as a fraction 0.0–1.0 of the track.
    ScrubMoved(f64),
    /// Pointer released — commit the seek.
    ScrubEnded,
    VolumeChanged(f64),
    PlayPause,
    Next,
    Previous,
}

#[derive(Debug)]
pub enum NowPlayingOutput {
    PlayPause,
    Next,
    Previous,
    Seek(u64),
    SetVolume(f64),
}

#[relm4::component(pub)]
impl SimpleComponent for NowPlaying {
    type Init = ();
    type Input = NowPlayingInput;
    type Output = NowPlayingOutput;

    view! {
        gtk::Box {
            set_orientation: gtk::Orientation::Horizontal,
            set_spacing: 12,
            set_margin_all: 10,
            #[watch]
            set_sensitive: model.snap.active,

            // --- artwork + labels ------------------------------------------
            #[name = "cover"]
            gtk::Image {
                set_pixel_size: 48,
                set_icon_name: Some("audio-x-generic-symbolic"),
                add_css_class: "np-cover",
            },

            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_valign: gtk::Align::Center,
                set_hexpand: true,
                set_spacing: 2,

                gtk::Label {
                    set_xalign: 0.0,
                    set_ellipsize: gtk::pango::EllipsizeMode::End,
                    add_css_class: "heading",
                    #[watch]
                    set_label: if model.snap.title.is_empty() {
                        "Nothing playing"
                    } else {
                        &model.snap.title
                    },
                },
                gtk::Label {
                    set_xalign: 0.0,
                    set_ellipsize: gtk::pango::EllipsizeMode::End,
                    add_css_class: "caption",
                    add_css_class: "dim-label",
                    #[watch]
                    set_label: &model.subtitle(),
                    #[watch]
                    set_visible: model.snap.active,
                },
            },

            // --- seek ------------------------------------------------------
            gtk::Box {
                set_orientation: gtk::Orientation::Horizontal,
                set_valign: gtk::Align::Center,
                set_spacing: 8,
                set_hexpand: true,

                #[name = "elapsed"]
                gtk::Label {
                    add_css_class: "numeric",
                    add_css_class: "caption",
                },

                #[name = "seek"]
                gtk::Scale {
                    set_hexpand: true,
                    set_width_request: 220,
                    set_draw_value: false,
                    set_range: (0.0, 1.0),
                    set_increments: (0.01, 0.1),

                    // `change-value` covers drags, keyboard steps and scroll.
                    // While dragging it only moves the label; the seek is sent
                    // on release, so a drag across the bar is one seek and not
                    // one per pixel.
                    connect_change_value[sender] => move |_, _, value| {
                        sender.input(NowPlayingInput::ScrubMoved(value));
                        gtk::glib::Propagation::Proceed
                    },

                    add_controller = gtk::GestureClick {
                        connect_pressed[sender] => move |_, _, _, _| {
                            sender.input(NowPlayingInput::ScrubStarted);
                        },
                        connect_released[sender] => move |_, _, _, _| {
                            sender.input(NowPlayingInput::ScrubEnded);
                        },
                    },
                },

                #[name = "total"]
                gtk::Label {
                    add_css_class: "numeric",
                    add_css_class: "caption",
                    add_css_class: "dim-label",
                },
            },

            // --- transport -------------------------------------------------
            gtk::Box {
                set_orientation: gtk::Orientation::Horizontal,
                set_valign: gtk::Align::Center,
                set_spacing: 4,

                gtk::Button {
                    set_icon_name: "media-skip-backward-symbolic",
                    set_tooltip_text: Some("Previous"),
                    add_css_class: "flat",
                    add_css_class: "circular",
                    #[watch]
                    set_sensitive: model.snap.has_previous,
                    connect_clicked => NowPlayingInput::Previous,
                },

                #[name = "play_button"]
                gtk::Button {
                    add_css_class: "circular",
                    add_css_class: "suggested-action",
                    #[watch]
                    set_icon_name: if model.snap.playing {
                        "media-playback-pause-symbolic"
                    } else {
                        "media-playback-start-symbolic"
                    },
                    #[watch]
                    set_tooltip_text: Some(if model.snap.playing { "Pause" } else { "Play" }),
                    connect_clicked => NowPlayingInput::PlayPause,
                },

                gtk::Button {
                    set_icon_name: "media-skip-forward-symbolic",
                    set_tooltip_text: Some("Next"),
                    add_css_class: "flat",
                    add_css_class: "circular",
                    #[watch]
                    set_sensitive: model.snap.has_next,
                    connect_clicked => NowPlayingInput::Next,
                },

                gtk::ScaleButton {
                    set_icons: &[
                        "audio-volume-muted-symbolic",
                        "audio-volume-high-symbolic",
                        "audio-volume-low-symbolic",
                        "audio-volume-medium-symbolic",
                    ],
                    set_tooltip_text: Some("Volume"),
                    add_css_class: "flat",
                    // ScaleButton is not a Range, so it takes an Adjustment
                    // rather than set_range. Page increment 0.1 makes scroll
                    // wheel steps feel right.
                    set_adjustment: &gtk::Adjustment::new(1.0, 0.0, 1.0, 0.05, 0.1, 0.0),
                    connect_value_changed[sender] => move |_, value| {
                        sender.input(NowPlayingInput::VolumeChanged(value));
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
        let model = NowPlaying {
            snap: Snapshot::default(),
            artwork: None,
            volume: 1.0,
            scrubbing: false,
        };
        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            NowPlayingInput::Sync(snap) => self.snap = *snap,
            NowPlayingInput::ArtworkReady(path) => self.artwork = path,
            NowPlayingInput::ScrubStarted => self.scrubbing = true,
            NowPlayingInput::ScrubMoved(fraction) => {
                let Some(target) = self.fraction_to_ms(fraction) else {
                    return;
                };
                // Move the label immediately either way — waiting for the
                // sidecar's echo makes a seek feel like it didn't register.
                self.snap.position_ms = target;
                // A keyboard step or scroll produces no press/release pair, so
                // there is no ScrubEnded coming: commit it now.
                if !self.scrubbing {
                    let _ = sender.output(NowPlayingOutput::Seek(target));
                }
            }
            NowPlayingInput::ScrubEnded => {
                if self.scrubbing {
                    self.scrubbing = false;
                    let _ = sender.output(NowPlayingOutput::Seek(self.snap.position_ms));
                }
            }
            NowPlayingInput::VolumeChanged(v) => {
                self.volume = v;
                let _ = sender.output(NowPlayingOutput::SetVolume(v));
            }
            NowPlayingInput::PlayPause => {
                let _ = sender.output(NowPlayingOutput::PlayPause);
            }
            NowPlayingInput::Next => {
                let _ = sender.output(NowPlayingOutput::Next);
            }
            NowPlayingInput::Previous => {
                let _ = sender.output(NowPlayingOutput::Previous);
            }
        }
    }

    /// The seek position and the cover are set here rather than with `#[watch]`
    /// because both need a condition the macro can't express: never move the
    /// slider while the user is dragging it, and only swap the image when the
    /// file actually changed.
    fn post_view(&self, widgets: &mut Self::Widgets) {
        if !self.scrubbing {
            widgets.seek.set_value(self.progress());
        }
        widgets.seek.set_sensitive(self.snap.duration_ms > 0);
        widgets
            .elapsed
            .set_label(&format_duration(self.snap.position_ms));
        widgets
            .total
            .set_label(&format_duration(self.snap.duration_ms));

        match &self.artwork {
            Some(path) => widgets.cover.set_from_file(Some(path)),
            None => widgets
                .cover
                .set_icon_name(Some("audio-x-generic-symbolic")),
        }
    }
}

impl NowPlaying {
    /// `None` for a track with no known length — seeking into nothing.
    fn fraction_to_ms(&self, fraction: f64) -> Option<u64> {
        (self.snap.duration_ms > 0)
            .then(|| (fraction.clamp(0.0, 1.0) * self.snap.duration_ms as f64) as u64)
    }

    fn progress(&self) -> f64 {
        if self.snap.duration_ms == 0 {
            return 0.0;
        }
        (self.snap.position_ms as f64 / self.snap.duration_ms as f64).clamp(0.0, 1.0)
    }

    /// `Artist — Album`, collapsing gracefully when either is missing rather
    /// than rendering a stray dash.
    fn subtitle(&self) -> String {
        match (self.snap.artist.is_empty(), self.snap.album.is_empty()) {
            (false, false) => format!("{} — {}", self.snap.artist, self.snap.album),
            (false, true) => self.snap.artist.clone(),
            (true, false) => self.snap.album.clone(),
            (true, true) => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(snap: Snapshot) -> NowPlaying {
        NowPlaying {
            snap,
            artwork: None,
            volume: 1.0,
            scrubbing: false,
        }
    }

    #[test]
    fn progress_handles_a_zero_length_track() {
        let m = model(Snapshot::default());
        assert_eq!(m.progress(), 0.0, "must not divide by zero");
    }

    #[test]
    fn progress_is_clamped_even_if_position_overruns() {
        let m = model(Snapshot {
            position_ms: 999_999,
            duration_ms: 1_000,
            ..Default::default()
        });
        assert_eq!(m.progress(), 1.0);
    }

    #[test]
    fn seeking_into_an_unknown_length_is_refused() {
        let m = model(Snapshot::default());
        assert_eq!(
            m.fraction_to_ms(0.5),
            None,
            "no duration, nothing to seek to"
        );
    }

    #[test]
    fn scrub_fractions_map_onto_the_track_and_clamp() {
        let m = model(Snapshot {
            duration_ms: 200_000,
            ..Default::default()
        });
        assert_eq!(m.fraction_to_ms(0.0), Some(0));
        assert_eq!(m.fraction_to_ms(0.5), Some(100_000));
        assert_eq!(m.fraction_to_ms(1.0), Some(200_000));
        // GTK can hand back slightly out-of-range values from a fast drag.
        assert_eq!(m.fraction_to_ms(-0.2), Some(0));
        assert_eq!(m.fraction_to_ms(1.7), Some(200_000));
    }

    #[test]
    fn subtitle_never_renders_a_dangling_dash() {
        let both = model(Snapshot {
            artist: "Yes".into(),
            album: "Fragile".into(),
            ..Default::default()
        });
        assert_eq!(both.subtitle(), "Yes — Fragile");

        let artist_only = model(Snapshot {
            artist: "Yes".into(),
            ..Default::default()
        });
        assert_eq!(artist_only.subtitle(), "Yes");

        let album_only = model(Snapshot {
            album: "Fragile".into(),
            ..Default::default()
        });
        assert_eq!(album_only.subtitle(), "Fragile");

        assert_eq!(model(Snapshot::default()).subtitle(), "");
    }
}
