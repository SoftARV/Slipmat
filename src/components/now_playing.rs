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

/// How long the slider must sit still before the seek is actually sent.
///
/// Dragging emits `change-value` continuously; seeking a DRM HLS stream on
/// every one of those would force a re-buffer per pixel. Waiting for a short
/// pause turns a drag into a single seek while still feeling immediate,
/// because the elapsed label moves with the handle straight away.
const SCRUB_COMMIT_MS: u64 = 250;

/// How close the sidecar's reported position must get to a seek target before
/// we believe the seek landed and start trusting its numbers again.
const SEEK_SETTLE_MS: u64 = 1_500;

/// How many snapshots to keep holding a seek target before giving up on it.
/// At the app's 500ms tick that is a few seconds — enough for a DRM re-buffer,
/// short enough that a seek which silently failed doesn't freeze the readout
/// on a position playback never reached.
const SEEK_SETTLE_TRIES: u8 = 12;

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
    /// True from the first slider movement until the debounce commits. State
    /// updates must not yank the handle out from under the user — the single
    /// most annoying bug a music player can have.
    scrubbing: bool,
    /// Bumped on every slider movement. Only the timer carrying the current
    /// generation is allowed to commit, which is how the debounce cancels
    /// earlier timers without juggling `SourceId`s (removing an already-fired
    /// source aborts the process).
    scrub_gen: u64,
    /// A seek we have sent but whose effect hasn't come back yet, plus how many
    /// more snapshots we'll hold it for. Without this the slider snaps back to
    /// the old position for the moment between committing a seek and the
    /// sidecar reporting the new one.
    pending_seek: Option<(u64, u8)>,
}

#[derive(Debug)]
pub enum NowPlayingInput {
    Sync(Box<Snapshot>),
    ArtworkReady(Option<PathBuf>),
    /// The slider moved, as a fraction 0.0–1.0 of the track.
    ScrubMoved(f64),
    /// The debounce elapsed. Carries the generation it was scheduled for, so a
    /// stale timer from earlier in the same drag is ignored rather than
    /// committing an outdated position.
    ScrubCommit(u64),
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

