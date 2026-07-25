// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The entire sidecar contract, in one file (CLAUDE.md rule 9).
//!
//! Newline-delimited JSON over the child's stdin/stdout. Nothing outside
//! `player/` should ever see these types — `app.rs` translates them into
//! `AppMsg`/`PlayerState`, and `components/` never sees them at all.
//!
//! The variant renames below must match `sidecar/preload.js` and
//! `sidecar/main.js` exactly. If you add a message, add it in both places in
//! the same commit; a silently-unmatched event is the bug you will spend an
//! evening on.
//!
//! The contract is written **complete**, ahead of the UI that calls it: the
//! sidecar's surface is a fixed thing and splitting it across milestones would
//! mean re-deriving it five times. Hence the allow — it covers variants M2–M6
//! will reach for. It does not extend to new dead code elsewhere.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};

/// Rust → sidecar.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "cmd")]
pub enum Command {
    /// Load a whole queue in ONE call and start playing at `start_position`.
    ///
    /// This is the gapless rule (CLAUDE.md rule 3) expressed as a type: there
    /// is deliberately no `PlayTrack { id }` variant, because feeding MusicKit
    /// one track at a time puts a gap at every boundary.
    #[serde(rename = "setQueue", rename_all = "camelCase")]
    SetQueue {
        songs: Vec<String>,
        start_position: usize,
    },

    #[serde(rename = "play")]
    Play,
    #[serde(rename = "pause")]
    Pause,
    #[serde(rename = "playPause")]
    PlayPause,
    #[serde(rename = "next")]
    Next,
    #[serde(rename = "previous")]
    Previous,

    /// Move within the *already loaded* queue. Never re-send `SetQueue` for this.
    #[serde(rename = "changeToIndex")]
    ChangeToIndex { index: usize },

    #[serde(rename = "seek", rename_all = "camelCase")]
    Seek { position_ms: u64 },
    #[serde(rename = "setVolume")]
    SetVolume { volume: f64 },
    #[serde(rename = "setShuffle")]
    SetShuffle { shuffle: bool },
    #[serde(rename = "setRepeat")]
    SetRepeat { mode: RepeatMode },

    /// Apple's own sign-in, shown exactly once.
    #[serde(rename = "showLogin")]
    ShowLogin,
    #[serde(rename = "hide")]
    Hide,
    #[serde(rename = "authorize")]
    Authorize,
    #[serde(rename = "unauthorize")]
    Unauthorize,
    #[serde(rename = "refreshTokens")]
    RefreshTokens,
    #[serde(rename = "quit")]
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RepeatMode {
    #[default]
    None,
    One,
    All,
}

/// Sidecar → Rust.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "event")]
pub enum Event {
    /// Process is up and the stdin loop is running.
    #[serde(rename = "ready")]
    Ready { debug: bool },

    /// The CDM finished installing. No playback is possible before this.
    #[serde(rename = "widevine-ready")]
    WidevineReady,

    /// The preload script executed. Proof-of-life: if this never arrives, the
    /// preload is not running at all and debugging inside it is wasted effort.
    #[serde(rename = "hook-boot")]
    HookBoot {
        #[serde(default)]
        ready_state: String,
        #[serde(default)]
        href: String,
    },

    /// The hook attached to `MusicKit.getInstance()`.
    #[serde(rename = "hook-ready")]
    HookReady {
        authorized: bool,
        version: String,
        /// Which of the two wiring triggers won — `self-poll` or `main-probe`.
        /// Worth logging: if it is always `main-probe`, the renderer's timers
        /// are stalling and that will matter for playback too.
        #[serde(default)]
        trigger: String,
    },

    /// Apple changed the page out from under us (rule 4). Loud, never silent.
    #[serde(rename = "hook-failed")]
    HookFailed { detail: String },

    /// A non-fatal gap — usually an event name this MusicKit version lacks.
    #[serde(rename = "hook-warning")]
    HookWarning { detail: String },

    /// Harvested live on every launch, never cached (rule 7).
    #[serde(rename = "tokens")]
    Tokens(Tokens),

    #[serde(rename = "playbackState")]
    PlaybackState { state: PlaybackState },

    #[serde(rename = "nowPlaying")]
    NowPlaying { item: Option<Item>, queue: Queue },

    #[serde(rename = "position", rename_all = "camelCase")]
    Position { position_ms: u64, duration_ms: u64 },

    #[serde(rename = "queue")]
    Queue(Queue),

    #[serde(rename = "authorization")]
    Authorization { authorized: bool },

