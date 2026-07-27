// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Desktop notifications on track change.
//!
//! **These only appear when the app is installed.** GNOME's notification
//! backend drops notifications from an application it cannot resolve to an
//! installed `.desktop` file and icon, so `cargo run` will send them and show
//! nothing. Testing them means `make install` — and a re-login the first time,
//! so the shell picks up the new icon. Pitwall learned this the hard way; it is
//! written here so nobody debugs a working code path again.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use relm4::gtk::gio;
use relm4::gtk::prelude::*;

/// How long a track-change notification stays before it is withdrawn.
///
/// Long enough for GNOME to show the banner through, short enough that it does
/// not settle into the tray. GNOME shows a low-priority banner for roughly four
/// seconds; this leaves margin without leaving it lying around.
const BANNER: std::time::Duration = std::time::Duration::from_secs(6);

/// Which notification is current. A track change bumps this, so the timer armed
/// by the *previous* track knows not to withdraw the one that replaced it.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Notify that a new track started.
///
/// Deliberately quiet about failure: a notification that does not appear is
/// never worth interrupting playback for.
pub fn track_changed(app: &gio::Application, title: &str, artist: &str, art: Option<&Path>) {
    if title.is_empty() {
        return;
    }

    let notification = gio::Notification::new(title);
    if !artist.is_empty() {
        notification.set_body(Some(artist));
    }

    // Cover art if we have it on disk — the same cached file MPRIS uses.
    if let Some(path) = art.filter(|p| p.is_file()) {
        notification.set_icon(&gio::FileIcon::new(&gio::File::for_path(path)));
    } else {
        notification.set_icon(&gio::ThemedIcon::new(crate::APP_ID));
    }

    // Low urgency: this is ambient information, not something to interrupt for.
    notification.set_priority(gio::NotificationPriority::Low);

    // One id, reused. A track change replaces the previous notification rather
    // than stacking a new one per song — a queue of 500 would otherwise bury
    // the notification tray.
    app.send_notification(Some("now-playing"), &notification);

    // Then take it back.
    //
    // **The banner is the useful part; the history is not.** GNOME's media
    // controls already sit in the same menu showing what is playing, so a
    // notification that stays only says the same thing again, once per track,
    // until the list is a log of everything you listened to.
    //
    // Withdrawing is the mechanism because `GNotification` has no way to say
    // "transient" — that is a freedesktop *hint*, reachable only by talking to
    // `org.freedesktop.Notifications` directly, which would mean giving up the
    // .desktop association this file's header exists to warn about.
    //
    // Guarded by generation: without it, the timer armed for one track would
    // withdraw the notification belonging to the next one, and a queue of short
    // tracks would show almost nothing.
    let generation = GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    let app = app.clone();
    relm4::gtk::glib::timeout_add_local_once(BANNER, move || {
        if GENERATION.load(Ordering::Relaxed) == generation {
            app.withdraw_notification("now-playing");
        }
    });
}

/// Withdraw the now-playing notification — on quit, so it does not outlive the
/// app that sent it.
pub fn clear(app: &gio::Application) {
    app.withdraw_notification("now-playing");
}
