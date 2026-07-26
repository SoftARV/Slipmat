// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Keeping the sidecar alive, and folding everything it says into the mirror.
//!
//! Rule 6: the child is supervised, not fired and forgotten. If it dies we back
//! off, restart, replay the queue and position, and toast **once**. A dead
//! sidecar presenting as a healthy silent player is the failure this file
//! exists to prevent.

use relm4::ComponentSender;

use super::{AppModel, CommandMsg, Stage, View};
use crate::player::protocol::{Command, Event};
use crate::player::{Incoming, sidecar};

pub(super) fn start_sidecar(sender: &ComponentSender<AppModel>) {
    respawn_sidecar(sender, std::time::Duration::ZERO);
}

/// Spawn the sidecar after `delay` and drain its stdout for as long as it lives.
///
/// This is a **streaming** command, not a `oneshot_command`: the receiver stays
/// alive for the whole session, which is the one case CLAUDE.md reserves
/// `command` for. `drop_on_shutdown` is what guarantees the child can't outlive
/// the window — without it, closing Tonearm would leave Chromium playing music
/// with no way to stop it.
pub(super) fn respawn_sidecar(sender: &ComponentSender<AppModel>, delay: std::time::Duration) {
    sender.command(move |out, shutdown| {
        shutdown
            .register(async move {
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                let (handle, mut rx) = match sidecar::spawn() {
                    Ok(pair) => pair,
                    Err(err) => {
                        // A missing sidecar is reported down the same path as a
                        // crashed one, so there is a single recovery route.
                        let _ = out.send(CommandMsg::Sidecar(Incoming::Died(err.to_string())));
                        return;
                    }
                };
                let _ = out.send(CommandMsg::Spawned(handle));
                while let Some(msg) = rx.recv().await {
                    if out.send(CommandMsg::Sidecar(msg)).is_err() {
                        break; // the component is gone
                    }
                }
            })
            .drop_on_shutdown()
    });
}

impl AppModel {
    pub(super) fn send(&self, cmd: Command) {
        // Remembered for the gapless diagnostic below. Cheap, and the only way
        // to tell "MusicKit advanced its own queue" from "we told it to".
        *self.last_command.borrow_mut() = Some((std::time::Instant::now(), cmd.name().to_owned()));
        match &self.sidecar {
            Some(handle) => handle.send(cmd),
            None => tracing::debug!(?cmd, "dropped: no sidecar"),
        }
    }

    /// Report a track change and, crucially, **why** it happened.
    ///
    /// Rule 3's whole point is that a natural boundary is MusicKit advancing a
    /// queue it already holds — that is what makes the transition gapless. If
    /// this ever logs `prompted_by` on a track that ran to its end, Rust is
    /// driving the queue and the headline feature is gone. It is the one
    /// invariant the architecture exists to protect, so it says so out loud
    /// rather than being inferred from silence.
    fn log_transition(&self, from: Option<&str>, left_ms: u64) {
        // A window, not an instant: the command goes out over stdio and the
        // echo comes back, so "just now" is the honest test.
        const RECENT: std::time::Duration = std::time::Duration::from_secs(2);
        let prompted = self
            .last_command
            .borrow()
            .as_ref()
            .filter(|(at, _)| at.elapsed() < RECENT)
            .map(|(_, cmd)| cmd.clone());

        tracing::info!(
            from = from.unwrap_or("<none>"),
            to = self
                .player
                .now_playing
                .as_ref()
                .map(|i| i.title.as_str())
                .unwrap_or("<none>"),
            // How much of the previous track never played. Near zero means it
            // ran out — a natural boundary. Seconds mean it was skipped.
            left_ms,
            prompted_by = prompted
                .as_deref()
                .unwrap_or("nothing — MusicKit advanced itself"),
            "track transition"
        );
    }

