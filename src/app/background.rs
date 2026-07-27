// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Closing the window, and staying alive when that is the right thing to do.

use relm4::ComponentSender;
use relm4::adw;
use relm4::gtk::prelude::*;

use super::{AppModel, CommandMsg};

impl AppModel {
    /// The close button. **Not a quit** — or not always.
    ///
    /// A music player that stops mid-song because its window was closed is
    /// getting the default wrong; MPRIS exists precisely so the Shell can drive
    /// playback without a window, and that surface used to vanish with it.
    ///
    /// But staying resident with nothing loaded would hold the sidecar's ~320 MB
    /// of Chromium for nothing, so close means one of two things:
    ///
    /// * something is loaded — hide, hold the process open, and keep playing;
    /// * nothing is loaded — quit for real, which is what the button looks like
    ///   it does.
    ///
    /// The asymmetry is deliberate and is the decision recorded in #32. What
    /// makes it honest rather than surprising is that a hidden-but-playing
    /// Tonearm is *findable*: the Shell's media applet shows it, and the
    /// Background portal below lists it under Quick Settings where it can be
    /// quit.
    pub(super) fn close_window(
        &mut self,
        root: &adw::ApplicationWindow,
        sender: &ComponentSender<Self>,
    ) {
        // The queue, not the playback state: paused-with-a-queue is still "you
        // are in the middle of something", and quitting on it would lose the
        // position for no reason.
        if self.player.queue.is_empty() {
            tracing::info!("closing: nothing loaded, quitting");
            relm4::main_application().quit();
            return;
        }

        tracing::info!(
            queue = self.player.queue.len(),
            "closing: staying in the background"
        );
        root.set_visible(false);

        // Idempotent: closing twice must not stack holds, and re-opening drops
        // this so an ordinary window close still quits later.
        if self.background.is_none() {
            self.background = Some(relm4::main_application().hold());
            self.request_background(sender);
        }
    }

    /// Ask the Background portal to list us, so an invisible player can be found
    /// and stopped.
    ///
    /// Best-effort on purpose. A refusal, a missing portal, or a headless
    /// session must not stop playback — the app is already in the background by
    /// the time this runs. It only decides whether GNOME shows Tonearm under
    /// Quick Settings → Background Apps, so a failure costs discoverability, not
    /// function.
    fn request_background(&self, sender: &ComponentSender<Self>) {
        sender.oneshot_command(async {
            let result = ashpd::desktop::background::Background::request()
                .reason("Tonearm keeps playing while its window is closed")
                // Never start on login. Tonearm needs a network and a live
                // session, and an app that launches itself to play nothing is
                // exactly the kind of background process people resent.
                .auto_start(false)
                .send()
                .await;
            CommandMsg::BackgroundPortal(match result {
                Ok(request) => request.response().map(|_| ()).map_err(|e| e.to_string()),
                Err(err) => Err(err.to_string()),
            })
        });
    }
}
