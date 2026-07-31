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
        /// Whether to start playing once it is loaded.
        ///
        /// True for everything a person asked for. False exactly once, when
        /// restoring the queue from the last session: an app that starts making
        /// noise because it was launched is hostile, and the point of restoring
        /// is to remove the work of finding your place, not to take the
        /// decision away.
        start_playing: bool,
        /// Where in the starting track to begin, in milliseconds.
        ///
        /// Part of the queue descriptor rather than a seek afterwards, because
        /// a seek needs a current item to seek *within* — and a queue loaded
        /// with `start_playing: false` does not have one yet.
        start_time_ms: u64,
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

    /// Move one item within the queue MusicKit already holds.
    ///
    /// `Array.prototype.splice` indices: `to` is where the item lands **after**
    /// it has been taken out, which is what the sidecar's two-call
    /// remove-then-insert produces and what a drag naturally means.
    #[serde(rename = "moveInQueue")]
    MoveInQueue { from: usize, to: usize },

    /// Tell MusicKit which index the current track is at, after an edit moved
    /// it. MusicKit does not work this out for itself.
    #[serde(rename = "syncQueuePosition")]
    SyncQueuePosition { index: usize },

    /// Insert songs into the queue MusicKit already holds — right after the
    /// current track, or at the end.
    ///
    /// Not a `SetQueue`, and that is the point: rebuilding the queue to add a
    /// track would restart playback and discard the gapless buffer (rule 3).
    /// These are the only sanctioned way to grow a queue that is already
    /// playing.
    #[serde(rename = "playNext")]
    PlayNext { songs: Vec<String> },
    #[serde(rename = "playLater")]
    PlayLater { songs: Vec<String> },

    /// Empty the queue and stop.
    #[serde(rename = "clearQueue")]
    ClearQueue,

    /// Drop one item from the loaded queue, by **MusicKit's** index. Also not a
    /// `SetQueue`: rebuilding the queue to remove one track would restart
    /// playback and lose the gapless buffer (rule 3).
    #[serde(rename = "removeFromQueue")]
    RemoveFromQueue { index: usize },

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
    /// Take a song out of the library. Carries the **library** id (`i.…`), not
    /// the catalog one — the two id spaces are not interchangeable and this
    /// endpoint only knows the first.
    ///
    /// Routed through the sidecar rather than `music/client.rs` because only
    /// MusicKit's own client can do it; Apple documents no REST endpoint. See
    /// issue #34.
    #[serde(rename = "removeFromLibrary")]
    RemoveFromLibrary { id: String },
    /// Un-star a song. Carries the **catalog** id, unlike the removal above.
    ///
    /// Also sidecar-only: the identical path over REST with our token answers
    /// `400 Insufficient Permissions`.
    ///
    /// Removes the star and **nothing else** — a song stays in the library
    /// after being un-favourited, which is what Apple's own client does. The
    /// asymmetry is real: favouriting *adds* to the library, un-favouriting
    /// does not take it back out.
    #[serde(rename = "unfavorite")]
    Unfavorite { id: String },
    /// End the session — **not** `unauthorize`.
    ///
    /// `MusicKit.unauthorize()` drops the Music User Token and nothing else.
    /// The login is an ordinary browser session in the sidecar's partition, so
    /// it outlived a sign-out and the next sign-in silently reused the same
    /// Apple identity. This command is handled in the sidecar's *main* process,
    /// which is the only place that can clear those cookies.
    #[serde(rename = "signOut")]
    SignOut,
    #[serde(rename = "refreshTokens")]
    RefreshTokens,
    #[serde(rename = "quit")]
    Quit,
}