    pub(super) fn on_event(&mut self, event: Event, sender: &ComponentSender<Self>) {
        match &event {
            // Bound as `shown`, not `debug`: inside a tracing macro the name
            // `debug` resolves to `tracing::field::debug` instead of our
            // binding, and the field never compiles.
            Event::Ready { debug: shown } => {
                tracing::info!(window_shown = shown, "sidecar ready");
                self.restarts = 0;
            }
            // CDM is in place. Now we're waiting on music.apple.com to load
            // and the hook to attach.
            Event::WidevineReady => self.stage = Stage::Connecting,
            Event::HookBoot { ready_state, href } => {
                tracing::info!(%ready_state, %href, "preload booted")
            }
            Event::HookReady {
                authorized,
                version,
                trigger,
            } => {
                tracing::info!(%version, authorized, %trigger, "musickit hook attached");
                self.stage = if *authorized {
                    Stage::Ready
                } else {
                    Stage::SignedOut
                };
            }
            Event::HookFailed { detail } => {
                // The loud failure rule 4 demands.
                self.stage = Stage::Broken(format!(
                    "Apple Music changed and Tonearm can't attach to its player ({detail}). \
                     Tonearm needs an update."
                ));
            }
            Event::HookWarning { detail } => tracing::warn!(%detail, "hook warning"),
            // Per-command tracing is debug, not info: it was invaluable while
            // the command path was broken and is pure noise now that it works.
            Event::CmdRecv { cmd } => tracing::debug!(%cmd, "sidecar received command"),
            Event::CmdQueued { cmd, depth } => {
                tracing::warn!(%cmd, depth, "command queued — hook not attached")
            }
            Event::CmdDone {
                cmd,
                state,
                queue_len,
            } => tracing::debug!(%cmd, state, queue_len, "sidecar finished command"),
            Event::Tokens(tokens) => {
                // `has_user_token` is the one that matters after sign-in: a
                // developer token alone gets you catalog search but not
                // playback, and the difference is otherwise invisible.
                tracing::info!(
                    storefront = %tokens.storefront,
                    authorized = tokens.authorized,
                    has_user_token = tokens.music_user_token.is_some(),
                    "tokens harvested"
                );
                if tokens.authorized {
                    self.stage = Stage::Ready;
                }
                self.tokens = Some(tokens.clone());
            }
            Event::Authorization { authorized } => {
                tracing::info!(authorized, "authorization changed");
                self.stage = if *authorized {
                    Stage::Ready
                } else {
                    Stage::SignedOut
                };
                if *authorized {
                    self.send(Command::Hide);
                }
            }
            // These three are what tell you whether audio is actually
            // happening. Without them a silent player looks identical to one
            // that was never asked to play anything — which is exactly the
            // hole the first run fell into.
            Event::PlaybackState { state } => tracing::info!(?state, "playback state"),
            Event::NowPlaying { item, queue } => tracing::info!(
                title = item.as_ref().map(|i| i.title.as_str()).unwrap_or("<none>"),
                queue_len = queue.items.len(),
                "now playing changed"
            ),
            Event::Queue(queue) => {
                tracing::debug!(len = queue.items.len(), position = queue.position, "queue")
            }
            Event::Error { code, detail } => {
                tracing::warn!(%code, %detail, "sidecar error");
                if !self.retry_without_dead_tracks(detail) {
                    self.toast(detail);
                }
            }
            _ => {}
        }
        // Captured before the mirror moves on, so a transition can be reported
        // against what was actually playing a moment ago.
        let was = self.player.now_playing.as_ref().map(|i| {
            (
                i.title.clone(),
                i.catalog_id.clone().or_else(|| i.id.clone()),
            )
        });
        // The high-water mark, not a live read — see `progress_mark`.
        let (reached, length) = self.progress_mark.get();
        let left_ms = length.saturating_sub(reached);

        // The mirror is updated last so the stage transitions above always see
        // the previous state (rule 3: this is a projection, not a source).
        let metadata_changed = self.player.apply(&event);

        let now = self
            .player
            .now_playing
            .as_ref()
            .and_then(|i| i.catalog_id.clone().or_else(|| i.id.clone()));
        let changed = was.as_ref().is_some_and(|(_, before)| before != &now) && now.is_some();
        if changed && let Some((title, _)) = &was {
            self.log_transition(Some(title), left_ms);
        }

        if changed || was.is_none() {
            // A new track: start the mark over at its length.
            self.progress_mark.set((0, self.player.duration_ms));
        } else if self.player.position_ms > reached {
            self.progress_mark
                .set((self.player.position_ms, self.player.duration_ms));
        }

        // Remembered before anything renders, so a queue reload has something
        // to hold on to while MusicKit reports nothing at all.
        if let Some(item) = &self.player.now_playing {
            self.last_item = Some(item.clone());
        }

        // Everything below is derived from the mirror, so it happens in one
        // place rather than being sprinkled through the match above — miss one
        // branch there and the bar silently goes stale.
        if metadata_changed {
            let artwork_in_flight = self.sync_artwork(sender);
            self.mark_now_playing();
            self.maybe_notify(artwork_in_flight);
        }
        self.sync_tick(sender);
        self.push_snapshot();
        // After the mirror has the new queue, confirm MusicKit put us on the
        // track that was actually clicked...
        self.verify_start();
        // ...and, if this queue came from the last session, seek into it.
        self.finish_restore();

        // Load the library the moment we're able to, rather than making the
        // user ask. Guarded on all three conditions so a later event — a
        // reconnect, a token refresh — can't kick off a second load over the
        // top of the first.
        if matches!(self.stage, Stage::Ready)
            && self.all_tracks.is_empty()
            && !self.loading_library
            && self.tokens.is_some()
        {
            self.load_library(sender);
        }

        // Put back what was playing when the app last closed. Gated the same
        // way the library load is — there is nothing to restore into without a
        // session — and once per run, so a token refresh cannot restart it.
        if matches!(self.stage, Stage::Ready) && !self.restored && self.tokens.is_some() {
            self.restored = true;
            self.restore_session();
        }

        // The grids load on first visit rather than at startup — but if the app
        // opened straight into one of them, this *is* the first visit, and the
        // `SetView` that would normally trigger it never fires (the view was
        // already correct before the tokens arrived).
        //
        // Gated on `Ready`, which it was not: signed out, this fired on every
        // event, and `refreshTokens` arrives once a second. It was a 403 per
        // second against Apple for as long as the window was open.
        if matches!(self.stage, Stage::Ready) {
            match self.view {
                View::Albums => self.load_albums(sender),
                View::Artists => self.load_artists(sender),
                View::Playlists => self.load_playlists(sender),
                _ => {}
            }
        }
    }
}