    #[serde(rename = "window-hidden")]
    WindowHidden,

    #[serde(rename = "ack")]
    Ack { id: u64 },

    #[serde(rename = "error")]
    Error { code: String, detail: String },
}

/// Never logged, never written to `settings.ini` (rule 7).
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tokens {
    pub developer_token: String,
    pub music_user_token: Option<String>,
    #[serde(default = "default_storefront")]
    pub storefront: String,
    #[serde(default)]
    pub authorized: bool,
}

fn default_storefront() -> String {
    "us".to_owned()
}

/// Hand-written so a stray `{:?}` can never leak a token into a log.
impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("developer_token", &"<redacted>")
            .field(
                "music_user_token",
                &self.music_user_token.as_ref().map(|_| "<redacted>"),
            )
            .field("storefront", &self.storefront)
            .field("authorized", &self.authorized)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlaybackState {
    #[default]
    None,
    Loading,
    Playing,
    Paused,
    Stopped,
    Ended,
    Seeking,
    Waiting,
    Stalled,
    Completed,
    #[serde(other)]
    Unknown,
}

impl PlaybackState {
    /// What MPRIS calls `PlaybackStatus`, collapsed from MusicKit's ten states.
    pub fn is_playing(self) -> bool {
        matches!(self, Self::Playing | Self::Seeking)
    }

    /// True while the sidecar is working towards audio — the UI shows a spinner
    /// rather than a stale "paused".
    pub fn is_busy(self) -> bool {
        matches!(self, Self::Loading | Self::Waiting | Self::Stalled)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Queue {
    #[serde(default)]
    pub position: usize,
    #[serde(default)]
    pub items: Vec<Item>,
}

/// One track as MusicKit sees it. Mapped into `music::types::Track` at the
/// boundary — this shape stays inside `player/`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: Option<String>,
    pub catalog_id: Option<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub track_number: u32,
    /// Apple serves a *template* containing `{w}`/`{h}`/`{f}`.
    /// See `music::types::Artwork`.
    pub artwork_template: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_queue_serialises_to_the_shape_preload_expects() {
        let json = serde_json::to_string(&Command::SetQueue {
            songs: vec!["1440857781".into(), "1440857782".into()],
            start_position: 1,
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"cmd":"setQueue","songs":["1440857781","1440857782"],"startPosition":1}"#
        );
    }

    #[test]
    fn seek_uses_camel_case_position() {
        let json = serde_json::to_string(&Command::Seek {
            position_ms: 30_000,
        })
        .unwrap();
        assert_eq!(json, r#"{"cmd":"seek","positionMs":30000}"#);
    }

    #[test]
    fn unit_commands_carry_only_the_tag() {
        assert_eq!(
            serde_json::to_string(&Command::Play).unwrap(),
            r#"{"cmd":"play"}"#
        );
    }

    #[test]
    fn parses_a_now_playing_event() {
        let raw = r#"{"event":"nowPlaying","item":{"id":"1440857781","title":"Roundabout",
            "artist":"Yes","album":"Fragile","durationMs":513000,"trackNumber":1,
            "artworkTemplate":"https://is1.mzstatic.com/image/thumb/x/{w}x{h}bb.jpg"},
            "queue":{"position":0,"items":[]}}"#;
        let ev: Event = serde_json::from_str(raw).unwrap();
        let Event::NowPlaying {
            item: Some(item), ..
        } = ev
        else {
            panic!("expected NowPlaying");
        };
        assert_eq!(item.title, "Roundabout");
        assert_eq!(item.duration_ms, 513_000);
    }

    #[test]
    fn an_unknown_playback_state_does_not_fail_the_parse() {
        // Apple adding a state must not take the player down.
        let ev: Event =
            serde_json::from_str(r#"{"event":"playbackState","state":"teleporting"}"#).unwrap();
        let Event::PlaybackState { state } = ev else {
            panic!("expected PlaybackState");
        };
        assert_eq!(state, PlaybackState::Unknown);
    }

    #[test]
    fn tokens_never_appear_in_debug_output() {
        let t = Tokens {
            developer_token: "eyJhbGciOi.SECRET".into(),
            music_user_token: Some("ALSO_SECRET".into()),
            storefront: "gb".into(),
            authorized: true,
        };
        let shown = format!("{t:?}");
        assert!(
            !shown.contains("SECRET"),
            "token leaked into Debug: {shown}"
        );
        assert!(shown.contains("gb"));
    }
}