impl Command {
    /// The wire name, for logging. Kept next to the `serde` renames above so
    /// the two cannot drift.
    pub fn name(&self) -> &'static str {
        match self {
            Self::SetQueue { .. } => "setQueue",
            Self::Play => "play",
            Self::Pause => "pause",
            Self::PlayPause => "playPause",
            Self::Next => "next",
            Self::Previous => "previous",
            Self::ChangeToIndex { .. } => "changeToIndex",
            Self::MoveInQueue { .. } => "moveInQueue",
            Self::SyncQueuePosition { .. } => "syncQueuePosition",
            Self::PlayNext { .. } => "playNext",
            Self::PlayLater { .. } => "playLater",
            Self::RemoveFromQueue { .. } => "removeFromQueue",
            Self::ClearQueue => "clearQueue",
            Self::Seek { .. } => "seek",
            Self::SetVolume { .. } => "setVolume",
            Self::SetShuffle { .. } => "setShuffle",
            Self::SetRepeat { .. } => "setRepeat",
            Self::RefreshTokens => "refreshTokens",
            Self::ShowLogin => "showLogin",
            Self::Authorize => "authorize",
            Self::RemoveFromLibrary { .. } => "removeFromLibrary",
            Self::Unfavorite { .. } => "unfavorite",
            Self::SignOut => "signOut",
            Self::Quit => "quit",
            Self::Hide => "hide",
        }
    }
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

    /// Shuffle and repeat, as MusicKit currently has them.
    ///
    /// Pushed on change *and* echoed after we set either, because MusicKit does
    /// not reliably fire its own change events for a programmatic change — and
    /// a mode the mirror never hears about is a toggle that springs back the
    /// instant you click it.
    #[serde(rename = "modes")]
    Modes {
        #[serde(default)]
        shuffle: bool,
        #[serde(default)]
        repeat: RepeatMode,
    },

    /// A non-fatal gap — usually an event name this MusicKit version lacks.
    #[serde(rename = "hook-warning")]
    HookWarning { detail: String },

    /// What MusicKit's volume actually is — at attach, and on every change.
    ///
    /// **Inbound, unlike everything else about volume.** MusicKit keeps its own
    /// volume and restores it across launches from the session that keeps the
    /// login, so Rust cannot know it without being told. Adopted, never echoed:
    /// sending a `SetVolume` back for a value MusicKit just reported is a loop.
    #[serde(rename = "volume")]
    Volume { volume: f64 },

    /// Harvested live on every launch, never cached (rule 7).
    #[serde(rename = "tokens")]
    Tokens(Tokens),

    #[serde(rename = "playbackState")]
    PlaybackState { state: PlaybackState },

    #[serde(rename = "nowPlaying")]
    NowPlaying { item: Option<Item>, queue: Queue },

    #[serde(rename = "position", rename_all = "camelCase")]
    Position {
        #[serde(deserialize_with = "ms")]
        position_ms: u64,
        #[serde(deserialize_with = "ms")]
        duration_ms: u64,
    },

    #[serde(rename = "queue")]
    Queue(Queue),

    #[serde(rename = "authorization")]
    Authorization { authorized: bool },

    #[serde(rename = "window-hidden")]
    WindowHidden,

    /// The outcome of one library write, **against the id it was for**.
    ///
    /// `cmd-done` carries only the command name, and the sidecar's dispatch is
    /// async — so two removals can finish out of order and correlating by name
    /// attributes one command's result to another's row. That is not
    /// hypothetical: it dropped the wrong row from the list. This carries the
    /// id so the match is exact.
    ///
    /// `id` is a **library** id for `remove` and a **catalog** id for
    /// `unfavorite`, mirroring the commands.
    #[serde(rename = "library-write")]
    LibraryWrite {
        kind: String,
        id: String,
        ok: bool,
        #[serde(default)]
        detail: String,
    },

    /// Apple's session is gone: cookies and web storage cleared, page reloaded.
    ///
    /// Confirmation, not a request to do anything — the model already forgot
    /// its half when it sent `signOut`. Worth having as a real variant rather
    /// than letting it fall through to `Unparsed`, because a sign-out that
    /// silently failed is the exact bug this pair was written to fix, and a
    /// `warn!("unparsed sidecar line")` on every sign-out would bury it.
    #[serde(rename = "signed-out")]
    SignedOut,

    #[serde(rename = "ack")]
    Ack { id: u64 },

    /// The renderer received a command. Its absence after a dispatch means the
    /// renderer never ran the handler — a frozen page, not a failed command.
    #[serde(rename = "cmd-recv")]
    CmdRecv { cmd: String },

    /// The sidecar parked a command because the hook was not attached. Emitted
    /// so this can never again be silent: a queued command and a dropped one
    /// look identical otherwise.
    #[serde(rename = "cmd-queued")]
    CmdQueued { cmd: String, depth: u32 },

    /// The command resolved. `state` and `queue_len` are MusicKit's own view
    /// immediately afterwards: a `setQueue` that completes with a populated
    /// queue but a non-playing state means playback is being *blocked*, not
    /// failing.
    #[serde(rename = "cmd-done", rename_all = "camelCase")]
    CmdDone {
        cmd: String,
        #[serde(default)]
        state: i32,
        #[serde(default)]
        queue_len: i32,
    },

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

