// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Keeping the sidecar alive, and folding everything it says into the mirror.
//!
//! Rule 6: the child is supervised, not fired and forgotten. If it dies we back
//! off, restart, replay the queue and position, and toast **once**. A dead
//! sidecar presenting as a healthy silent player is the failure this file
//! exists to prevent.

use relm4::ComponentSender;

use super::{AppModel, CommandMsg, Stage};
use slipmat_core::player::protocol::{Command, Event};
use slipmat_core::player::{Incoming, sidecar};

pub(super) fn start_sidecar(sender: &ComponentSender<AppModel>) {
    respawn_sidecar(sender, std::time::Duration::ZERO);
}

/// Spawn the sidecar after `delay` and drain its stdout for as long as it lives.
///
/// This is a **streaming** command, not a `oneshot_command`: the receiver stays
/// alive for the whole session, which is the one case CLAUDE.md reserves
/// `command` for. `drop_on_shutdown` is what guarantees the child can't outlive
/// the window — without it, closing Slipmat would leave Chromium playing music
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
                    "Apple Music changed and Slipmat can't attach to its player ({detail}). \
                     Slipmat needs an update."
                ));
            }
            Event::HookWarning { detail } => tracing::warn!(%detail, "hook warning"),
            Event::Volume { volume } => self.adopt_volume(*volume),
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
            } => {
                tracing::debug!(%cmd, state, queue_len, "sidecar finished command");
                // A `play` that completed and left us not playing is the
                // signature of a decrypt session that did not survive a
                // suspend — see `playback::play_did_nothing`.
                self.play_did_nothing(cmd);
                // Confirmed. Forget the way back, or an unrelated failure
                // later would undo a move that actually happened.
                if cmd == "moveInQueue" {
                    self.pending_move = None;
                }
                // Deliberately does **not** settle library writes: `cmd-done`
                // carries only the command name, and this dispatch is async, so
                // two removals can finish out of order. `Event::LibraryWrite`
                // carries the id and is what settles them.
            }
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
                // **Symmetric**, like `HookReady` and `Authorization` above and
                // below. Promoting without ever demoting was a real bug: a
                // `tokens` event carrying `authorized=false` left the stage on
                // `Ready`, so for the moment before the matching
                // `Authorization` event arrived the app believed it was signed
                // in while holding no user token — and the auto-load below
                // fired a request that could only ever 403.
                self.stage = if tokens.authorized {
                    Stage::Ready
                } else {
                    Stage::SignedOut
                };
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
                if detail == "moveInQueue" && self.undo_move() {
                    return;
                }
                // A refused library write has to put the row back before
                // anything else looks at it, and it words its own toast — the
                // raw detail is a command name, which means nothing to anyone.
                if !self.retry_without_dead_tracks(detail) {
                    self.toast(detail);
                }
            }
            // Logged rather than acted on: the model forgot its half when it
            // sent the command. This is the sidecar confirming it dropped
            // Apple's cookies too, which is the half that used to be skipped
            // silently — so it is worth being able to see in a log.
            Event::SignedOut => tracing::info!("apple session cleared"),
            Event::LibraryWrite {
                kind,
                id,
                ok,
                detail,
            } => self.settle_library_write(kind, id, *ok, detail),
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
        // **Once it is actually playing, not merely current.** A reloaded item
        // is `Loading` for a beat after `nowPlayingItemDidChange`, and a seek
        // sent then is dropped — measured: the track restarted at zero and ran
        // on from there while the log said the position had been restored.
        if self.player.state.is_playing() {
            self.resume_position();
        }
        // **Correcting our own copy is only half of it.** MusicKit does not
        // re-index its position when the queue is edited — measured for both a
        // removal and a splice — and the half it keeps is the one
        // `skipToNextItem` counts from. Left alone, the list looks right and
        // the next track is whatever sits at a stale index.
        //
        // Here rather than beside each edit, because it does not matter what
        // moved: any disagreement is worth settling, and `_updatePosition`
        // returns early when the value already matches, so a redundant one
        // costs nothing.
        if self.player.position_disagrees {
            self.player.position_disagrees = false;
            tracing::debug!(
                index = self.player.queue_position,
                "telling MusicKit where the current track is"
            );
            self.send(Command::SyncQueuePosition {
                index: self.player.queue_position,
            });
        }
        self.sync_tick(sender);
        self.push_snapshot();
        // After the mirror has the new queue, confirm MusicKit put us on the
        // track that was actually clicked.
        self.verify_start();

        // Refresh the whole library the moment we're able to, rather than
        // making the user ask.
        //
        // All four, not just the songs. It used to be songs here and each grid
        // on its first visit, which made sense while a fetch was the only way
        // to fill a section — the wait was paid where it was asked for. With
        // the disk cache the sections are already on screen, so a section left
        // unrefreshed is one showing last launch's answer until you press
        // reload. The cost lands behind content instead of in front of it.
        //
        // Each loader owns the rest of the decision — whether it has already
        // been tried, and whether there is a user token to try with.
        // Duplicating those here is how the two drifted apart.
        if matches!(self.stage, Stage::Ready) {
            self.load_library(sender);
            self.load_albums(sender);
            self.load_artists(sender);
            self.load_playlists(sender);
        }

        // Put back what was playing when the app last closed. Gated the same
        // way the library load is — there is nothing to restore into without a
        // session — and once per run, so a token refresh cannot restart it.
        if matches!(self.stage, Stage::Ready) && !self.restored && self.tokens.is_some() {
            self.restored = true;
            self.restore_session();
        }
    }
}
