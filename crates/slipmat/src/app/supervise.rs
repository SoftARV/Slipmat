// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Staying connected to the daemon, and folding in what it says.
//!
//! This file used to supervise a child process. It does not any more: the
//! daemon owns the sidecar, because the Chromium profile lock says exactly one
//! process may. What is left is a socket that can go away — which is a smaller
//! problem, since reconnecting costs a connect rather than a Widevine boot.

use relm4::ComponentSender;

use super::{AppModel, CommandMsg, Stage};
use crate::daemon;
use slipmat_core::ipc::{Event, Request, Stage as DaemonStage, Transport};

/// How long to wait before dialling again, per consecutive failure.
///
/// Shorter than the sidecar's backoff was, because there is no Chromium to boot
/// — a daemon that is coming up is listening within a fraction of a second, and
/// one that will not start is not helped by waiting longer.
fn redial_delay(attempt: u32) -> std::time::Duration {
    std::time::Duration::from_millis(200 * (1 << attempt.min(5)) as u64)
}

pub(super) fn connect(sender: &ComponentSender<AppModel>) {
    reconnect(sender, std::time::Duration::ZERO);
}

/// Connect after `delay` and stream the daemon's events for as long as it lasts.
///
/// A **streaming** command, not a `oneshot_command`: the receiver stays alive
/// for the whole session, which is the one case CLAUDE.md reserves `command`
/// for. `drop_on_shutdown` no longer guards against an orphaned Chromium — the
/// daemon owns that — but it still stops a dead window holding a socket.
pub(super) fn reconnect(sender: &ComponentSender<AppModel>, delay: std::time::Duration) {
    sender.command(move |out, shutdown| {
        shutdown
            .register(async move {
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                tokio::spawn(daemon::connect(tx));
                while let Some(message) = rx.recv().await {
                    if out.send(CommandMsg::Daemon(message)).is_err() {
                        break; // the component is gone
                    }
                }
            })
            .drop_on_shutdown()
    });
}

impl AppModel {
    /// Ask the daemon for something.
    ///
    /// Fire-and-forget, and deliberately: the answer comes back as an event
    /// like any other, so a button can never draw a state by asking for it.
    pub(super) fn ask(&self, request: Request) {
        match &self.daemon {
            Some(handle) => handle.send(request),
            // The window is up before the connection is, which is a fraction of
            // a second and nothing has been drawn to click yet.
            None => tracing::debug!(?request, "dropped: not connected yet"),
        }
    }

    /// The transport verbs, which are most of what this app asks for.
    pub(super) fn transport(&self, verb: Transport) {
        self.ask(Request::Transport(verb));
    }

    pub(super) fn on_daemon(&mut self, message: daemon::Incoming, sender: &ComponentSender<Self>) {
        match message {
            daemon::Incoming::Connected(handle) => {
                tracing::info!("connected to slipmatd");
                self.redials = 0;
                self.daemon = Some(handle);
                // Everything at once, so the window is right before the first
                // change rather than after it. **The stage especially**: it is
                // only broadcast when it changes, so a client attaching to a
                // daemon that has been ready for an hour has to ask — otherwise
                // it draws its startup screen with music playing behind it.
                self.ask(Request::Stage);
                self.ask(Request::Snapshot);
                self.ask(Request::Queue);
            }
            daemon::Incoming::Event(event) => self.on_event(*event, sender),
            daemon::Incoming::Unparsed(line) => {
                // Loudly, per rule 4: this is `ipc.rs` and this build
                // disagreeing, which a restart will not fix and silence hides.
                tracing::warn!(%line, "daemon sent something this build cannot read");
            }
            daemon::Incoming::Lost(why) => {
                tracing::warn!(%why, "lost the daemon");
                self.daemon = None;
                self.stage = Stage::Connecting;
                let attempt = self.redials;
                self.redials += 1;
                reconnect(sender, redial_delay(attempt));
            }
        }
    }

    fn on_event(&mut self, event: Event, sender: &ComponentSender<Self>) {
        match event {
            Event::Snapshot(snap) => {
                self.mirror.snap = snap;
                self.sync_tick(sender);
                self.push_snapshot();
                // **Here, not on the queue event.** The cover belongs to the
                // snapshot that carries its path, and the two events arrive
                // separately — syncing on the queue read whichever snapshot
                // happened to be current, which is the one *before* the track
                // changed. `art_for` makes the repeat calls free.
                self.sync_artwork(sender);
            }
            Event::Queue { items, position } => {
                let moved = position != self.mirror.queue_position;
                self.mirror.queue = items;
                self.mirror.queue_position = position;
                self.push_snapshot();
                self.mark_now_playing();
                if moved {
                    // A track change is where a notification belongs, and the
                    // queue's position is what says one happened — the snapshot
                    // arrives twice a second and says nothing about *change*.
                    let in_flight = self.mirror.snap.art_path.is_some()
                        && self.art_for != self.mirror.snap.art_path;
                    self.maybe_notify(in_flight);
                }
            }
            Event::Stage(stage) => {
                self.mirror.stage = Some(stage.clone());
                self.stage = match stage {
                    DaemonStage::Connecting => Stage::Connecting,
                    DaemonStage::Ready => Stage::Ready,
                    DaemonStage::SignedOut => Stage::SignedOut,
                    // The loud failure rule 4 demands.
                    DaemonStage::Broken { detail } => Stage::Broken(detail),
                };
            }
            Event::LibraryChanged => {
                tracing::info!("the daemon refreshed the library");
                self.reload_from_cache(sender);
            }
            Event::Page {
                id,
                header,
                entries,
                ..
            } => self.fill_page(&id, header, entries, sender),
            Event::Results {
                query,
                entries,
                offset,
                more,
            } => self.fill_catalog(&query, entries, offset, more),
            Event::Rows { .. } => {
                // Not asked for: this client reads the cache the daemon writes
                // and does its own filtering, sorting and grids, which is
                // presentation rather than something to ask across a socket.
            }
            Event::Error { detail } => self.toast(&detail),
        }
    }
}
