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
use std::time::{Duration, Instant};

use relm4::adw::prelude::*;
use relm4::{ComponentParts, ComponentSender, RelmWidgetExt, SimpleComponent, gtk};

use crate::music::types::format_duration;

/// How long the slider must sit still before the seek is actually sent.
///
/// Dragging emits `change-value` continuously; seeking a DRM HLS stream on
/// every one of those would force a re-buffer per pixel. Waiting for a short
/// pause turns a drag into a single seek while still feeling immediate,
/// because the elapsed label moves with the handle straight away.
/// How much of the bar the track title and artist may claim, in logical pixels.
/// Fixed rather than proportional so the seek scale does not jump about as
/// tracks with different name lengths come and go.
const METADATA_WIDTH: i32 = 240;

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

const SCRUB_COMMIT_MS: u64 = 250;

/// How close the sidecar's reported position must get to a seek target before
/// we believe the seek landed and start trusting its numbers again.
const SEEK_SETTLE_MS: u64 = 1_500;

/// How long to keep holding a seek target before giving up on it.
///
/// Wall-clock, deliberately. This was a snapshot *count* first, which was
/// wrong: `Sync` fires on every sidecar event plus the 500ms repaint tick, so
/// during playback a dozen snapshots elapse in two or three seconds — less than
/// a backward seek into unbuffered audio takes. The hold expired mid-buffer,
/// the bar fell back to the old position, and the seek then landed a moment
/// later. Buffering happens in seconds, so the budget has to be measured in
/// seconds.
const SEEK_HOLD: Duration = Duration::from_secs(10);

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
    pub shuffle: bool,
    /// Whether the queue sidebar is open, so the bar's toggle agrees with it.
    pub queue_open: bool,
    /// Mirrored from MusicKit, never authored here (rule 3).
    pub repeat: Repeat,
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
    /// A seek we have sent but whose effect hasn't come back yet, and the
    /// wall-clock deadline for believing in it. Without this the slider snaps
    /// back to the old position between committing a seek and the sidecar
    /// reporting the new one.
    pending_seek: Option<(u64, Instant)>,
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
    /// Redraw the slider from the interpolated position. Carries nothing and
    /// touches nothing but the two widgets that show time.
    Advance,
    ShuffleToggled(bool),
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
            set_orientation: gtk::Orientation::Horizontal,
            set_spacing: 12,
            set_margin_all: 10,
            add_css_class: "np-bar",
            // Deliberately no blanket `set_sensitive` here. Greying the whole
            // bar when nothing is playing also greyed the queue button, so you
            // could not open the queue to start something — the one moment you
            // most need it. Each control gates itself instead.

            // --- artwork + labels ------------------------------------------
            #[name = "cover"]
            gtk::Image {
                set_pixel_size: 48,
                set_icon_name: Some("audio-x-generic-symbolic"),
                add_css_class: "np-cover",
            },

            // Deliberately **not** hexpand, and width-limited.
            //
            // A GtkBox hands every child up to its *natural* width before any
            // hexpand child gets a share of what's left, and an ellipsizing
            // label's natural width is its whole untruncated string. So
            // "Castlevania Sound Team — Akumajo Dracula Judgment Original
            // Soundtrack" was claiming the space and squeezing the seek scale
            // down to its 220px minimum. Capping `max_width_chars` caps the
            // natural width; the fixed request keeps the bar from reflowing
            // every time the track changes.
            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_valign: gtk::Align::Center,
                set_hexpand: false,
                set_width_request: METADATA_WIDTH,
                set_spacing: 2,

                gtk::Label {
                    set_xalign: 0.0,
                    set_ellipsize: gtk::pango::EllipsizeMode::End,
                    set_max_width_chars: 1,
                    add_css_class: "heading",
                    // Track and album names are plain text, not markup. Without
                    // this, a title containing `&` — "Blood, Sweat & 3 Years",
                    // "Slade & Co" — fails to render and GTK warns on every
                    // track change.
                    set_use_markup: false,
                    #[watch]
                    set_label: if model.snap.title.is_empty() {
                        "Nothing playing"
                    } else {
                        &model.snap.title
                    },
                    #[watch]
                    set_tooltip_text: (!model.snap.title.is_empty())
                        .then_some(model.snap.title.as_str()),
                },
                gtk::Label {
                    set_xalign: 0.0,
                    set_ellipsize: gtk::pango::EllipsizeMode::End,
                    set_max_width_chars: 1,
                    add_css_class: "caption",
                    add_css_class: "dim-label",
                    set_use_markup: false,
                    #[watch]
                    set_label: &model.subtitle(),
                    // The full text on hover, since the bar always truncates.
                    #[watch]
                    set_tooltip_text: Some(&model.subtitle()),
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

                // Fixed width. `numeric` gives tabular figures, but "0:59" and
                // "1:00:00" are different lengths, and a label that resizes
                // under the scale drags the scale with it.
                #[name = "elapsed"]
                gtk::Label {
                    add_css_class: "numeric",
                    add_css_class: "caption",
                    set_width_chars: 5,
                    set_xalign: 1.0,
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
                    set_width_chars: 5,
                    set_xalign: 0.0,
                },
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
                gtk::ToggleButton {
                    set_icon_name: "media-playlist-shuffle-symbolic",
                    set_tooltip_text: Some("Shuffle"),
                    add_css_class: "flat",
                    add_css_class: "circular",
                    #[watch]
                    set_active: model.snap.shuffle,
                    #[watch]
                    set_sensitive: model.snap.active,
                    connect_clicked[sender] => move |b| {
                        sender.input(NowPlayingInput::ShuffleToggled(b.is_active()));
                    },
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

                // A ToggleButton, even though repeat has three states and a
                // toggle has two. "Off" versus "on" is the distinction that
                // needs to be *visible* — a plain button gave no indication at
                // all that repeat was off — and which flavour of on it is comes
                // through the icon. Clicking still cycles.
                #[name = "repeat_button"]
                gtk::ToggleButton {
                    add_css_class: "flat",
                    add_css_class: "circular",
                    #[watch]
                    set_icon_name: model.snap.repeat.icon(),
                    #[watch]
                    set_tooltip_text: Some(model.snap.repeat.tooltip()),
                    #[watch]
                    set_active: model.snap.repeat != Repeat::Off,
                    #[watch]
                    set_sensitive: model.snap.active,
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
            shown_artwork: std::cell::RefCell::new(None),
            shown_ms: std::cell::Cell::new(0),
            synced_at: None,
            advance: None,
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
                let settled = self.settle_position(held, incoming);

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
                    self.pending_seek = Some((target, Instant::now() + SEEK_HOLD));
                    let _ = sender.output(NowPlayingOutput::Seek(target));
                }
            }
            NowPlayingInput::VolumeChanged(v) => {
                self.volume = v;
                let _ = sender.output(NowPlayingOutput::SetVolume(v));
            }
            NowPlayingInput::ShuffleToggled(on) => {
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

    /// The seek position and the cover are set here rather than with `#[watch]`
    /// because both need a condition the macro can't express: never move the
    /// slider while the user is dragging it, and only swap the image when the
    /// file actually changed.
    fn post_view(&self, widgets: &mut Self::Widgets) {
        if !self.scrubbing {
            widgets.seek.set_value(self.progress());
        }
        widgets.seek.set_sensitive(self.snap.duration_ms > 0);
        // `set_label` compares internally and no-ops when unchanged, so these
        // are free on a tick that only moved the slider.
        widgets
            .elapsed
            .set_label(&format_duration(self.shown_position_ms()));
        widgets
            .total
            .set_label(&format_duration(self.snap.duration_ms));

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
                Some(path) => widgets.cover.set_from_file(Some(path)),
                None => widgets
                    .cover
                    .set_icon_name(Some("audio-x-generic-symbolic")),
            }
            shown.clone_from(&self.artwork);
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
        self.settle_position_at(held, incoming, Instant::now())
    }

    /// `settle_position` with the clock injected, so the hold can be tested
    /// without sleeping.
    fn settle_position_at(&mut self, held: u64, incoming: u64, now: Instant) -> u64 {
        if self.scrubbing {
            return held;
        }
        let Some((target, deadline)) = self.pending_seek else {
            return incoming;
        };

        if incoming.abs_diff(target) <= SEEK_SETTLE_MS {
            // The sidecar got there; hand control back.
            self.pending_seek = None;
            return incoming;
        }
        if now >= deadline {
            // Give up rather than show a position playback never reached — a
            // silently failed seek must not freeze the readout.
            self.pending_seek = None;
            return incoming;
        }

        // Still working towards audio (loading, waiting, stalled). Buffering is
        // exactly the case this hold exists for, so don't let it run the clock
        // down — a slow seek must not be mistaken for a failed one.
        if self.snap.busy {
            self.pending_seek = Some((target, now + SEEK_HOLD));
        }
        target
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

    fn model(snap: Snapshot) -> NowPlaying {
        NowPlaying {
            snap,
            artwork: None,
            shown_artwork: std::cell::RefCell::new(None),
            shown_ms: std::cell::Cell::new(0),
            synced_at: None,
            advance: None,
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
        let now = Instant::now();
        let mut m = model(Snapshot::default());
        m.pending_seek = Some((150_000, now + SEEK_HOLD));

        // Still reporting the old position: keep showing the target, no bounce.
        assert_eq!(m.settle_position_at(150_000, 10_000, now), 150_000);
        assert!(m.pending_seek.is_some());

        // Now it has arrived — hand control back to the sidecar.
        assert_eq!(m.settle_position_at(150_000, 150_400, now), 150_400);
        assert!(
            m.pending_seek.is_none(),
            "must stop overriding once settled"
        );
    }

    /// The reported bug: seeking back into audio that had not buffered yet made
    /// the bar fall back to the old position, then jump forward once the data
    /// arrived. The hold used to be a count of snapshots, and `Sync` fires
    /// several times a second while playing, so the budget expired mid-buffer.
    #[test]
    fn many_snapshots_do_not_exhaust_the_hold_a_slow_seek_needs() {
        let now = Instant::now();
        let mut m = model(Snapshot::default());
        m.pending_seek = Some((150_000, now + SEEK_HOLD));

        // Fifty snapshots inside one second — easily reached during playback.
        for i in 0..50 {
            let t = now + Duration::from_millis(i * 20);
            assert_eq!(
                m.settle_position_at(0, 10_000, t),
                150_000,
                "snapshot {i} must not shorten a wall-clock hold"
            );
        }
        assert!(m.pending_seek.is_some());
    }

    #[test]
    fn buffering_refreshes_the_hold_rather_than_running_it_down() {
        let now = Instant::now();
        let mut m = model(Snapshot {
            busy: true,
            ..Default::default()
        });
        m.pending_seek = Some((150_000, now + Duration::from_millis(10)));

        // Nearly expired, but the player is still working towards audio —
        // which is exactly the case the hold exists for.
        assert_eq!(m.settle_position_at(0, 10_000, now), 150_000);
        let (_, deadline) = m.pending_seek.expect("hold must survive buffering");
        assert!(deadline > now + SEEK_HOLD - Duration::from_secs(1));
    }

    #[test]
    fn a_seek_that_never_lands_gives_up_rather_than_freezing() {
        let now = Instant::now();
        let mut m = model(Snapshot::default());
        m.pending_seek = Some((150_000, now + Duration::from_secs(1)));

        assert_eq!(
            m.settle_position_at(0, 10_000, now),
            150_000,
            "still inside the hold"
        );
        // Past the deadline: accept reality rather than showing a position
        // playback never reached.
        assert_eq!(
            m.settle_position_at(0, 10_500, now + Duration::from_secs(2)),
            10_500
        );
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
