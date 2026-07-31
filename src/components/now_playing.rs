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
use std::time::Duration;

use relm4::adw::prelude::*;
use relm4::{ComponentParts, ComponentSender, SimpleComponent, gtk};

use super::cover::SWAP_MS;
use crate::music::types::format_duration;

/// How often the slider redraws itself between snapshots.
///
/// The app pushes a snapshot twice a second, which moved the slider in visible
/// steps. Rather than push ten a second — every one of which also rebuilds
/// strings, updates the queue sidebar and writes MPRIS properties over D-Bus —
/// the bar advances its own display between them. Nothing else is recomputed.
const ADVANCE_MS: u64 = 100;

/// How far the slider may be allowed to run ahead of a correction before it
/// gives in and jumps back. Below this it holds still and lets the real
/// position catch up; above it, something discontinuous happened.
const BACKSTEP_TOLERANCE_MS: u64 = 1_500;

/// How big the disc is drawn inside the empty sleeve. The sleeve itself stays
/// 48px; this is what leaves a margin inside it.
const EMPTY_COVER_PX: i32 = 22;

/// How a mode button reads when its mode is off.
///
/// Shuffle and repeat are not transport buttons — they change what "next"
/// *means* rather than doing anything now — so they say on or off by weight
/// rather than by the pressed-in circle a `GtkToggleButton` draws, and the
/// icon says which flavour of on. Shared with the drawer and the queue header
/// so the same control cannot read two ways in one app.
pub fn mode_opacity(on: bool) -> f64 {
    if on { 1.0 } else { 0.45 }
}

/// How much one keyboard press or one scale step moves the volume.
///
/// Shared by the volume button's adjustment and the `Ctrl`+`Up`/`Down`
/// accelerators, so the two cannot drift into disagreeing about what a step is.
pub const VOLUME_STEP: f64 = 0.05;

/// Everything the bar needs, flattened out of `PlayerState` at the boundary.
#[derive(Debug, Clone, PartialEq)]
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
    pub shuffle: bool,
    /// Whether the queue sidebar is open, so the bar's toggle agrees with it.
    pub queue_open: bool,
    /// Mirrored from MusicKit, never authored here (rule 3).
    pub repeat: Repeat,
    /// 0.0–1.0. The volume button follows this rather than owning it, so a
    /// change from the keyboard or from MPRIS moves the widget too.
    pub volume: f64,
    /// The window is too narrow to carry the whole bar.
    ///
    /// Set from the same breakpoint that turns the header's search entry into a
    /// button. The bar answers by standing down shuffle, repeat and volume —
    /// all three of which the open drawer still has, so nothing becomes
    /// unreachable, it just stops being reachable from a strip with no room
    /// for it.
    pub narrow: bool,
}

impl Default for Snapshot {
    /// Hand-written for one field: **volume defaults to full, not to zero.**
    ///
    /// A derived `Default` starts every field at its zero value, and the bar is
    /// built with a default snapshot before the first real one arrives — so a
    /// zero here draws a muted button on a player that is not muted, until the
    /// first snapshot lands.
    ///
    /// It used to be worse than cosmetic: the write was unguarded, so the zero
    /// went back out as a genuine `SetVolume(0.0)` and the app muted itself on
    /// launch. `post_view` silences the handler now, so the write no longer
    /// escapes — but a button that opens on silence is still wrong.
    fn default() -> Self {
        Self {
            narrow: false,
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            position_ms: 0,
            duration_ms: 0,
            playing: false,
            busy: false,
            has_next: false,
            has_previous: false,
            active: false,
            shuffle: false,
            queue_open: false,
            repeat: Repeat::default(),
            volume: 1.0,
        }
    }
}

/// What the repeat button is showing. Ours, not MusicKit's — `protocol` owns
/// the wire type and `components/` never sees it (rule 9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Repeat {
    #[default]
    Off,
    All,
    One,
}

impl Repeat {
    /// What clicking the button does: off → all → one → off. The order the
    /// GNOME music apps use, and the one Apple Music itself uses.
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::All,
            Self::All => Self::One,
            Self::One => Self::Off,
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Off | Self::All => "media-playlist-repeat-symbolic",
            Self::One => "media-playlist-repeat-song-symbolic",
        }
    }

    fn tooltip(self) -> &'static str {
        match self {
            Self::Off => "Repeat: off",
            Self::All => "Repeat: all",
            Self::One => "Repeat: this track",
        }
    }
}

