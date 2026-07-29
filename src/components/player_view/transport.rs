// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The drawer's transport: the scrubber, the buttons, and keeping them honest.
//!
//! A **child** module rather than a sibling, and that is the whole point.
//! These three things are one unit — `build_transport` decides what [`Bits`]
//! holds, and `refresh_transport` is obliged to set every property it holds,
//! because the transport is built by hand and `#[watch]` cannot reach a widget
//! the `view!` macro does not own. Putting construction and refresh in
//! different files is exactly how they drift, which was the recorded objection
//! to splitting this out at all.
//!
//! A child module answers it: they move together, and a child can still see
//! its parent's private fields, so `refresh_transport` stays an inherent
//! method on [`PlayerView`] reading `snap` and `bits` directly.

use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;
use relm4::prelude::*;

use super::{PlayerView, PlayerViewInput};
use crate::components::now_playing::{Repeat, VOLUME_STEP, mode_opacity, volume_is_new};
use crate::music::types::format_duration;

/// The widest the scrubber may get before it stops growing and centres.
const SCRUB_MAX_W: i32 = 520;

/// Build the scrubber and the transport row into `into`.
///
/// By hand rather than in `view!` because this block **moves** between two
/// containers depending on the layout, and the macro's tree is fixed. Building
/// it once and reparenting is what keeps one set of buttons driving one player
/// — the alternative is two transports that have to be kept in step.
///
/// The labels and the scale are handed back through `Bits` so `update` can
/// refresh them; `#[watch]` cannot reach a widget the macro does not own.
pub(super) fn build_transport(into: &gtk::Box, sender: &ComponentSender<PlayerView>) -> Bits {
    let elapsed = gtk::Label::builder()
        .css_classes(["numeric", "caption"])
        .build();
    let remaining = gtk::Label::builder()
        .css_classes(["numeric", "caption"])
        .build();
    let scale = gtk::Scale::builder()
        .hexpand(true)
        .draw_value(false)
        .build();
    {
        let sender = sender.clone();
        scale.connect_change_value(move |_, _, v| {
            sender.input(PlayerViewInput::Scrub(v));
            gtk::glib::Propagation::Proceed
        });
    }

    let scrubber_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    scrubber_row.append(&elapsed);
    scrubber_row.append(&scale);
    scrubber_row.append(&remaining);
    // Clamped rather than filling. A scrubber that spans a maximised window is
    // both hard to aim — one pixel is most of a second — and unlike anything
    // else in the drawer, which is a centred column. `AdwClamp` is the widget
    // for "up to this wide, then centre", and using it keeps the ceiling out of
    // the widget's own minimum, so the compact layout still shrinks freely.
    into.append(
        &adw::Clamp::builder()
            .maximum_size(SCRUB_MAX_W)
            .child(&scrubber_row)
            .build(),
    );

    let buttons = gtk::Box::builder()
        .spacing(10)
        .halign(gtk::Align::Center)
        .build();

    let button = |icon: &str, classes: &[&str]| {
        gtk::Button::builder()
            .icon_name(icon)
            .css_classes(classes.iter().map(|c| c.to_string()).collect::<Vec<_>>())
            .build()
    };

    // Flanking the transport, and only while the queue is closed — the queue's
    // own header carries them once it is open, and two live copies of one
    // control on screen at once is the redundancy this drawer keeps avoiding.
    let shuffle = button("media-playlist-shuffle-symbolic", &["flat", "circular"]);
    shuffle.set_tooltip_text(Some("Shuffle"));
    let repeat = button("media-playlist-repeat-symbolic", &["flat", "circular"]);
    let previous = button("media-skip-backward-symbolic", &["flat", "circular"]);
    let play = button(
        "media-playback-start-symbolic",
        &["suggested-action", "circular"],
    );
    play.set_width_request(56);
    play.set_height_request(56);
    let next = button("media-skip-forward-symbolic", &["flat", "circular"]);
    // Only the way *in*. Closing belongs to the queue's own header, so this
    // hides once the queue is showing rather than becoming a second control
    // for the same thing.
    let queue = button("view-list-symbolic", &["flat", "circular"]);
    queue.set_tooltip_text(Some("Queue"));

    // **Volume lives here now.** The bar drops its own below the narrow
    // breakpoint, and shuffle and repeat were already down here to fall back
    // on — volume was the one control that would have had nowhere left to go.
    let volume = gtk::ScaleButton::builder()
        .icons([
            "audio-volume-muted-symbolic",
            "audio-volume-high-symbolic",
            "audio-volume-low-symbolic",
            "audio-volume-medium-symbolic",
        ])
        .tooltip_text("Volume")
        .css_classes(["flat", "circular"])
        .adjustment(&gtk::Adjustment::new(1.0, 0.0, 1.0, VOLUME_STEP, 0.1, 0.0))
        .build();
    {
        let sender = sender.clone();
        volume.connect_value_changed(move |_, v| {
            sender.input(PlayerViewInput::VolumeChanged(v));
        });
    }

    for (widget, msg) in [
        (&previous, PlayerViewInput::Previous),
        (&play, PlayerViewInput::PlayPause),
        (&next, PlayerViewInput::Next),
        (&queue, PlayerViewInput::SetQueueShown(true)),
        (&shuffle, PlayerViewInput::ShuffleClicked),
        (&repeat, PlayerViewInput::RepeatClicked),
    ] {
        let sender = sender.clone();
        widget.connect_clicked(move |_| sender.input(msg.clone()));
    }

    // Play is in the middle either way: five with the modes, three without,
    // and a hidden widget takes no space.
    for w in [
        shuffle.upcast_ref::<gtk::Widget>(),
        previous.upcast_ref(),
        play.upcast_ref(),
        next.upcast_ref(),
        repeat.upcast_ref(),
    ] {
        buttons.append(w);
    }
    into.append(&buttons);

    // Under the play button rather than beside it, so it never disturbs the
    // count that keeps play centred.
    let queue_row = gtk::Box::builder()
        .halign(gtk::Align::Center)
        .spacing(6)
        .build();
    queue_row.append(&queue);
    queue_row.append(&volume);
    into.append(&queue_row);

    Bits {
        elapsed,
        remaining,
        scale,
        play,
        previous,
        next,
        queue,
        volume,
        shuffle,
        repeat,
    }
}

