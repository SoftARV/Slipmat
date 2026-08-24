// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! MPRIS — the v1 headline.
//!
//! Exposes `org.mpris.MediaPlayer2.Slipmat` so the GNOME Shell media applet,
//! the lock screen and `playerctl` can see and drive playback. Half-working
//! MPRIS is the most common failure of the Apple Music wrappers; this has to be
//! bidirectional and correct or it isn't worth shipping.
//!
//! GNOME is not the only consumer, and the ones that are not it read more of
//! the interface: a Quickshell or Waybar bar draws `Shuffle` and `LoopStatus`
//! next to the transport, and offers `Raise` to get back to the window — which
//! on those desktops is the *only* offer, since there is no Shell applet
//! underneath it. So the export is the whole player, not the parts GNOME
//! happens to render.
//!
//! ## Why `Rc<RefCell<…>>` here, of all places
//!
//! CLAUDE.md says reaching for `Rc<RefCell<>>` usually means state belongs in a
//! model. This is the exception, and it is forced by the types rather than
//! chosen:
//!
//! - `mpris_server::Player` is **`!Send`** — its callbacks are `Fn(&Self)`,
//!   not `Fn(&Self) + Send`. It cannot be moved to another thread, and it
//!   cannot be sent through a relm4 `CommandOutput` (those require `Send`).
//! - Building it is **async** (`build().await`), so it does not exist yet when
//!   `AppModel::init` returns.
//!
//! So the handle is created empty, filled in by a task on the GTK main thread,
//! and every later call goes through the same cell. Everything here runs on the
//! main thread; there is no cross-thread sharing and no lock contention.
//!
//! ## What it must never do
//!
//! Emit `PropertiesChanged` on every position tick. MPRIS `Position` is
//! deliberately *polled*, not signalled — which is why `Player::set_position`
//! is the one setter in the crate that is neither `async` nor emits a signal.
//! Everything else is diffed against the last applied state, so a 500ms tick
//! costs one cheap property write and no bus traffic.

use std::cell::RefCell;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::rc::Rc;

use mpris_server::{LoopStatus, Metadata, PlaybackStatus, Player, Time, TrackId};

use crate::player::protocol::RepeatMode;

/// What a controller on the bus asked for.
///
/// The frontend decides what each one means — a GTK client turns them into
/// `AppMsg`, a daemon writes them straight to the sidecar. This module only
/// says what was asked.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MprisCommand {
    Play,
    Pause,
    PlayPause,
    Next,
    Previous,
    /// Absolute, in milliseconds. Relative seeks are resolved before they get
    /// here, against the position the player already holds.
    Seek(u64),
    SetShuffle(bool),
    SetRepeat(RepeatMode),
    SetVolume(f64),
    /// Ask for the window back — the counterpart to a close that only hid it.
    Raise,
    Quit,
}

/// Where those commands go.
type Sink = Rc<dyn Fn(MprisCommand)>;

/// What this player admits to being able to do.
///
/// Both are lies for a daemon: it has no window to raise, and quitting it takes
/// playback from every client attached to it. A controller that offers a button
/// which does nothing is worse than one that offers no button, so the frontend
/// answers rather than this module guessing.
#[derive(Debug, Clone, Copy)]
pub struct Capabilities {
    pub can_raise: bool,
    pub can_quit: bool,
}

impl Capabilities {
    /// An app with a window somebody can be sent back to.
    pub fn windowed() -> Self {
        Self {
            can_raise: true,
            can_quit: true,
        }
    }

    /// A player with no window and no business exiting on a media key.
    pub fn headless() -> Self {
        Self {
            can_raise: false,
            can_quit: false,
        }
    }
}

/// How the frontend spawns a `!Send` future on its own main thread.
///
/// `mpris_server::Player` cannot leave the thread it was built on, and every
/// property write is async, so something has to spawn them. relm4 has
/// `spawn_local`; a tokio `LocalSet` has its own. Choosing between them is the
/// one thing this module must not do.
pub type Spawn = Rc<dyn Fn(Pin<Box<dyn Future<Output = ()>>>)>;

/// Bus name suffix — the full name becomes `org.mpris.MediaPlayer2.Slipmat`.
const BUS_SUFFIX: &str = "Slipmat";

/// Everything MPRIS exports, flattened so it can be diffed cheaply.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct MprisState {
    pub track_id: Option<String>,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub track_number: u32,
    pub art_path: Option<PathBuf>,
    pub length_ms: u64,
    pub position_ms: u64,
    pub playing: bool,
    pub stopped: bool,
    pub can_next: bool,
    pub can_previous: bool,
    pub volume: f64,
    pub shuffle: bool,
    pub repeat: RepeatMode,
}

