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
//! - `mpris_server::LocalServer` is **`!Send`**. It cannot be moved to another
//!   thread or sent through a relm4 `CommandOutput` (those require `Send`).
//! - Building it is **async**, so it does not exist when `AppModel::init`
//!   returns.
//!
//! So the handle is created empty, filled in by a task on the GTK main thread,
//! and every later call goes through the same cell. Everything here runs on the
//! main thread; there is no cross-thread sharing and no lock contention.
//!
//! ## What it must never do
//!
//! Emit `PropertiesChanged` on every position tick. MPRIS `Position` is
//! deliberately *polled*, not signalled. Everything is diffed against the last
//! applied state, so a 500ms tick only replaces the shared snapshot and sends
//! no bus traffic.

use std::cell::RefCell;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::rc::Rc;

use mpris_server::{
    LocalPlayerInterface, LocalRootInterface, LocalServer, LoopStatus, Metadata, PlaybackRate,
    PlaybackStatus, Property, Signal, Time, TrackId, Volume,
    zbus::{Result as ZbusResult, fdo},
};

use crate::player::protocol::RepeatMode;

pub(crate) mod track_list;

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
/// `mpris_server::LocalServer` cannot leave the thread it was built on, and every
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

struct SlipmatPlayer {
    state: RefCell<MprisState>,
    sink: Sink,
    can: Capabilities,
}

impl SlipmatPlayer {
    fn new(state: MprisState, sink: Sink, can: Capabilities) -> Self {
        Self {
            state: RefCell::new(state),
            sink,
            can,
        }
    }

    fn send(&self, command: MprisCommand) {
        (self.sink)(command);
    }
}

impl LocalRootInterface for SlipmatPlayer {
    async fn raise(&self) -> fdo::Result<()> {
        self.send(MprisCommand::Raise);
        Ok(())
    }

    async fn quit(&self) -> fdo::Result<()> {
        self.send(MprisCommand::Quit);
        Ok(())
    }

    async fn can_quit(&self) -> fdo::Result<bool> {
        Ok(self.can.can_quit)
    }

    async fn fullscreen(&self) -> fdo::Result<bool> {
        Ok(false)
    }

    async fn set_fullscreen(&self, _fullscreen: bool) -> ZbusResult<()> {
        Ok(())
    }

    async fn can_set_fullscreen(&self) -> fdo::Result<bool> {
        Ok(false)
    }

    async fn can_raise(&self) -> fdo::Result<bool> {
        Ok(self.can.can_raise)
    }

    async fn has_track_list(&self) -> fdo::Result<bool> {
        Ok(false)
    }

    async fn identity(&self) -> fdo::Result<String> {
        Ok("Slipmat".into())
    }

    async fn desktop_entry(&self) -> fdo::Result<String> {
        Ok(crate::APP_ID.into())
    }

    async fn supported_uri_schemes(&self) -> fdo::Result<Vec<String>> {
        Ok(Vec::new())
    }

    async fn supported_mime_types(&self) -> fdo::Result<Vec<String>> {
        Ok(Vec::new())
    }
}

impl LocalPlayerInterface for SlipmatPlayer {
    async fn next(&self) -> fdo::Result<()> {
        self.send(MprisCommand::Next);
        Ok(())
    }

    async fn previous(&self) -> fdo::Result<()> {
        self.send(MprisCommand::Previous);
        Ok(())
    }

    async fn pause(&self) -> fdo::Result<()> {
        self.send(MprisCommand::Pause);
        Ok(())
    }

    async fn play_pause(&self) -> fdo::Result<()> {
        self.send(MprisCommand::PlayPause);
        Ok(())
    }

    async fn stop(&self) -> fdo::Result<()> {
        self.send(MprisCommand::Pause);
        Ok(())
    }

    async fn play(&self) -> fdo::Result<()> {
        self.send(MprisCommand::Play);
        Ok(())
    }