/// Why the queue event fired, which the two cases need distinguishing by.
///
/// MusicKit subscribes these separately and they mean opposite things. The
/// sidecar used to collapse both into one `queue` event, and that is what made
/// a pre-advance indistinguishable from an edit — see [`QueueChange::Position`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QueueChange {
    /// The items changed: something was added, removed, moved or reordered.
    ///
    /// **MusicKit does not re-index its own position after one** — measured for
    /// a removal and for a splice — so this is the case where it has to be told
    /// where the current track went (#117, #118).
    Items,
    /// Only the cursor moved, which is MusicKit driving its own queue.
    ///
    /// **The default, deliberately.** Settling is a write to the player at the
    /// most delicate moment it has, so it happens only when something says an
    /// edit occurred — never because nothing said otherwise. An older sidecar
    /// sends no reason at all and lands here, which loses a correction rather
    /// than inventing one.
    ///
    /// Includes the pre-advance MusicKit does a few hundred milliseconds before
    /// every boundary: it moves the cursor to the next track while
    /// `now_playing` is still the current one. Reading that as a disagreement
    /// and putting it back is fighting the thing that makes gapless work, and
    /// it also meant a command always landed inside the window
    /// `log_transition` looks over — so the rule 3 check could no longer fail
    /// (#121).
    #[default]
    Position,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Queue {
    #[serde(default)]
    pub reason: QueueChange,
    /// MusicKit's queue position, and it is **signed**: it reports `-1` when a
    /// queue is loaded but nothing has become current yet. Deserialising this
    /// as `usize` made serde reject the whole event, so the first `queue` after
    /// every `setQueue` was silently dropped. Use `index()`, not this field.
    #[serde(default = "no_position")]
    pub position: i64,
    #[serde(default)]
    pub items: Vec<Item>,
}

/// Milliseconds from MusicKit, floored at zero.
///
/// **MusicKit uses negative numbers as sentinels**, and a bare `u64` turns that
/// into a rejected line rather than a value. [`Queue::position`] already carries
/// this lesson — it reports `-1` before anything is current — and it recurred on
/// a different field: at a track boundary MusicKit reports a negative position,
///
/// ```text
/// {"event":"position","positionMs":-544000,"durationMs":266000}
/// ```
///
/// and serde rejected the whole event, so the position was dropped with a
/// warning at every transition (#89).
///
/// Read as `f64` rather than `i64` so a fractional millisecond is tolerated as
/// well. The wire has only ever carried integers, but a rejected line is a worse
/// failure than a rounded one — which is the whole point of this function.
fn ms<'de, D: serde::Deserializer<'de>>(de: D) -> Result<u64, D::Error> {
    let raw = f64::deserialize(de)?;
    Ok(if raw.is_finite() && raw > 0.0 {
        raw.round() as u64
    } else {
        0
    })
}

fn no_position() -> i64 {
    -1
}