#[derive(Debug)]
pub struct NowPlaying {
    snap: Snapshot,
    artwork: Option<PathBuf>,
    /// What the cover widget is actually displaying, so `post_view` can tell a
    /// real change from the many redraws that are not one.
    shown_artwork: std::cell::RefCell<Option<PathBuf>>,
    /// When the last snapshot arrived, so the position can be carried forward
    /// between them. `None` while paused — a paused player's position is a
    /// fact, not something to extrapolate from.
    synced_at: Option<std::time::Instant>,
    /// The last position actually drawn, so the slider can be kept monotonic.
    shown_ms: std::cell::Cell<u64>,
    /// Drives [`ADVANCE_MS`]. Removed the moment playback stops, so a paused
    /// app is not waking up ten times a second.
    advance: Option<gtk::glib::SourceId>,
    /// The volume button's `value-changed` handler, so `post_view` can silence
    /// it while writing to the button. See the note there — without this the
    /// bar's own write comes back as a message and feeds itself.
    volume_handler: std::cell::RefCell<Option<gtk::glib::SignalHandlerId>>,
}

#[derive(Debug)]
pub enum NowPlayingInput {
    Sync(Box<Snapshot>),
    ArtworkReady(Option<PathBuf>),
    VolumeChanged(f64),
    PlayPause,
    Next,
    Previous,
    /// Redraw the slider from the interpolated position. Carries nothing and
    /// touches nothing but the two widgets that show time.
    Advance,
    ShuffleClicked,
    RepeatCycled,
    /// The queue button was clicked. Carries nothing: the app owns whether the
    /// sidebar is open, and the button follows it rather than leading.
    QueueToggled,
}

#[derive(Debug)]
pub enum NowPlayingOutput {
    PlayPause,
    Next,
    Previous,
    Seek(u64),
    SetVolume(f64),
    SetShuffle(bool),
    SetRepeat(Repeat),
    ToggleQueue,
}

#[relm4::component(pub)]
impl SimpleComponent for NowPlaying {
    type Init = ();
    type Input = NowPlayingInput;
    type Output = NowPlayingOutput;