    async fn seek(&self, offset: Time) -> fdo::Result<()> {
        let position = self.state.borrow().position_ms as i128;
        let target = (position + i128::from(offset.as_millis())).clamp(0, i128::from(u64::MAX));
        self.send(MprisCommand::Seek(target as u64));
        Ok(())
    }

    async fn set_position(&self, supplied_id: TrackId, position: Time) -> fdo::Result<()> {
        let current_id = track_id(self.state.borrow().track_id.as_deref());
        if current_id.as_ref() == Some(&supplied_id) && position.as_millis() >= 0 {
            self.send(MprisCommand::Seek(position.as_millis() as u64));
        }
        Ok(())
    }

    async fn open_uri(&self, _uri: String) -> fdo::Result<()> {
        Ok(())
    }

    async fn playback_status(&self) -> fdo::Result<PlaybackStatus> {
        Ok(self.state.borrow().status())
    }

    async fn loop_status(&self) -> fdo::Result<LoopStatus> {
        Ok(self.state.borrow().loop_status())
    }

    async fn set_loop_status(&self, status: LoopStatus) -> ZbusResult<()> {
        self.send(MprisCommand::SetRepeat(match status {
            LoopStatus::None => RepeatMode::None,
            LoopStatus::Playlist => RepeatMode::All,
            LoopStatus::Track => RepeatMode::One,
        }));
        Ok(())
    }

    async fn rate(&self) -> fdo::Result<PlaybackRate> {
        Ok(1.0)
    }

    async fn set_rate(&self, _rate: PlaybackRate) -> ZbusResult<()> {
        Ok(())
    }

    async fn shuffle(&self) -> fdo::Result<bool> {
        Ok(self.state.borrow().shuffle)
    }

    async fn set_shuffle(&self, shuffle: bool) -> ZbusResult<()> {
        self.send(MprisCommand::SetShuffle(shuffle));
        Ok(())
    }

    async fn metadata(&self) -> fdo::Result<Metadata> {
        Ok(self.state.borrow().metadata())
    }

    async fn volume(&self) -> fdo::Result<Volume> {
        Ok(self.state.borrow().volume)
    }

    async fn set_volume(&self, volume: Volume) -> ZbusResult<()> {
        self.send(MprisCommand::SetVolume(volume.clamp(0.0, 1.0)));
        Ok(())
    }

    async fn position(&self) -> fdo::Result<Time> {
        Ok(Time::from_millis(self.state.borrow().position_ms as i64))
    }

    async fn minimum_rate(&self) -> fdo::Result<PlaybackRate> {
        Ok(1.0)
    }

    async fn maximum_rate(&self) -> fdo::Result<PlaybackRate> {
        Ok(1.0)
    }

    async fn can_go_next(&self) -> fdo::Result<bool> {
        Ok(self.state.borrow().can_next)
    }

    async fn can_go_previous(&self) -> fdo::Result<bool> {
        Ok(self.state.borrow().can_previous)
    }

    async fn can_play(&self) -> fdo::Result<bool> {
        Ok(true)
    }

    async fn can_pause(&self) -> fdo::Result<bool> {
        Ok(true)
    }

    async fn can_seek(&self) -> fdo::Result<bool> {
        Ok(true)
    }

    async fn can_control(&self) -> fdo::Result<bool> {
        Ok(true)
    }
}

fn changed_player_properties(
    previous: Option<&MprisState>,
    state: &MprisState,
) -> Vec<(&'static str, Property)> {
    let mut changed = Vec::new();
    if previous.is_none_or(|prev| state.metadata_fields() != prev.metadata_fields()) {
        changed.push(("metadata", Property::Metadata(state.metadata())));
    }
    if previous.is_none_or(|prev| state.status() != prev.status()) {
        changed.push(("playback status", Property::PlaybackStatus(state.status())));
    }
    if previous.is_none_or(|prev| state.can_next != prev.can_next) {
        changed.push(("can-go-next", Property::CanGoNext(state.can_next)));
    }
    if previous.is_none_or(|prev| state.can_previous != prev.can_previous) {
        changed.push((
            "can-go-previous",
            Property::CanGoPrevious(state.can_previous),
        ));
    }
    if previous.is_none_or(|prev| (state.volume - prev.volume).abs() > f64::EPSILON) {
        changed.push(("volume", Property::Volume(state.volume)));
    }
    if previous.is_none_or(|prev| state.shuffle != prev.shuffle) {
        changed.push(("shuffle", Property::Shuffle(state.shuffle)));
    }
    if previous.is_none_or(|prev| state.loop_status() != prev.loop_status()) {
        changed.push(("loop status", Property::LoopStatus(state.loop_status())));
    }
    changed
}