/// The pieces of the hand-built transport that `update` has to refresh.
pub(super) struct Bits {
    elapsed: gtk::Label,
    remaining: gtk::Label,
    scale: gtk::Scale,
    play: gtk::Button,
    previous: gtk::Button,
    next: gtk::Button,
    queue: gtk::Button,
    volume: gtk::ScaleButton,
    shuffle: gtk::Button,
    repeat: gtk::Button,
}

impl PlayerView {
    /// Push the current snapshot into the hand-built transport.
    ///
    /// The macro's `#[watch]` cannot reach these — they are built outside its
    /// tree so they can move between layouts — so this is the equivalent, and
    /// it has the same obligation: set **every** property it cares about, since
    /// the last track left its own values behind.
    pub(super) fn refresh_transport(&self) {
        let Some(bits) = self.bits.as_ref() else {
            return;
        };
        bits.elapsed
            .set_label(&format_duration(self.snap.position_ms));
        bits.remaining.set_label(&format_duration(
            self.snap.duration_ms.saturating_sub(self.snap.position_ms),
        ));
        bits.scale
            .set_range(0.0, self.snap.duration_ms.max(1) as f64);
        bits.scale.set_sensitive(self.snap.duration_ms > 0);
        // Only when it is not the user's: a drag must not be argued with.
        if !self.scrubbing {
            bits.scale.set_value(self.snap.position_ms as f64);
        }
        bits.play.set_icon_name(if self.snap.playing {
            "media-playback-pause-symbolic"
        } else {
            "media-playback-start-symbolic"
        });
        bits.play.set_sensitive(self.snap.active);
        bits.previous.set_sensitive(self.snap.has_previous);
        bits.shuffle.set_opacity(mode_opacity(self.snap.shuffle));
        bits.repeat.set_icon_name(match self.snap.repeat {
            Repeat::One => "media-playlist-repeat-song-symbolic",
            _ => "media-playlist-repeat-symbolic",
        });
        bits.repeat
            .set_opacity(mode_opacity(!matches!(self.snap.repeat, Repeat::Off)));
        bits.next.set_sensitive(self.snap.has_next);
        // Guarded, or the mirror and the widget ping-pong through the reducer —
        // the #37 loop, which this control is a second chance to reintroduce.
        if volume_is_new(bits.volume.value(), self.snap.volume) {
            bits.volume.set_value(self.snap.volume);
        }
    }
}

impl Bits {
    /// Show or hide the controls that stand down when the queue is open.
    ///
    /// Shuffle and repeat move to the queue's own header there, and the way
    /// *in* is pointless once you are in — so all three go together, and the
    /// parent asks for that rather than reaching into three fields it would
    /// then have to keep in step.
    pub(super) fn set_secondary_visible(&self, visible: bool) {
        // **Faded, not hidden.** Shuffle and repeat sit in the horizontal row
        // and cost nothing to hide, but the queue button has a row of its own —
        // and a hidden child collapses it, taking 34px out of the drawer's
        // height every time the queue opened. Measured: 562px shut, 528px
        // shown, and the whole difference was this one button.
        //
        // The drawer's height is its content's natural height, because
        // `AdwBottomSheet` has no height setter, so a control that vanishes
        // *is* a resize.
        self.queue.set_opacity(if visible { 1.0 } else { 0.0 });
        self.queue.set_sensitive(visible);
        self.shuffle.set_visible(visible);
        self.repeat.set_visible(visible);
    }
}
