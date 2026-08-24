// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The daemon contract, in one file (rule 9's discipline, a second time).
//!
//! Newline-delimited JSON over a Unix socket, the same shape as the sidecar's
//! protocol and for the same reason: one file holds the whole surface, so a
//! request and its handler cannot drift apart across a release.
//!
//! **The client sends intent, never state.** `slipmatd` owns the sidecar and
//! the mirror; a frontend asks for things and is told what happened. That is
//! rule 3 one hop further out — MusicKit owns the queue, the daemon mirrors it,
//! and clients mirror the daemon.

use serde::{Deserialize, Serialize};

use crate::player::protocol::{Item, RepeatMode};

/// Where the daemon listens. `$XDG_RUNTIME_DIR` is per-user and cleared on
/// logout, which is what a socket wants — a stale one in `~` outlives the
/// process that made it.
pub fn socket_path() -> Option<std::path::PathBuf> {
    let dir = std::env::var("XDG_RUNTIME_DIR")
        .ok()
        .filter(|d| !d.is_empty())?;
    Some(std::path::PathBuf::from(dir).join("slipmat.sock"))
}

/// Client → daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "req")]
pub enum Request {
    /// Drive playback. The daemon translates to the sidecar; nothing here says
    /// what the resulting state will be, because only MusicKit decides that.
    #[serde(rename = "transport")]
    Transport(Transport),

    /// Move within the queue the daemon already holds. **Not** a way to build
    /// one — that would cost the gapless buffer (rule 3).
    #[serde(rename = "jumpTo")]
    JumpTo { index: usize },

    /// Everything the daemon is holding, once. Sent on connect by a client that
    /// needs to draw before anything changes.
    #[serde(rename = "snapshot")]
    Snapshot,

    /// The queue as the daemon mirrors it.
    #[serde(rename = "queue")]
    Queue,

    /// Start receiving [`Event`]s on this connection. Idempotent.
    #[serde(rename = "subscribe")]
    Subscribe,
}

/// The transport verbs, kept separate from [`Request`] so a client can pass one
/// around without carrying the rest of the protocol.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(tag = "cmd", rename_all = "camelCase")]
pub enum Transport {
    Play,
    Pause,
    PlayPause,
    Next,
    Previous,
    /// Absolute, in milliseconds.
    Seek {
        position_ms: u64,
    },
    SetVolume {
        volume: f64,
    },
    SetShuffle {
        shuffle: bool,
    },
    SetRepeat {
        mode: RepeatMode,
    },
}

/// Daemon → client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event")]
pub enum Event {
    /// What is playing and how. The one a bar redraws from.
    #[serde(rename = "snapshot")]
    Snapshot(Snapshot),

    /// The queue changed — reordered, replaced, or emptied. Carries the whole
    /// list: it is bounded by what MusicKit will hold, and a diff protocol here
    /// would be a second reconciliation to keep honest.
    #[serde(rename = "queue")]
    Queue {
        items: Vec<QueueItem>,
        position: usize,
    },

    /// Where the daemon is in its own startup, or why it is not playable.
    #[serde(rename = "stage")]
    Stage(Stage),

    /// Something went wrong that a person should see.
    #[serde(rename = "error")]
    Error { detail: String },
}

/// One row of the queue.
///
/// Not `protocol::Item`, whose doc says it stays inside `player/` — and it
/// should: that is MusicKit's shape, carrying two id spaces and an artwork
/// template a client has no business resolving. This is what a row needs to
/// draw itself and to be jumped to.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct QueueItem {
    /// Whichever id the daemon can act on — catalog if there is one, else the
    /// library id. Opaque to the client; it comes back in `JumpTo`.
    pub id: Option<String>,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u64,
}

impl From<&Item> for QueueItem {
    fn from(item: &Item) -> Self {
        Self {
            id: item.catalog_id.clone().or_else(|| item.id.clone()),
            title: item.title.clone(),
            artist: item.artist.clone(),
            album: item.album.clone(),
            duration_ms: item.duration_ms,
        }
    }
}

/// What is playing, flattened for a client that only draws.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// A local file, already fetched. Clients do not talk to Apple for art.
    pub art_path: Option<String>,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub playing: bool,
    /// Still working towards audio. A client should not read this as paused.
    pub busy: bool,
    pub volume: f64,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub can_next: bool,
    pub can_previous: bool,
}

/// How far along the daemon is. A client draws something different for each.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "stage", rename_all = "camelCase")]
pub enum Stage {
    /// The sidecar is up but MusicKit has not attached yet.
    Connecting,
    /// Ready to play.
    Ready,
    /// No Apple session. A client cannot fix this — sign-in needs a window.
    SignedOut,
    /// Broken in a way a restart will not fix.
    Broken { detail: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire names are the contract. A rename here is a breaking change for
    /// every client, so it fails a test rather than a user's bar.
    #[test]
    fn requests_carry_the_names_clients_send() {
        let seek =
            serde_json::to_string(&Request::Transport(Transport::Seek { position_ms: 4200 }))
                .unwrap();
        assert_eq!(
            seek,
            r#"{"req":"transport","cmd":"seek","position_ms":4200}"#
        );

        let sub = serde_json::to_string(&Request::Subscribe).unwrap();
        assert_eq!(sub, r#"{"req":"subscribe"}"#);
    }

    #[test]
    fn a_request_round_trips_through_its_own_wire_form() {
        let sent = Request::Transport(Transport::SetRepeat {
            mode: RepeatMode::All,
        });
        let line = serde_json::to_string(&sent).unwrap();
        let back: Request = serde_json::from_str(&line).unwrap();
        assert!(matches!(
            back,
            Request::Transport(Transport::SetRepeat {
                mode: RepeatMode::All
            })
        ));
    }

    #[test]
    fn an_unknown_request_is_an_error_rather_than_a_default() {
        // A client from a newer version must fail loudly here, not be silently
        // read as something else — which is what an untagged enum would do.
        let bad = serde_json::from_str::<Request>(r#"{"req":"teleport"}"#);
        assert!(bad.is_err());
    }

    #[test]
    fn the_socket_lives_under_the_runtime_dir() {
        // Not `~`: a socket that outlives the session is a socket that answers
        // for a daemon which is not running.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000") };
        assert_eq!(
            socket_path().unwrap(),
            std::path::PathBuf::from("/run/user/1000/slipmat.sock")
        );
    }
}