/// Handle to the exported player. Cheap to clone and hold in the model.
#[derive(Clone)]
pub struct Mpris {
    player: Rc<RefCell<Option<Rc<LocalServer<SlipmatPlayer>>>>>,
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
            let player = match LocalServer::new(
                BUS_SUFFIX,
                SlipmatPlayer::new(MprisState::default(), sink, can),
            )
            .await
            {
                Ok(player) => player,
                Err(err) => {
                    tracing::warn!(?err, "MPRIS unavailable — playback still works");
                    return;
                }
            };

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
        *player.imp().state.borrow_mut() = state.clone();
        let changed = changed_player_properties(previous.as_ref(), &state);
        (self.spawn)(Box::pin(emit_player_properties(player, changed)));
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
                player
                    .emit(Signal::Seeked {
                        position: Time::from_millis(position_ms as i64),
                    })
                    .await,
            );
        }));
    }
}

async fn emit_player_properties(
    player: Rc<LocalServer<SlipmatPlayer>>,
    changed: Vec<(&'static str, Property)>,
) {
    for (name, property) in changed {
        log_err(name, player.properties_changed([property]).await);
    }
}

/// A failed property update is worth a log line and nothing more — the bus
/// going away must not take playback with it (rule 5).
fn log_err(what: &str, result: mpris_server::zbus::Result<()>) {
    if let Err(err) = result {
        tracing::warn!(?err, "MPRIS {what} update failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local_player(state: MprisState) -> (SlipmatPlayer, Rc<RefCell<Vec<MprisCommand>>>) {
        let commands = Rc::new(RefCell::new(Vec::new()));
        let captured = commands.clone();
        let sink = Rc::new(move |command| captured.borrow_mut().push(command));
        (
            SlipmatPlayer::new(state, sink, Capabilities::windowed()),
            commands,
        )
    }

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
        assert!(changed_player_properties(Some(&a), &b).is_empty());
    }

    #[test]
    fn player_property_changes_match_the_existing_export() {
        let previous = MprisState::default();
        let state = MprisState {
            title: "Roundabout".into(),
            playing: true,
            can_next: true,
            can_previous: true,
            volume: 0.4,
            shuffle: true,
            repeat: RepeatMode::One,
            ..Default::default()
        };

        assert_eq!(
            changed_player_properties(Some(&previous), &state),
            [
                ("metadata", Property::Metadata(state.metadata())),
                (
                    "playback status",
                    Property::PlaybackStatus(PlaybackStatus::Playing),
                ),
                ("can-go-next", Property::CanGoNext(true)),
                ("can-go-previous", Property::CanGoPrevious(true)),
                ("volume", Property::Volume(0.4)),
                ("shuffle", Property::Shuffle(true)),
                ("loop status", Property::LoopStatus(LoopStatus::Track)),
            ]
        );
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

    #[tokio::test(flavor = "current_thread")]
    async fn local_root_matches_the_existing_export() {
        let (player, commands) = local_player(MprisState::default());

        assert!(player.can_raise().await.expect("can raise"));
        assert!(player.can_quit().await.expect("can quit"));
        assert!(!player.fullscreen().await.expect("fullscreen"));
        assert!(!player.can_set_fullscreen().await.expect("set fullscreen"));
        assert!(!player.has_track_list().await.expect("has track list"));
        assert_eq!(player.identity().await.expect("identity"), "Slipmat");
        assert_eq!(
            player.desktop_entry().await.expect("desktop entry"),
            crate::APP_ID
        );
        assert!(
            player
                .supported_uri_schemes()
                .await
                .expect("URI schemes")
                .is_empty()
        );
        assert!(
            player
                .supported_mime_types()
                .await
                .expect("MIME types")
                .is_empty()
        );

        player.raise().await.expect("raise");
        player.quit().await.expect("quit");
        assert_eq!(
            *commands.borrow(),
            [MprisCommand::Raise, MprisCommand::Quit]
        );

        let headless = SlipmatPlayer::new(
            MprisState::default(),
            Rc::new(|_| {}),
            Capabilities::headless(),
        );
        assert!(!headless.can_raise().await.expect("headless can raise"));
        assert!(!headless.can_quit().await.expect("headless can quit"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn local_player_reports_the_current_state() {
        let state = MprisState {
            track_id: Some("1049009209".into()),
            title: "Roundabout".into(),
            position_ms: 12_345,
            playing: true,
            can_next: true,
            can_previous: true,
            volume: 0.4,
            shuffle: true,
            repeat: RepeatMode::One,
            ..Default::default()
        };
        let (player, _) = local_player(state.clone());

        assert_eq!(
            player.playback_status().await.expect("status"),
            state.status()
        );
        assert_eq!(
            player.loop_status().await.expect("loop status"),
            state.loop_status()
        );
        assert_eq!(player.rate().await.expect("rate"), 1.0);
        assert_eq!(player.minimum_rate().await.expect("minimum rate"), 1.0);
        assert_eq!(player.maximum_rate().await.expect("maximum rate"), 1.0);
        assert!(player.shuffle().await.expect("shuffle"));
        assert_eq!(player.metadata().await.expect("metadata"), state.metadata());
        assert_eq!(player.volume().await.expect("volume"), 0.4);
        assert_eq!(
            player.position().await.expect("position"),
            Time::from_millis(12_345)
        );
        assert!(player.can_go_next().await.expect("can go next"));
        assert!(player.can_go_previous().await.expect("can go previous"));
        assert!(player.can_play().await.expect("can play"));
        assert!(player.can_pause().await.expect("can pause"));
        assert!(player.can_seek().await.expect("can seek"));
        assert!(player.can_control().await.expect("can control"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn local_controls_map_to_existing_commands() {
        let state = MprisState {
            track_id: Some("1049009209".into()),
            position_ms: 5_000,
            ..Default::default()
        };
        let (player, commands) = local_player(state);

        player.play().await.expect("play");
        player.pause().await.expect("pause");
        player.play_pause().await.expect("play pause");
        player.stop().await.expect("stop");
        player.next().await.expect("next");
        player.previous().await.expect("previous");
        player
            .seek(Time::from_millis(-2_000))
            .await
            .expect("relative seek");
        player
            .set_position(
                track_id(Some("1049009209")).expect("track id"),
                Time::from_millis(4_000),
            )
            .await
            .expect("absolute seek");
        player.set_shuffle(true).await.expect("shuffle");
        player
            .set_loop_status(LoopStatus::Playlist)
            .await
            .expect("loop status");
        player.set_volume(2.0).await.expect("volume");

        assert_eq!(
            *commands.borrow(),
            [
                MprisCommand::Play,
                MprisCommand::Pause,
                MprisCommand::PlayPause,
                MprisCommand::Pause,
                MprisCommand::Next,
                MprisCommand::Previous,
                MprisCommand::Seek(3_000),
                MprisCommand::Seek(4_000),
                MprisCommand::SetShuffle(true),
                MprisCommand::SetRepeat(RepeatMode::All),
                MprisCommand::SetVolume(1.0),
            ]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn set_position_ignores_a_stale_track_id() {
        let state = MprisState {
            track_id: Some("current".into()),
            ..Default::default()
        };
        let (player, commands) = local_player(state);

        player
            .set_position(
                track_id(Some("stale")).expect("track id"),
                Time::from_millis(9_000),
            )
            .await
            .expect("stale seek");

        assert!(commands.borrow().is_empty());
    }
}