    view! {
        gtk::Box {
            set_orientation: gtk::Orientation::Vertical,
            add_css_class: "np-bar",

            // The track's progress, across the whole width of the bar.
            //
            // **Information, not a control.** It was a `GtkScale`, and a scale
            // needs a grabbable handle and room to aim in — which is exactly
            // what the bar runs out of first when the window is tiled narrow.
            // Reading it does not need any of that, and the drawer is where
            // scrubbing belongs: it has the width, and its scrubber is
            // already clamped to a size you can hit.
            //
            // Outside the padded row on purpose, so the line reaches both
            // edges rather than stopping short of them.
            #[name = "progress"]
            gtk::ProgressBar {
                add_css_class: "np-progress",
                #[watch]
                set_fraction: model.progress(),
                #[watch]
                set_visible: model.snap.duration_ms > 0,
            },

            gtk::Box {
                set_orientation: gtk::Orientation::Horizontal,
                set_spacing: 12,
                // Padding, not margin — the spacing is in `.np-row`'s CSS.
                //
                // A margin sits *outside* the widget's background, so a 10px
                // one left an untinted frame around the whole bar once it had
                // a cover behind it. Padding is inside it, and the backdrop
                // reaches the edges.
                add_css_class: "np-row",
            // Deliberately no blanket `set_sensitive` here. Greying the whole
            // bar when nothing is playing also greyed the queue button, so you
            // could not open the queue to start something — the one moment you
            // most need it. Each control gates itself instead.

            // --- artwork + labels ------------------------------------------
            // Two pages, cross-faded, rather than one image whose contents
            // are swapped underneath you. A cover arriving or going away is a
            // change of state and now dissolves like one; `Cover` does the
            // same thing in the drawer, and they share `SWAP_MS` so the two
            // surfaces move together.
            #[name = "cover_stack"]
            gtk::Stack {
                set_transition_type: gtk::StackTransitionType::Crossfade,
                set_transition_duration: SWAP_MS,
                set_valign: gtk::Align::Center,

                #[name = "cover"]
                add_named[Some("cover")] = &gtk::Image {
                    set_pixel_size: 48,
                    set_size_request: (48, 48),
                    add_css_class: "np-cover",
                },

                // An empty sleeve rather than a floating icon: with nothing
                // playing, the bar should still read as having a place where
                // the artwork goes. The *widget* stays 48px so the case does
                // not change size; the disc inside it is drawn smaller so it
                // sits within the sleeve rather than against its edges.
                add_named[Some("sleeve")] = &gtk::Image {
                    set_pixel_size: EMPTY_COVER_PX,
                    set_size_request: (48, 48),
                    set_icon_name: Some("media-optical-symbolic"),
                    add_css_class: "np-cover",
                    add_css_class: "np-cover-empty",
                },

                // **After the children**, or naming one before it is added
                // warns and does nothing.
                //
                // A `GtkStack` shows whichever child was added first, and that
                // is the cover — an image with no file and no icon, which
                // draws nothing at all. So the bar launched with a hole where
                // the sleeve goes, and `post_view` could not correct it: it
                // only switches pages when the artwork *changes*, and at
                // startup there is nothing to change from. The empty state is
                // the state the app opens in, so it is the one to open on.
                set_visible_child_name: "sleeve",
            },

            // Deliberately **not** hexpand, and width-limited.
            //
            // A GtkBox gives every child its *natural* width before any
            // hexpand child sees what is left, and an ellipsizing label's
            // natural width is the whole untruncated string — so one long
            // album title squeezed the seek scale to its 220px minimum.
            //
            // `max_width_chars` caps the *natural* width; `width_chars` would
            // set a *minimum*, and leaving that unset is what lets the bar
            // shrink far enough for the window to be tiled.
            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_valign: gtk::Align::Center,
                // **The one that stretches.** The scale used to be, and with
                // it gone the slack has to go somewhere: giving it to the text
                // is what the bar has most use for, and it is also what keeps
                // the clock beside it from moving the transport when a digit
                // is added.
                set_hexpand: true,
                // No minimum width. This carried a 240px floor, and a floor in
                // the bar is a floor under the whole window — the app could not
                // be tiled to half a screen. The labels ellipsize instead,
                // which is what they were already set up to do.
                set_width_request: -1,
                set_spacing: 2,
                // Crossfaded, not flipped.
                //
                // A queue emptying used to cut: the title gone and the grey
                // bars there in the same frame. A `GtkStack` dissolves between
                // its pages instead, and the page is chosen in `post_view`
                // rather than by a `#[watch]` — a transition is an animation,
                // and animated properties are written on an edge.
                #[name = "meta_stack"]
                gtk::Stack {
                    set_transition_type: gtk::StackTransitionType::Crossfade,
                    set_transition_duration: SWAP_MS,
                    set_valign: gtk::Align::Center,
                    // **A `GtkStack` measures its widest child, showing or
                    // not.** So the skeleton below set the bar's minimum width
                    // even with a track playing — 140px the labels never asked
                    // for, and they ellipsize while it cannot. That was the
                    // largest single contribution to a bar that would not go
                    // under 506px, which is why the window clipped its own
                    // content when tiled narrow.
                    set_hhomogeneous: false,

                    // Two grey bars where the title and artist go.
                    //
                    // Deliberately **not** animated: a pulsing skeleton means
                    // "loading", and nothing is loading — nothing is playing.
                    // The static version says "this is where the track goes",
                    // which is both true and quieter than the words "Nothing
                    // playing" sitting in the bar all evening.
                    add_named[Some("empty")] = &gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_valign: gtk::Align::Center,
                        set_spacing: 7,

                        // `halign` on each bar, not only on the column. A
                        // vertical `GtkBox` gives children `Align::Fill`
                        // across and `set_size_request` is a *minimum*, so
                        // both stretched to the column and drew the same
                        // length whatever these numbers said. Once the
                        // metadata column became the hexpanding child, that
                        // width was most of the bar.
                        // 140/92 before. They are a *minimum*, so on an empty
                        // bar they are also the window's — and two grey bars
                        // are a hint about where the title goes, not something
                        // that needs to be title-sized to read as one.
                        gtk::Box {
                            set_halign: gtk::Align::Start,
                            set_size_request: (80, 11),
                            add_css_class: "np-skeleton",
                        },
                        gtk::Box {
                            set_halign: gtk::Align::Start,
                            set_size_request: (52, 9),
                            add_css_class: "np-skeleton",
                        },
                    },

                    add_named[Some("track")] = &gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_valign: gtk::Align::Center,
                        set_spacing: 2,

                        gtk::Label {
                            set_xalign: 0.0,
                            set_ellipsize: gtk::pango::EllipsizeMode::End,
                            // Natural width, not minimum. Roomier than it was,
                            // now that the widest thing in the bar has gone and
                            // the text is what the space is for.
                            set_max_width_chars: 40,
                            add_css_class: "heading",
                            // Track and album names are plain text, not markup.
                            // Without this, a title containing `&` fails to
                            // render and GTK warns on every track change.
                            set_use_markup: false,
                            #[watch]
                            set_label: &model.snap.title,
                            #[watch]
                            set_tooltip_text: (!model.snap.title.is_empty())
                                .then_some(model.snap.title.as_str()),
                        },
                        gtk::Label {
                            set_xalign: 0.0,
                            set_ellipsize: gtk::pango::EllipsizeMode::End,
                            set_max_width_chars: 48,
                            add_css_class: "caption",
                            add_css_class: "dim-label",
                            set_use_markup: false,
                            #[watch]
                            set_label: &model.subtitle(),
                            // The full text on hover, since the bar always
                            // truncates.
                            #[watch]
                            set_tooltip_text: Some(&model.subtitle()),
                        },
                    },
                },
            },
            // One label beside the track, not two centred in the bar,
            // where they read as a caption for nothing.
            //
            // **No `width-chars`.** A fixed width would be a floor under the
            // window, and it is not needed: the metadata beside it is the
            // hexpanding child, so "9:59" becoming "10:00" comes out of that
            // slack and the transport does not move.
            #[name = "time"]
            gtk::Label {
                add_css_class: "numeric",
                add_css_class: "caption",
                add_css_class: "dim-label",
                set_valign: gtk::Align::Center,
            },

