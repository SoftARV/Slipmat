// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The volume panel: what `Ctrl`+`Up`/`Down` shows you.
//!
//! Slipmat's volume is MusicKit's, not the system's, so there is no shell OSD
//! to fall back on — the Shell's slider moves the sink, not the player. And the
//! control that displays it stands down on a narrow window, which is exactly
//! when the shortcut is the only way to reach it: five presses would change the
//! volume by a quarter and show nothing at all (#120).
//!
//! **Raised by the keyboard and by nothing else.** Not by MPRIS — the Shell
//! draws its own slider, our window may not even be on screen, and an animated
//! panel in an unseen window holds the frame clock open, which is the whole of
//! #126. Not by dragging the slider either, because you are looking at the thing
//! that already moved.
//!
//! `.osd` is GTK's own — the same translucent panel the Shell uses — and it
//! brings every colour, in both themes. What it does not bring is a shape: it
//! stops at square corners, and no libadwaita widget exists for an OSD panel to
//! inherit a pill from. So `style.rs` carries one rule of two properties, which
//! is the argument that rule has to make.

use relm4::gtk::prelude::*;
use relm4::{adw, gtk};

use super::{AppModel, AppMsg};

/// How long the panel stays up after the last press.
///
/// Re-armed on every press, so holding the key keeps it visible rather than
/// flickering once per repeat.
const HOLD_MS: u64 = 1_200;

/// A pending hide, and whether it has already gone off.
///
/// A `SourceId` must be removed exactly **once**. A one-shot timeout removes its
/// own source when it fires, but our callback only *sends a message* — and relm4
/// processes that a main-loop turn later. Between those two moments the id we
/// are holding is already dead, and a keypress landing in that gap would remove
/// it a second time.
///
/// The flag closes the gap because the callback sets it **synchronously**,
/// before the message goes anywhere.
///
/// `Rc<Cell<bool>>` rather than a plain `bool`: the closure outlives this scope
/// and cannot borrow the model, so the flag needs two owners. `Rc` and not
/// `Arc` because `timeout_add_local_once` runs its callback on the main thread —
/// there is no second thread to synchronise with, so the cheaper one is right.
pub(super) struct HideTimer {
    id: gtk::glib::SourceId,
    fired: std::rc::Rc<std::cell::Cell<bool>>,
}

/// The panel, and the two widgets whose values change.
pub(super) struct VolumeOsd {
    pub(super) revealer: gtk::Revealer,
    icon: gtk::Image,
    level: gtk::LevelBar,
}

impl VolumeOsd {
    pub(super) fn new() -> Self {
        let icon = gtk::Image::from_icon_name(icon_for(1.0));
        icon.set_pixel_size(16);

        let level = gtk::LevelBar::new();
        level.set_min_value(0.0);
        level.set_max_value(1.0);
        level.set_size_request(140, -1);
        level.set_valign(gtk::Align::Center);
        level.set_hexpand(true);
        // **Or it recolours itself by value, like a battery meter.** A level bar
        // ships `low`, `high` and `full` offsets and paints them yellow, blue and
        // green; volume has no such reading — 20% is not a warning.
        for offset in [
            gtk::LEVEL_BAR_OFFSET_LOW,
            gtk::LEVEL_BAR_OFFSET_HIGH,
            gtk::LEVEL_BAR_OFFSET_FULL,
        ] {
            level.remove_offset_value(Some(offset));
        }

        let panel = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        panel.add_css_class("osd");
        // The colours are `.osd`; the pill shape is ours. See `style.rs`.
        panel.add_css_class("volume-osd");
        panel.append(&icon);
        panel.append(&level);

        let revealer = gtk::Revealer::new();
        revealer.set_transition_type(gtk::RevealerTransitionType::Crossfade);
        revealer.set_child(Some(&panel));
        revealer.set_halign(gtk::Align::Center);
        // **Top, not bottom.** It sat above the Now Playing bar, which put it
        // straight through the middle of the drawer's transport and its queue
        // the moment the drawer was open — over the controls you were reaching
        // for. Nothing is ever laid out directly under the header.
        revealer.set_valign(gtk::Align::Start);
        // It is feedback, not furniture: a click aimed at what is underneath
        // must reach it rather than landing on a panel that is fading out.
        revealer.set_can_target(false);

        Self {
            revealer,
            icon,
            level,
        }
    }