                    // `change-value` covers drags, keyboard steps and scroll,
                    // and is the ONLY input handler on this widget.
                    //
                    // Do not add a GestureClick here to detect drag start/end:
                    // GtkGestureClick claims the event sequence on press, which
                    // cancels GtkScale's own internal drag gesture and leaves
                    // the slider completely unscrubbable. The commit is
                    // debounced instead — see SCRUB_COMMIT_MS.
                    connect_change_value[sender] => move |_, _, value| {
                        sender.input(NowPlayingInput::ScrubMoved(value));
                        gtk::glib::Propagation::Proceed
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
            scrub_gen: 0,
            pending_seek: None,
        };
        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            NowPlayingInput::Sync(snap) => {
                let held = self.snap.position_ms;
                let incoming = snap.position_ms;
                self.snap = *snap;
                // Everything except the position comes straight from the
                // sidecar. The position is ours to defend while the user is
                // driving it — see `settle_position`.
                self.snap.position_ms = self.settle_position(held, incoming);
            }
            NowPlayingInput::ArtworkReady(path) => self.artwork = path,
            NowPlayingInput::ScrubMoved(fraction) => {
                let Some(target) = self.fraction_to_ms(fraction) else {
                    return;
                };
                // Move the label with the handle immediately — waiting for the
                // sidecar's echo makes a seek feel like it didn't register.
                self.snap.position_ms = target;
                self.scrubbing = true;
                self.scrub_gen = self.scrub_gen.wrapping_add(1);

                let generation = self.scrub_gen;
                let sender = sender.clone();
                gtk::glib::timeout_add_local_once(
                    std::time::Duration::from_millis(SCRUB_COMMIT_MS),
                    move || sender.input(NowPlayingInput::ScrubCommit(generation)),
                );
            }
            NowPlayingInput::ScrubCommit(generation) => {
                if self.should_commit(generation) {
                    self.scrubbing = false;
                    let target = self.snap.position_ms;
                    // Hold the target on screen until the sidecar's own
                    // position reports reach it, so the handle doesn't bounce
                    // back to where playback still is.
                    self.pending_seek = Some((target, SEEK_SETTLE_TRIES));
                    let _ = sender.output(NowPlayingOutput::Seek(target));
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
    /// Decide which position to display when a snapshot arrives.
    ///
    /// This is the fix for the bug where a seek during playback jumped back to
    /// where the track already was. `Sync` replaces the whole snapshot, so
    /// before this the sidecar's position — which streams in constantly while
    /// playing — overwrote the position the user was dragging to. By the time
    /// the debounce committed, it seeked to the *current* position instead of
    /// the chosen one. Pausing first happened to work only because a paused
    /// player sends no position events, so the dragged value survived.
    ///
    /// Precedence: the user's drag beats a pending seek, which beats the
    /// sidecar.
    fn settle_position(&mut self, held: u64, incoming: u64) -> u64 {
        if self.scrubbing {
            return held;
        }
        match self.pending_seek {
            Some((target, tries)) => {
                if incoming.abs_diff(target) <= SEEK_SETTLE_MS {
                    // The sidecar got there; hand control back.
                    self.pending_seek = None;
                    incoming
                } else if tries == 0 {
                    // Give up rather than show a position playback never
                    // reached — a silently failed seek must not freeze the bar.
                    self.pending_seek = None;
                    incoming
                } else {
                    self.pending_seek = Some((target, tries - 1));
                    target
                }
            }
            None => incoming,
        }
    }

    /// Whether a fired debounce timer is the one we're still waiting for.
    ///
    /// Anything older is a leftover from earlier in the same drag; committing
    /// it would seek to a position the user already moved away from.
    fn should_commit(&self, generation: u64) -> bool {
        generation == self.scrub_gen && self.scrubbing
    }

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
            scrub_gen: 0,
            pending_seek: None,
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

    /// The reported bug: seeking while playing jumped back to where the track
    /// already was, and only worked if you paused first. A playing sidecar
    /// streams position events, and each one used to overwrite the position the
    /// user was dragging to — so the commit seeked to the current position
    /// instead of the chosen one. Paused, no events arrive, so it "worked".
    #[test]
    fn a_position_update_mid_drag_does_not_steal_the_dragged_position() {
        let mut m = model(Snapshot {
            duration_ms: 200_000,
            ..Default::default()
        });
        m.snap.position_ms = 150_000; // where the user dragged to
        m.scrubbing = true;

        // The sidecar reports playback still down at 10s.
        let shown = m.settle_position(150_000, 10_000);

        assert_eq!(shown, 150_000, "the drag must win while it is happening");
    }

    #[test]
    fn a_committed_seek_is_held_until_the_sidecar_catches_up() {
        let mut m = model(Snapshot::default());
        m.pending_seek = Some((150_000, SEEK_SETTLE_TRIES));

        // Still reporting the old position: keep showing the target, no bounce.
        assert_eq!(m.settle_position(150_000, 10_000), 150_000);
        assert!(m.pending_seek.is_some());

        // Now it has arrived — hand control back to the sidecar.
        assert_eq!(m.settle_position(150_000, 150_400), 150_400);
        assert!(
            m.pending_seek.is_none(),
            "must stop overriding once settled"
        );
    }

    #[test]
    fn a_seek_that_never_lands_gives_up_rather_than_freezing() {
        let mut m = model(Snapshot::default());
        m.pending_seek = Some((150_000, 1));

        assert_eq!(m.settle_position(0, 10_000), 150_000, "one try left");
        // Out of tries: accept reality instead of showing a position playback
        // never reached.
        assert_eq!(m.settle_position(0, 10_500), 10_500);
        assert!(m.pending_seek.is_none());
    }

    #[test]
    fn without_a_drag_or_pending_seek_the_sidecar_wins() {
        let mut m = model(Snapshot::default());
        assert_eq!(m.settle_position(999, 42_000), 42_000);
    }

    #[test]
    fn only_the_newest_scrub_timer_commits() {
        let mut m = model(Snapshot {
            duration_ms: 200_000,
            ..Default::default()
        });
        m.scrubbing = true;
        m.scrub_gen = 7;

        assert!(m.should_commit(7), "the current generation commits");
        assert!(
            !m.should_commit(6),
            "a leftover timer must not seek to a position already moved away from"
        );
    }

    #[test]
    fn a_committed_scrub_does_not_commit_twice() {
        let mut m = model(Snapshot::default());
        m.scrub_gen = 3;
        m.scrubbing = false; // already committed
        assert!(!m.should_commit(3));
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