            // --- transport -------------------------------------------------
            gtk::Box {
                set_orientation: gtk::Orientation::Horizontal,
                set_valign: gtk::Align::Center,
                set_spacing: 4,

                // Shuffle and repeat flank the transport rather than sitting
                // in it: they change what "next" *means* rather than doing
                // anything now, and a toggle that looks like a transport button
                // gets pressed by accident.
                //
                // A plain button, weighted by [`mode_opacity`]. It was a
                // `GtkToggleButton`, whose "on" is a filled circle — a heavier
                // mark than a mode deserves next to the transport, and one the
                // drawer had no equivalent of, so the same control read two
                // ways in one app.
                gtk::Button {
                    set_icon_name: "media-playlist-shuffle-symbolic",
                    set_tooltip_text: Some("Shuffle"),
                    add_css_class: "flat",
                    add_css_class: "circular",
                    // Stands down on a narrow window. The drawer carries all
                    // three, so this is a control moving rather than one being
                    // taken away — and three round buttons is 102px of a bar
                    // that would not go under 506.
                    #[watch]
                    set_visible: !model.snap.narrow,
                    #[watch]
                    set_opacity: mode_opacity(model.snap.shuffle),
                    #[watch]
                    set_sensitive: model.snap.active,
                    connect_clicked => NowPlayingInput::ShuffleClicked,
                },

                gtk::Button {
                    set_icon_name: "media-skip-backward-symbolic",
                    set_tooltip_text: Some("Previous"),
                    add_css_class: "flat",
                    add_css_class: "circular",
                    #[watch]
                    set_sensitive: model.snap.has_previous,
                    connect_clicked => NowPlayingInput::Previous,
                },

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

                // Three states, so never a toggle. "Off" versus "on" still has
                // to be *visible* — a plain button used to give no indication
                // at all — and that is what [`mode_opacity`] is for; which
                // flavour of on comes through the icon. Clicking cycles.
                gtk::Button {
                    add_css_class: "flat",
                    add_css_class: "circular",
                    #[watch]
                    set_icon_name: model.snap.repeat.icon(),
                    #[watch]
                    set_tooltip_text: Some(model.snap.repeat.tooltip()),
                    #[watch]
                    set_opacity: mode_opacity(model.snap.repeat != Repeat::Off),
                    #[watch]
                    set_sensitive: model.snap.active,
                    #[watch]
                    set_visible: !model.snap.narrow,
                    connect_clicked => NowPlayingInput::RepeatCycled,
                },

                // Lives here rather than in the header: pushing an album or
                // playlist page replaces the header, and the queue was
                // unreachable until you navigated back. The bar is on every
                // page by definition.
                gtk::ToggleButton {
                    set_icon_name: "view-list-symbolic",
                    set_tooltip_text: Some("Queue"),
                    add_css_class: "flat",
                    add_css_class: "circular",
                    #[watch]
                    set_active: model.snap.queue_open,
                    connect_clicked => NowPlayingInput::QueueToggled,
                },

                #[name = "volume"]
                gtk::ScaleButton {
                    set_icons: &[
                        "audio-volume-muted-symbolic",
                        "audio-volume-high-symbolic",
                        "audio-volume-low-symbolic",
                        "audio-volume-medium-symbolic",
                    ],
                    set_tooltip_text: Some("Volume"),
                    add_css_class: "flat",
                    #[watch]
                    set_visible: !model.snap.narrow,
                    // ScaleButton is not a Range, so it takes an Adjustment
                    // rather than set_range. Page increment 0.1 makes scroll
                    // wheel steps feel right.
                    set_adjustment: &gtk::Adjustment::new(
                        1.0, 0.0, 1.0, VOLUME_STEP, 0.1, 0.0,
                    ),
                    // The button follows the mirror rather than owning the
                    // volume, so `Ctrl`+`Up` and the Shell's own slider move it
                    // too.
                    //
                    // **The value is written in `post_view`, and
                    // `value-changed` is connected in `init`** — see the note
                    // in `post_view`. Neither can be expressed here: the write
                    // has to be conditional, and it has to be able to silence
                    // the handler, which means keeping the handler's id.
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
            shown_artwork: std::cell::RefCell::new(None),
            shown_ms: std::cell::Cell::new(0),
            synced_at: None,
            advance: None,
            volume_handler: std::cell::RefCell::new(None),
        };
        let widgets = view_output!();

        // Connected here rather than in `view!` because the handler's id is the
        // point: `post_view` blocks it while writing, and the macro gives no
        // way to keep what `connect_*` returns.
        let handler = {
            let sender = sender.clone();
            widgets.volume.connect_value_changed(move |_, value| {
                sender.input(NowPlayingInput::VolumeChanged(value));
            })
        };
        *model.volume_handler.borrow_mut() = Some(handler);

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            NowPlayingInput::Sync(snap) => {
                let held = self.snap.position_ms;
                self.snap = *snap;
                // Straight from the sidecar, all of it. The bar used to defend
                // the position against a seek of its own that had not come back
                // yet; it no longer has one to defend. The line only reports,
                // and the drawer's scrubber defends its own drag.
                let settled = self.snap.position_ms;

                // Decided once: calling this again afterwards would ask about
                // the state we just changed.
                let action =
                    base_action(self.snap.playing, settled, held, self.synced_at.is_some());
                match action {
                    Base::Clear => self.synced_at = None,
                    Base::Reset => self.synced_at = Some(std::time::Instant::now()),
                    Base::Keep => {}
                }
                if action != Base::Keep {
                    // The sidecar is authoritative, so a new base resets the
                    // monotonic floor. Otherwise a stale one survives a track
                    // change and strands the slider mid-song.
                    self.shown_ms.set(settled);
                }
                self.snap.position_ms = settled;
                self.retime(&sender);
            }
            NowPlayingInput::Advance => {
                // Nothing to update: `post_view` reads `shown_position_ms`,
                // which is a function of the clock. This message exists purely
                // to make relm4 redraw.
            }
            NowPlayingInput::ArtworkReady(path) => self.artwork = path,
            NowPlayingInput::VolumeChanged(v) => {
                // **Adopted locally, unlike shuffle and repeat.** Those are
                // sent and left to come back from the mirror, because nothing
                // downstream re-asserts them. This is a two-way binding, so the
                // model has to hold what the widget just reported or the next
                // view update argues with the user.
                //
                // Every message that reaches here is now a real gesture —
                // `post_view` blocks this handler while it writes, so nothing
                // we do ourselves comes back. The guard below is idempotence
                // rather than echo-catching: a snapshot can move the model
                // while the widget still holds the same number, and resending
                // that helps nobody.
                if !volume_is_new(v, self.snap.volume) {
                    return;
                }
                self.snap.volume = v;
                let _ = sender.output(NowPlayingOutput::SetVolume(v));
            }
            NowPlayingInput::ShuffleClicked => {
                let on = !self.snap.shuffle;
                // Sent, not stored. The button's own state is a `#[watch]` on
                // the snapshot, so it snaps back if MusicKit disagrees — the
                // mirror stays the only source of truth (rule 3).
                let _ = sender.output(NowPlayingOutput::SetShuffle(on));
            }
            NowPlayingInput::QueueToggled => {
                // The button's state is a watch on the snapshot, so it follows
                // the app rather than leading it — same discipline as shuffle.
                let _ = sender.output(NowPlayingOutput::ToggleQueue);
            }
            NowPlayingInput::RepeatCycled => {
                let _ = sender.output(NowPlayingOutput::SetRepeat(self.snap.repeat.next()));
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

    /// The cover is set here rather than with `#[watch]` because it needs a
    /// condition the macro can't express: only swap the image when the file
    /// actually changed.
    fn post_view(&self, widgets: &mut Self::Widgets) {
        // `set_label` compares internally and no-ops when unchanged, so these
        // are free on a tick that only moved the slider.
        widgets.time.set_label(&self.time_label());

        // **The handler is silenced while we write, and that is the whole fix.**
        //
        // GTK emits `value-changed` for a programmatic write exactly as for a
        // drag, and `sender.input` *queues* — so our own write comes back as a
        // message one lap later, by which time the model has already adopted
        // some other value. Comparing the incoming value against the model
        // therefore passes honestly on both sides, the model flips, this writes
        // the flipped value, and that write queues the next message. Two
        // messages stay in flight forever.
        //
        // Measured, holding `Ctrl`+`Down` on a playing track: 495,000 laps, one
        // write per message, the two values a `page_increment` apart and the
        // adjustment provably the same object throughout. 100% of one core
        // inside `update_view`, and because this component's task never yields,
        // the app's own task never runs again — which is why the window dies
        // and `setVolume` stops reaching the sidecar. It does not stop when the
        // key is released; only SIGKILL ends it.
        //
        // Blocking is what breaks the cycle, not the comparison: the comparison
        // only skips writes that were already redundant, and a redundant write
        // was never the problem.
        if let Some(handler) = self.volume_handler.borrow().as_ref()
            && volume_is_new(widgets.volume.value(), self.snap.volume)
        {
            widgets.volume.block_signal(handler);
            widgets.volume.set_value(self.snap.volume);
            widgets.volume.unblock_signal(handler);
        }

        // `gtk_image_set_from_file` does **not** compare — it reloads and
        // re-decodes every time it is called. This function runs on every
        // snapshot, which is twice a second while playing plus every position
        // event MusicKit sends, so the unconditional version was decoding the
        // cover several times a second on the GTK main thread and making the
        // seek bar stutter. The doc comment above always claimed it only
        // swapped on change; now it does.
        let mut shown = self.shown_artwork.borrow_mut();
        if *shown != self.artwork {
            match &self.artwork {
                Some(path) => {
                    widgets.cover.set_from_file(Some(path));
                    widgets.cover_stack.set_visible_child_name("cover");
                }
                None => widgets.cover_stack.set_visible_child_name("sleeve"),
            }
            shown.clone_from(&self.artwork);
        }

        // Guarded, and on an edge: a stack with a transition is an animation,
        // so writing this is asking for a cross-fade. `post_view` runs after
        // every message, and asking on every one is the level trigger that
        // wedged the app elsewhere.
        let want = if self.snap.active { "track" } else { "empty" };
        if widgets.meta_stack.visible_child_name().as_deref() != Some(want) {
            widgets.meta_stack.set_visible_child_name(want);
        }
    }
}

/// What to do with the extrapolation base when a snapshot arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Base {
    /// Stop extrapolating. Nothing is playing, so the position is a fact.
    Clear,
    /// Start counting from now, against the reading just received.
    Reset,
    /// Carry on. The reading has not moved, so re-anchoring it to `now` would
    /// stall the slider and then make it leap.
    Keep,
}

/// Decide what happens to the base.
///
/// The two rules that matter, both learned from bugs:
///
/// - **Not playing means no base at all.** Keeping one across a pause meant
///   resuming extrapolated from *before* the pause, so the slider jumped
///   forward by however long the pause lasted and then snapped back. The same
///   thing happens on every track change, where the state passes through
///   Waiting and Loading for a second or two before audio actually starts.
/// - **Only rebase on a reading that moved.** MusicKit reports about once a
///   second while snapshots go out twice a second, so rebasing on every one
///   would anchor half of them to an unchanged position.
fn base_action(playing: bool, settled_ms: u64, held_ms: u64, have_base: bool) -> Base {
    if !playing {
        Base::Clear
    } else if settled_ms != held_ms || !have_base {
        Base::Reset
    } else {
        Base::Keep
    }
}

/// Whether a volume reading differs from the one already held.
///
/// Used at both ends of the two-way binding: to skip a write the widget does
/// not need, and to skip resending a value that changed nothing.
///
/// Exact comparison, not a tolerance: `GtkScaleButton` stores what it is given
/// without rounding it (measured against GTK 4.22), so two readings of the same
/// value are bit-identical. A tolerance would instead start swallowing small
/// deliberate moves.
pub(crate) fn volume_is_new(incoming: f64, held: f64) -> bool {
    (incoming - held).abs() >= f64::EPSILON
}

/// Where the slider should sit, given the last real reading and how long ago it
/// arrived.
///
/// A free function so the arithmetic can be tested without a clock. Three
/// separate attempts at this shipped broken; the tests below are the reason a
/// fourth one will not.
fn advance(base_ms: u64, ahead_ms: u64, duration_ms: u64, last_shown_ms: u64) -> u64 {
    // Only clamp against a duration we actually have. It is 0 until the first
    // metadata arrives, and clamping to that pins the position at the base —
    // the slider hides itself while duration is unknown, but the elapsed label
    // does not, and a frozen clock is a bug you can read.
    let raw = base_ms + ahead_ms;
    let candidate = if duration_ms > 0 {
        raw.min(duration_ms)
    } else {
        raw
    };

    // A clock only runs forwards. MusicKit's reports are not perfectly even,
    // and a small backward correction is far more noticeable than being a few
    // tens of milliseconds optimistic — but a *large* jump is a seek or a new
    // track, and must be obeyed.
    if candidate < last_shown_ms && last_shown_ms - candidate < BACKSTEP_TOLERANCE_MS {
        last_shown_ms
    } else {
        candidate
    }
}

impl NowPlaying {
    /// Start or stop the redraw timer to match playback.
    ///
    /// Mirrors the app's own tick discipline: a timer that runs while paused is
    /// a timer waking the machine for nothing.
    fn retime(&mut self, sender: &relm4::ComponentSender<Self>) {
        match (self.snap.playing, self.advance.is_some()) {
            (true, false) => {
                let sender = sender.clone();
                self.advance = Some(gtk::glib::timeout_add_local(
                    Duration::from_millis(ADVANCE_MS),
                    move || {
                        sender.input(NowPlayingInput::Advance);
                        gtk::glib::ControlFlow::Continue
                    },
                ));
            }
            (false, true) => {
                if let Some(id) = self.advance.take() {
                    id.remove();
                }
            }
            _ => {}
        }
    }

