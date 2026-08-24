// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The daemon's mirror, and how it becomes something a client can draw.

use slipmat_core::ipc::{QueueItem, Snapshot, Stage};
use slipmat_core::player::PlayerState;
use slipmat_core::player::protocol::Tokens;

/// Everything the daemon knows, in one place.
pub struct Model {
    pub player: PlayerState,
    pub stage: Stage,
    /// Ours, not the sidecar's: MusicKit is told the volume and does not report
    /// one back, so this is the only record of it.
    pub volume: f64,
    /// Live for the process lifetime, never persisted (rule 7). Unused until
    /// the daemon serves the library, which is what needs them.
    pub tokens: Option<Tokens>,
}

impl Model {
    pub fn new() -> Self {
        Self {
            player: PlayerState::new(),
            stage: Stage::Connecting,
            volume: 1.0,
            tokens: None,
        }
    }

    /// What a client draws from.
    ///
    /// `position_ms` is interpolated rather than last-reported: MusicKit reports
    /// about once a second, and a bar redrawing at that rate visibly steps.
    pub fn snapshot(&self) -> Snapshot {
        let item = self.player.now_playing.as_ref();
        Snapshot {
            title: item.map(|i| i.title.clone()).unwrap_or_default(),
            artist: item.map(|i| i.artist.clone()).unwrap_or_default(),
            album: item.map(|i| i.album.clone()).unwrap_or_default(),
            // Artwork is a client's own business for now: the daemon has no
            // fetcher, and handing over a template would make every client
            // resolve Apple's URLs itself.
            art_path: None,
            position_ms: self.player.interpolated_position_ms(),
            duration_ms: self.player.duration_ms,
            playing: self.player.state.is_playing(),
            busy: self.player.state.is_busy(),
            volume: self.volume,
            shuffle: self.player.shuffle,
            repeat: self.player.repeat,
            can_next: self.player.has_next(),
            can_previous: self.player.has_previous(),
        }
    }

    pub fn queue(&self) -> (Vec<QueueItem>, usize) {
        (
            self.player.queue.iter().map(QueueItem::from).collect(),
            self.player.queue_position,
        )
    }
}
