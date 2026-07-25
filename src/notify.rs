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

use relm4::gtk::gio;
use relm4::gtk::prelude::*;

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
}

/// Withdraw the now-playing notification — on quit, so it does not outlive the
/// app that sent it.
pub fn clear(app: &gio::Application) {
    app.withdraw_notification("now-playing");
}