impl MprisState {
    fn status(&self) -> PlaybackStatus {
        if self.stopped {
            PlaybackStatus::Stopped
        } else if self.playing {
            PlaybackStatus::Playing
        } else {
            PlaybackStatus::Paused
        }
    }

    /// MPRIS names the two loop modes after what they loop over, not after how
    /// many times: `Track` is our One, `Playlist` is our All. The queue is the
    /// list either way — MusicKit has one notion of repeat, not a separate one
    /// per source.
    fn loop_status(&self) -> LoopStatus {
        match self.repeat {
            RepeatMode::None => LoopStatus::None,
            RepeatMode::All => LoopStatus::Playlist,
            RepeatMode::One => LoopStatus::Track,
        }
    }

    /// Only the fields that ride in the `Metadata` dict. Used to decide whether
    /// a `PropertiesChanged` is warranted at all.
    fn metadata_fields(
        &self,
    ) -> (
        &Option<String>,
        &str,
        &str,
        &str,
        u32,
        &Option<PathBuf>,
        u64,
    ) {
        (
            &self.track_id,
            &self.title,
            &self.artist,
            &self.album,
            self.track_number,
            &self.art_path,
            self.length_ms,
        )
    }

    fn metadata(&self) -> Metadata {
        let mut m = Metadata::new();
        m.set_trackid(track_id(self.track_id.as_deref()));
        m.set_title(Some(self.title.clone()));
        if !self.artist.is_empty() {
            m.set_artist(Some([self.artist.clone()]));
        }
        if !self.album.is_empty() {
            m.set_album(Some(self.album.clone()));
        }
        if self.track_number > 0 {
            m.set_track_number(Some(self.track_number as i32));
        }
        if self.length_ms > 0 {
            m.set_length(Some(Time::from_millis(self.length_ms as i64)));
        }
        // `mpris:artUrl` must be a file:// URL — GNOME Shell will not reliably
        // fetch an https:// one, which is the whole reason artwork is cached to
        // disk in components/artwork.rs.
        if let Some(path) = &self.art_path {
            m.set_art_url(Some(format!("file://{}", path.display())));
        }
        m
    }
}

/// Build a D-Bus object path for a track.
///
/// Track ids are object paths, so only `[A-Za-z0-9_]` and `/` are legal.
/// Apple's catalog ids are numeric, but library ids are not (`i.AbCd123`), and
/// an invalid path would make the whole metadata dict fail to serialise —
/// taking the Shell applet's display down with it. Sanitise, don't trust.
fn track_id(id: Option<&str>) -> Option<TrackId> {
    let id = id?;
    let safe: String = id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if safe.is_empty() {
        return None;
    }
    TrackId::try_from(format!("/dev/miguelrincon/Slipmat/track/{safe}")).ok()
}

/// Handle to the exported player. Cheap to clone and hold in the model.
#[derive(Clone)]
pub struct Mpris {
    player: Rc<RefCell<Option<Rc<Player>>>>,
    last: Rc<RefCell<Option<MprisState>>>,
    spawn: Spawn,
}

impl std::fmt::Debug for Mpris {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mpris")
            .field("connected", &self.player.borrow().is_some())
            .finish()
    }
}

impl Mpris {
    /// Start exporting on the session bus.
    ///
    /// Returns immediately; the D-Bus name is claimed by a task on the main
    /// thread. Failing to export is **not fatal** — MPRIS is an integration,
    /// not the app, and a missing session bus should degrade to a working
    /// player rather than a dead one. It logs and stays disconnected.
    pub fn start(spawn: Spawn, sink: Sink, can: Capabilities) -> Self {
        let this = Self {
            player: Rc::new(RefCell::new(None)),
            last: Rc::new(RefCell::new(None)),
            spawn: spawn.clone(),
        };

        let slot = this.player.clone();
        let inner = spawn.clone();
        spawn(Box::pin(async move {
            let player = match Player::builder(BUS_SUFFIX)
                .identity("Slipmat")
                .desktop_entry(crate::APP_ID)
                .can_play(true)
                .can_pause(true)
                .can_seek(true)
                .can_control(true)
                // Both of these describe a player that outlives its window, and
                // that is exactly what `close_window` leaves behind: a hidden,
                // still-playing Slipmat holding the sidecar open. Without
                // `CanRaise` the controller showing that player has no way to
                // bring it back, and without `CanQuit` no way to end it either —
                // the Background portal was the only route to both.
                .can_raise(can.can_raise)
                .can_quit(can.can_quit)
                .build()
                .await
            {
                Ok(player) => player,
                Err(err) => {
                    tracing::warn!(?err, "MPRIS unavailable — playback still works");
                    return;
                }
            };

            wire_controls(&player, sink);

            let player = Rc::new(player);
            // The run task owns its state (`LocalServerRunTask` is 'static), so
            // it can outlive this scope. It must be awaited promptly or the
            // interface never answers.
            inner(Box::pin(player.run()));
            tracing::info!("MPRIS exported as org.mpris.MediaPlayer2.{BUS_SUFFIX}");
            *slot.borrow_mut() = Some(player);
        }));

        this
    }