    /// `elapsed / total`, or nothing at all for a track with no known length —
    /// a lone "0:04" beside a slash and a blank says the app has lost track of
    /// something, when in fact there is simply nothing to divide by yet.
    fn time_label(&self) -> String {
        if self.snap.duration_ms == 0 {
            return String::new();
        }
        format!(
            "{} / {}",
            format_duration(self.shown_position_ms()),
            format_duration(self.snap.duration_ms)
        )
    }

    fn progress(&self) -> f64 {
        if self.snap.duration_ms == 0 {
            return 0.0;
        }
        (self.shown_position_ms() as f64 / self.snap.duration_ms as f64).clamp(0.0, 1.0)
    }

    /// Where the track is *now*: the last reported position, carried forward by
    /// however long ago it was reported.
    ///
    /// Clamped to the track length so a late snapshot cannot run the slider
    /// past the end, and never extrapolated while paused.
    fn shown_position_ms(&self) -> u64 {
        let base = self.snap.position_ms;
        let Some(at) = self.synced_at.filter(|_| self.snap.playing) else {
            self.shown_ms.set(base);
            return base;
        };
        let shown = advance(
            base,
            at.elapsed().as_millis() as u64,
            self.snap.duration_ms,
            self.shown_ms.get(),
        );
        self.shown_ms.set(shown);
        shown
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

    #[test]
    fn an_unchanged_value_is_not_a_move() {
        // Cheap idempotence at both ends of the binding. It is *not* what stops
        // the freeze — blocking the handler is, and no unit test can see that.
        // See `post_view`.
        assert!(!volume_is_new(0.4, 0.4));
        assert!(!volume_is_new(1.0, 1.0));
        assert!(!volume_is_new(0.0, 0.0));
    }

    #[test]
    fn a_real_move_still_gets_through() {
        assert!(volume_is_new(0.4, 1.0));
        assert!(volume_is_new(1.0, 0.95));
        // One keyboard step, which is the smallest move that has to survive.
        assert!(volume_is_new(1.0 - VOLUME_STEP, 1.0));
    }

    #[test]
    fn stepping_the_whole_range_never_stalls_or_overshoots() {
        // Repeated `+= 0.05` does not land on exact decimals, so this walks the
        // accumulated float rather than trusting it: every step must move, and
        // the ends must be exactly 0.0 and 1.0 so the button can reach silent
        // and full.
        let mut v: f64 = 1.0;
        for _ in 0..40 {
            let next = (v - VOLUME_STEP).clamp(0.0, 1.0);
            assert!(next <= v, "a down step went up: {v} -> {next}");
            v = next;
        }
        assert_eq!(v, 0.0, "stepping down never reached silence");
        for _ in 0..40 {
            v = (v + VOLUME_STEP).clamp(0.0, 1.0);
        }
        assert_eq!(v, 1.0, "stepping up never reached full");
    }

    fn model(snap: Snapshot) -> NowPlaying {
        NowPlaying {
            snap,
            artwork: None,
            shown_artwork: std::cell::RefCell::new(None),
            shown_ms: std::cell::Cell::new(0),
            synced_at: None,
            advance: None,
            volume_handler: std::cell::RefCell::new(None),
        }
    }

    #[test]
    fn a_track_with_no_length_shows_no_clock() {
        assert_eq!(model(Snapshot::default()).time_label(), "");
    }

    #[test]
    fn the_clock_reads_elapsed_over_total() {
        let m = model(Snapshot {
            position_ms: 64_000,
            duration_ms: 217_000,
            ..Default::default()
        });
        assert_eq!(m.time_label(), "1:04 / 3:37");
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

    #[test]
    fn the_slider_advances_by_real_time_and_no_more() {
        // 20s in, reported 300ms ago, on a 3 minute track.
        assert_eq!(advance(20_000, 300, 180_000, 20_000), 20_300);
        // The offset is time since the *reading*, not since the app started.
        // Getting that wrong made a track that just began read minutes in.
        assert_eq!(advance(0, 250, 180_000, 0), 250);
    }

    #[test]
    fn the_slider_never_runs_past_the_end() {
        assert_eq!(advance(179_900, 5_000, 180_000, 179_900), 180_000);
    }

    #[test]
    fn a_small_backward_correction_is_absorbed() {
        // MusicKit reports unevenly; a 200ms step back is far more noticeable
        // than being 200ms optimistic, so the slider holds instead.
        assert_eq!(advance(19_800, 0, 180_000, 20_000), 20_000);
    }

    #[test]
    fn a_large_jump_backwards_is_obeyed() {
        // A seek, or a new track. Holding here would strand the slider
        // mid-song for the whole of the next one.
        assert_eq!(advance(0, 0, 180_000, 90_000), 0);
        assert_eq!(advance(5_000, 0, 180_000, 120_000), 5_000);
    }

    #[test]
    fn a_zero_length_track_does_not_clamp_the_position_away() {
        // Duration can be 0 before the first metadata arrives; the position
        // must survive that rather than being clamped to nothing.
        assert_eq!(advance(4_000, 100, 0, 4_000), 4_100);
    }

    #[test]
    fn pausing_drops_the_base_so_resuming_starts_from_now() {
        // The reported bug: pause, wait, play — the slider leapt forward by
        // roughly the length of the pause and then snapped back, because the
        // base still pointed at the moment before the pause.
        assert_eq!(base_action(false, 42_000, 42_000, true), Base::Clear);
        // ...and resuming with the position unchanged must still rebase.
        assert_eq!(base_action(true, 42_000, 42_000, false), Base::Reset);
    }

    #[test]
    fn a_track_change_rebases_even_though_it_passes_through_loading() {
        // Playing -> Waiting -> Loading -> Playing takes a second or two before
        // audio starts. Without dropping the base across it, the slider ran for
        // that whole gap and then snapped back once real positions arrived.
        assert_eq!(base_action(false, 0, 180_000, true), Base::Clear);
        assert_eq!(base_action(true, 0, 180_000, false), Base::Reset);
    }

    #[test]
    fn an_unchanged_reading_keeps_its_base() {
        // MusicKit reports about once a second; snapshots go out twice a
        // second. Rebasing the repeats would stall the slider, then leap it.
        assert_eq!(base_action(true, 20_000, 20_000, true), Base::Keep);
    }

    #[test]
    fn a_moved_reading_rebases() {
        assert_eq!(base_action(true, 21_000, 20_000, true), Base::Reset);
    }
}