    /// Sit just below the header, however tall the header happens to be.
    ///
    /// `top-bar-height` notifies, so the panel follows it rather than guessing —
    /// the height changes with the theme and the text scale, and guessing at a
    /// bar's height has already been got wrong once here for the content inset.
    ///
    /// Every page's header is the same height, so following one is enough; the
    /// panel floats above the whole window rather than inside a page, which is
    /// what keeps it over the drawer instead of under it.
    pub(super) fn sit_below_the_header(&self, bars: &adw::ToolbarView) {
        let revealer = self.revealer.clone();
        let apply = move |bars: &adw::ToolbarView| {
            revealer.set_margin_top(bars.top_bar_height() + 12);
        };
        apply(bars);
        bars.connect_top_bar_height_notify(apply);
    }

    fn show_level(&self, volume: f64) {
        self.icon.set_icon_name(Some(icon_for(volume)));
        self.level.set_value(volume.clamp(0.0, 1.0));
    }
}

/// Which speaker icon reads as this level.
///
/// The icon carries the coarse answer and the bar the fine one, which is what
/// lets the panel say the level without a number on it.
fn icon_for(volume: f64) -> &'static str {
    match volume {
        v if v <= 0.0 => "audio-volume-muted-symbolic",
        v if v < 1.0 / 3.0 => "audio-volume-low-symbolic",
        v if v < 2.0 / 3.0 => "audio-volume-medium-symbolic",
        _ => "audio-volume-high-symbolic",
    }
}

impl AppModel {
    /// Raise the panel, and arrange for it to go away again.
    ///
    /// **Called from the shortcut's own arms, not from `set_volume`.** That is
    /// deliberate: `set_volume` returns early when the value has not changed, so
    /// at 0.0 and 1.0 it does nothing — and a shortcut that shows nothing at the
    /// ends reads as a dropped keypress rather than as "you are already there".
    pub(super) fn flash_volume(&mut self, sender: &relm4::ComponentSender<Self>) {
        self.volume_osd.show_level(self.volume);
        self.osd_shown = true;

        // **One timer, reset — not one per press.** Holding the key repeats at
        // the keyboard's rate, and a fresh timeout per repeat left dozens alive
        // at once, each holding a cloned sender and each firing later to say
        // something already said.
        //
        // Only cancel one that has not gone off yet — see [`HideTimer`].
        if let Some(pending) = self.osd_timer.take()
            && !pending.fired.get()
        {
            pending.id.remove();
        }

        let fired = std::rc::Rc::new(std::cell::Cell::new(false));
        let id = gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(HOLD_MS), {
            let fired = fired.clone();
            let sender = sender.clone();
            move || {
                fired.set(true);
                sender.input(AppMsg::HideVolumeOsd);
            }
        });
        self.osd_timer = Some(HideTimer { id, fired });
    }

    pub(super) fn hide_volume_osd(&mut self) {
        // The source removed itself when it fired, so drop the handle rather
        // than removing it again.
        self.osd_timer = None;
        self.osd_shown = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_icon_reads_the_level() {
        assert_eq!(icon_for(0.0), "audio-volume-muted-symbolic");
        assert_eq!(icon_for(0.05), "audio-volume-low-symbolic");
        assert_eq!(icon_for(0.5), "audio-volume-medium-symbolic");
        assert_eq!(icon_for(1.0), "audio-volume-high-symbolic");
    }

    #[test]
    fn silence_is_muted_rather_than_low() {
        // The distinction the shortcut exists to make visible: pressing Down to
        // nothing must not look the same as pressing it to almost nothing.
        assert_eq!(icon_for(0.0), "audio-volume-muted-symbolic");
        assert_ne!(icon_for(0.0), icon_for(f64::EPSILON));
    }

    #[test]
    fn a_volume_outside_the_range_still_picks_an_icon() {
        // `set_volume` clamps, but nothing here should depend on that having
        // happened first.
        assert_eq!(icon_for(-1.0), "audio-volume-muted-symbolic");
        assert_eq!(icon_for(2.0), "audio-volume-high-symbolic");
    }
}