    /// Push the current state, emitting signals only for what actually changed.
    pub fn update(&self, state: MprisState) {
        let Some(player) = self.player.borrow().clone() else {
            return; // not exported (yet, or at all)
        };

        let previous = self.last.borrow().clone();
        *self.last.borrow_mut() = Some(state.clone());

        // Position is a polled property in MPRIS, so this is a plain setter
        // with no bus traffic. Doing it unconditionally is what keeps
        // `playerctl position` honest between events.
        player.set_position(Time::from_millis(state.position_ms as i64));

        let Some(prev) = previous else {
            // First push: send everything.
            (self.spawn)(Box::pin(apply_all(player, state)));
            return;
        };

        (self.spawn)(Box::pin(async move {
            if state.metadata_fields() != prev.metadata_fields() {
                log_err("metadata", player.set_metadata(state.metadata()).await);
            }
            if state.status() != prev.status() {
                log_err(
                    "playback status",
                    player.set_playback_status(state.status()).await,
                );
            }
            if state.can_next != prev.can_next {
                log_err("can-go-next", player.set_can_go_next(state.can_next).await);
            }
            if state.can_previous != prev.can_previous {
                log_err(
                    "can-go-previous",
                    player.set_can_go_previous(state.can_previous).await,
                );
            }
            if (state.volume - prev.volume).abs() > f64::EPSILON {
                log_err("volume", player.set_volume(state.volume).await);
            }
            if state.shuffle != prev.shuffle {
                log_err("shuffle", player.set_shuffle(state.shuffle).await);
            }
            if state.loop_status() != prev.loop_status() {
                log_err(
                    "loop status",
                    player.set_loop_status(state.loop_status()).await,
                );
            }
        }));
    }

    /// Announce a discontinuous jump. Required by the spec — without it,
    /// controllers keep extrapolating from the old position after a seek.
    pub fn seeked(&self, position_ms: u64) {
        let Some(player) = self.player.borrow().clone() else {
            return;
        };
        (self.spawn)(Box::pin(async move {
            log_err(
                "seeked",
                player.seeked(Time::from_millis(position_ms as i64)).await,
            );
        }));
    }
}

async fn apply_all(player: Rc<Player>, state: MprisState) {
    log_err("metadata", player.set_metadata(state.metadata()).await);
    log_err(
        "playback status",
        player.set_playback_status(state.status()).await,
    );
    log_err("can-go-next", player.set_can_go_next(state.can_next).await);
    log_err(
        "can-go-previous",
        player.set_can_go_previous(state.can_previous).await,
    );
    log_err("volume", player.set_volume(state.volume).await);
    log_err("shuffle", player.set_shuffle(state.shuffle).await);
    log_err(
        "loop status",
        player.set_loop_status(state.loop_status()).await,
    );
}

/// A failed property update is worth a log line and nothing more — the bus
/// going away must not take playback with it (rule 5).
fn log_err(what: &str, result: mpris_server::zbus::Result<()>) {
    if let Err(err) = result {
        tracing::warn!(?err, "MPRIS {what} update failed");
    }
}