impl Queue {
    /// The current index, or `None` when nothing is current yet.
    pub fn index(&self) -> Option<usize> {
        usize::try_from(self.position)
            .ok()
            .filter(|i| *i < self.items.len())
    }
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
    #[serde(default, deserialize_with = "ms")]
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
            start_playing: true,
            start_time_ms: 0,
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"cmd":"setQueue","songs":["1440857781","1440857782"],"startPosition":1,"startPlaying":true,"startTimeMs":0}"#
        );
    }

    #[test]
    fn a_restored_queue_asks_not_to_play_and_carries_its_position() {
        // The one case that sends false. If this key ever stops being sent,
        // launching the app would start making noise on its own.
        let json = serde_json::to_string(&Command::SetQueue {
            songs: vec!["1".into()],
            start_position: 0,
            start_playing: false,
            start_time_ms: 42_000,
        })
        .unwrap();
        assert!(json.contains(r#""startPlaying":false"#), "{json}");
        // The position rides in the descriptor. A seek afterwards cannot work:
        // there is no current item to seek within until something plays.
        assert!(json.contains(r#""startTimeMs":42000"#), "{json}");
    }

    #[test]
    fn a_negative_position_is_read_as_zero_not_rejected() {
        // The exact lines from the log in #89, twice at a track boundary. As a
        // `u64` these were rejected whole and the position was dropped.
        for line in [
            r#"{"event":"position","positionMs":-544000,"durationMs":266000}"#,
            r#"{"event":"position","positionMs":-298000,"durationMs":232000}"#,
        ] {
            match serde_json::from_str::<Event>(line).expect("must parse") {
                Event::Position {
                    position_ms,
                    duration_ms,
                } => {
                    assert_eq!(position_ms, 0, "negative should floor at zero");
                    assert!(duration_ms > 0, "duration should survive: {line}");
                }
                other => panic!("wrong variant: {other:?}"),
            }
        }
    }

    #[test]
    fn an_ordinary_position_is_untouched() {
        let line = r#"{"event":"position","positionMs":30000,"durationMs":266000}"#;
        match serde_json::from_str::<Event>(line).unwrap() {
            Event::Position {
                position_ms,
                duration_ms,
            } => assert_eq!((position_ms, duration_ms), (30_000, 266_000)),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn a_fractional_millisecond_rounds_rather_than_failing() {
        // Not seen on the wire, but the point of reading `f64` is that a shape
        // we have not seen costs a rounded value rather than a dropped event.
        let line = r#"{"event":"position","positionMs":1500.6,"durationMs":266000}"#;
        match serde_json::from_str::<Event>(line).unwrap() {
            Event::Position { position_ms, .. } => assert_eq!(position_ms, 1501),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn a_negative_track_duration_is_floored_too() {
        // `Item::duration_ms` comes from the same source and had the same gap.
        let item: Item =
            serde_json::from_str(r#"{"title":"x","durationMs":-1}"#).expect("must parse");
        assert_eq!(item.duration_ms, 0);
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
    fn a_queue_position_of_minus_one_parses() {
        // MusicKit reports -1 between setQueue and the first item becoming
        // current. Deserialising position as usize rejected the entire event,
        // so the queue silently never updated.
        let raw = r#"{"event":"queue","position":-1,"items":[
            {"id":"1049009209","title":"Roundabout"}]}"#;
        let ev: Event = serde_json::from_str(raw).expect("must not fail to parse");
        let Event::Queue(queue) = ev else {
            panic!("expected Queue")
        };
        assert_eq!(queue.items.len(), 1);
        assert_eq!(queue.index(), None, "-1 means nothing is current yet");
    }

    #[test]
    fn a_real_queue_position_resolves() {
        let queue = Queue {
            position: 1,
            items: vec![Item::default(), Item::default()],
            ..Default::default()
        };
        assert_eq!(queue.index(), Some(1));
    }

    #[test]
    fn a_position_past_the_end_is_not_current() {
        let queue = Queue {
            position: 5,
            items: vec![Item::default()],
            ..Default::default()
        };
        assert_eq!(queue.index(), None, "must not index out of bounds");
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
    fn a_library_write_outcome_carries_the_id_it_was_for() {
        // `cmd-done` names only the command, and the sidecar's dispatch is
        // async — so two removals can finish out of order. Correlating by name
        // attributed one completion to the other's row and took the wrong track
        // off the list. The id is what makes the match exact.
        let ok: Event = serde_json::from_str(
            r#"{"event":"library-write","kind":"remove","id":"i.ABC","ok":true}"#,
        )
        .unwrap();
        let Event::LibraryWrite { kind, id, ok, .. } = ok else {
            panic!("expected LibraryWrite");
        };
        assert_eq!((kind.as_str(), id.as_str(), ok), ("remove", "i.ABC", true));

        // And a failure must carry why, so the row is put back with a reason.
        let bad: Event = serde_json::from_str(
            r#"{"event":"library-write","kind":"unfavorite","id":"282559791",
                "ok":false,"detail":"403 Forbidden"}"#,
        )
        .unwrap();
        let Event::LibraryWrite { ok, detail, .. } = bad else {
            panic!("expected LibraryWrite");
        };
        assert!(!ok);
        assert_eq!(detail, "403 Forbidden");
    }

    #[test]
    fn the_removals_carry_the_right_id_space() {
        // Both are `{"cmd": …, "id": …}` on the wire, which is exactly why the
        // *kind* of id matters and cannot be checked by the compiler: removal
        // takes the library id, un-favouriting the catalog id, and swapping
        // them yields a well-formed command that quietly does nothing.
        let remove = serde_json::to_value(Command::RemoveFromLibrary {
            id: "i.RBrxxaLS1BA3Jv5".into(),
        })
        .unwrap();
        assert_eq!(remove["cmd"], "removeFromLibrary");
        assert!(
            remove["id"].as_str().unwrap().starts_with("i."),
            "removal must carry a library id, got {remove:?}"
        );

        let unfav = serde_json::to_value(Command::Unfavorite {
            id: "282559791".into(),
        })
        .unwrap();
        assert_eq!(unfav["cmd"], "unfavorite");
        assert!(
            unfav["id"].as_str().unwrap().parse::<u64>().is_ok(),
            "un-favourite must carry a numeric catalog id, got {unfav:?}"
        );

        assert_eq!(
            Command::RemoveFromLibrary { id: String::new() }.name(),
            "removeFromLibrary"
        );
        assert_eq!(
            Command::Unfavorite { id: String::new() }.name(),
            "unfavorite"
        );
    }

    #[test]
    fn the_sign_out_pair_matches_the_sidecar() {
        // Both halves of one contract, and both were wrong before: the command
        // used to be `unauthorize`, which only ever dropped MusicKit's token
        // and left Apple's cookies in place.
        assert_eq!(Command::SignOut.name(), "signOut");
        assert_eq!(
            serde_json::to_value(Command::SignOut).unwrap()["cmd"],
            "signOut"
        );
        // And the confirmation must parse. If it does not it falls through to
        // `Unparsed` and warns "unparsed sidecar line" on every sign-out —
        // noise sitting exactly where a real sign-out failure would appear.
        assert!(matches!(
            serde_json::from_str::<Event>(r#"{"event":"signed-out"}"#).unwrap(),
            Event::SignedOut
        ));
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