/// Wire the bus's buttons to [`MprisCommand`]. This is the half that makes
/// MPRIS bidirectional, and the half wrappers usually skip.
fn wire_controls(player: &Player, sink: Sink) {
    macro_rules! on {
        ($connect:ident, $cmd:expr) => {{
            let sink = sink.clone();
            player.$connect(move |_| sink($cmd));
        }};
    }

    on!(connect_play_pause, MprisCommand::PlayPause);
    on!(connect_play, MprisCommand::Play);
    on!(connect_pause, MprisCommand::Pause);
    on!(connect_stop, MprisCommand::Pause);
    on!(connect_next, MprisCommand::Next);
    on!(connect_previous, MprisCommand::Previous);
    on!(connect_raise, MprisCommand::Raise);
    on!(connect_quit, MprisCommand::Quit);

    // Relative seek, in microseconds, and it can be negative. Resolved here
    // against the position the player holds, so the sink only ever sees an
    // absolute one.
    let s = sink.clone();
    player.connect_seek(move |player, offset| {
        let target = player.position().as_millis() + offset.as_millis();
        s(MprisCommand::Seek(target.max(0) as u64));
    });

    // Absolute seek. The track id is ignored on purpose: we export one player
    // with no TrackList, so there is nothing else it could refer to.
    let s = sink.clone();
    player.connect_set_position(move |_, _track, position| {
        s(MprisCommand::Seek(position.as_millis().max(0) as u64));
    });

    let s = sink.clone();
    player.connect_set_shuffle(move |_, shuffle| s(MprisCommand::SetShuffle(shuffle)));

    // Writable properties, so a controller can set one we do not offer — MPRIS
    // has no way to advertise a partial set. Every `LoopStatus` maps onto a
    // `RepeatMode`, so there is nothing to reject.
    let s = sink.clone();
    player.connect_set_loop_status(move |_, status| {
        s(MprisCommand::SetRepeat(match status {
            LoopStatus::None => RepeatMode::None,
            LoopStatus::Playlist => RepeatMode::All,
            LoopStatus::Track => RepeatMode::One,
        }));
    });

    player.connect_set_volume(move |_, volume| {
        sink(MprisCommand::SetVolume(volume.clamp(0.0, 1.0)));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_ids_are_valid_object_paths() {
        let id = track_id(Some("1049009209")).expect("numeric id");
        assert_eq!(id.as_str(), "/dev/miguelrincon/Slipmat/track/1049009209");
    }

    #[test]
    fn library_ids_with_illegal_characters_are_sanitised() {
        // Library ids look like `i.AbCd123`. A dot is not legal in an object
        // path, and an invalid one makes the whole metadata dict fail to
        // serialise — which takes the Shell applet's display down with it.
        let id = track_id(Some("i.AbCd-123")).expect("sanitised id");
        assert_eq!(id.as_str(), "/dev/miguelrincon/Slipmat/track/i_AbCd_123");
    }

    #[test]
    fn no_track_means_no_id() {
        assert!(track_id(None).is_none());
        assert!(track_id(Some("")).is_none());
    }

    #[test]
    fn status_maps_the_three_mpris_states() {
        let playing = MprisState {
            playing: true,
            ..Default::default()
        };
        assert_eq!(playing.status(), PlaybackStatus::Playing);

        let paused = MprisState::default();
        assert_eq!(paused.status(), PlaybackStatus::Paused);

        let stopped = MprisState {
            stopped: true,
            playing: true, // stopped wins
            ..Default::default()
        };
        assert_eq!(stopped.status(), PlaybackStatus::Stopped);
    }

    #[test]
    fn position_is_not_part_of_the_metadata_diff() {
        // Position changes every tick. If it counted as a metadata change we
        // would emit PropertiesChanged twice a second forever, which is what
        // makes Shell applets stutter.
        let a = MprisState {
            title: "Roundabout".into(),
            position_ms: 1_000,
            ..Default::default()
        };
        let b = MprisState {
            position_ms: 90_000,
            ..a.clone()
        };
        assert_eq!(a.metadata_fields(), b.metadata_fields());
    }

    #[test]
    fn art_url_is_a_file_uri() {
        let state = MprisState {
            art_path: Some(PathBuf::from("/home/x/.cache/slipmat/artwork/ab-512.jpg")),
            ..Default::default()
        };
        let art = state.metadata().art_url().expect("art url");
        assert!(
            art.starts_with("file:///"),
            "GNOME Shell needs a file:// path, got {art}"
        );
    }

    #[test]
    fn repeat_maps_onto_the_loop_status_that_means_the_same_thing() {
        // The names invert: repeat-one is `Track`, repeat-all is `Playlist`.
        // Swapping them is silent — both are valid values — and leaves a bar
        // showing the wrong glyph for a mode the player really is in.
        let mode = |repeat| {
            MprisState {
                repeat,
                ..Default::default()
            }
            .loop_status()
        };

        assert_eq!(mode(RepeatMode::None), LoopStatus::None);
        assert_eq!(mode(RepeatMode::All), LoopStatus::Playlist);
        assert_eq!(mode(RepeatMode::One), LoopStatus::Track);
    }

    #[test]
    fn shuffle_and_repeat_are_not_part_of_the_metadata_diff() {
        // They are properties of the player, not of the track. Folding them
        // into the metadata dict would re-send the whole thing — artwork url
        // included — every time the shuffle button was pressed.
        let off = MprisState {
            title: "Roundabout".into(),
            ..Default::default()
        };
        let on = MprisState {
            shuffle: true,
            repeat: RepeatMode::One,
            ..off.clone()
        };
        assert_eq!(off.metadata_fields(), on.metadata_fields());
        assert_ne!(off.loop_status(), on.loop_status());
    }

    #[test]
    fn an_unknown_length_is_omitted_rather_than_sent_as_zero() {
        let state = MprisState::default();
        assert!(
            state.metadata().length().is_none(),
            "a zero length would render as a 0:00 track"
        );
    }
}
