// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The daemon's mirror, and how it becomes something a client can draw.

use slipmat_core::ipc::{QueueItem, Snapshot, Stage};
use slipmat_core::player::PlayerState;
use slipmat_core::player::protocol::Tokens;

use crate::library::Library;

/// Everything the daemon knows, in one place.
pub struct Model {
    pub player: PlayerState,
    pub stage: Stage,
    /// The desktop stream's volume, mirrored for clients while no stream exists.
    pub volume: f64,
    /// Live for the process lifetime, never persisted (rule 7). Unused until
    /// the daemon serves the library, which is what needs them.
    pub tokens: Option<Tokens>,
    pub library: Library,
    /// Ids MusicKit has refused. Remembered across runs so the first play of a
    /// list with a delisted track is not slow twice.
    pub dead_ids: std::collections::HashSet<String>,
    /// The current track's cover on disk, once fetched.
    pub art_path: Option<std::path::PathBuf>,
}

impl Model {
    pub fn new() -> Self {
        Self {
            player: PlayerState::new(),
            stage: Stage::Connecting,
            volume: 1.0,
            tokens: None,
            library: Library::from_cache(),
            dead_ids: slipmat_core::unplayable::load(),
            art_path: None,
        }
    }

    pub fn clear_account_state(&mut self) {
        self.player = PlayerState::new();
        self.tokens = None;
        self.library = Library::default();
        self.art_path = None;
    }

    pub fn current_item(&self) -> Option<&slipmat_core::player::protocol::Item> {
        self.player
            .now_playing
            .as_ref()
            .or_else(|| self.player.queue.get(self.player.queue_position))
    }

    pub fn duration_ms(&self) -> u64 {
        if self.player.now_playing.is_some() {
            self.player.duration_ms
        } else {
            self.current_item()
                .map(|item| item.duration_ms)
                .unwrap_or(0)
        }
    }

    /// What a client draws from.
    ///
    /// `position_ms` is interpolated rather than last-reported: MusicKit reports
    /// about once a second, and a bar redrawing at that rate visibly steps.
    pub fn snapshot(&self) -> Snapshot {
        // **A queue loaded but never started has no now-playing item** —
        // MusicKit only sets one when something begins, which is exactly the
        // state a restored session is in. The queue's own current entry is the
        // honest answer to "what is this player on", and answering it here
        // means no client has to work it out again.
        let item = self.current_item();
        Snapshot {
            // From the same item as the title beside it, which is the whole
            // point: one object, one answer.
            track_id: item.and_then(|i| i.catalog_id.clone().or_else(|| i.id.clone())),
            title: item.map(|i| i.title.clone()).unwrap_or_default(),
            artist: item.map(|i| i.artist.clone()).unwrap_or_default(),
            album: item.map(|i| i.album.clone()).unwrap_or_default(),
            // A local file, already fetched. A client never talks to Apple for
            // a cover, and never resolves a template itself.
            art_path: self.art_path.as_ref().map(|p| p.display().to_string()),
            // **Reported, not interpolated.** Clients extrapolate between
            // snapshots themselves — a bar redraws at 60fps against a 500ms
            // tick — and two interpolations of the same clock fight each other:
            // every tick looks like a reading that moved, so the client rebases
            // on it, and the slider walks backwards and forwards. MPRIS gets
            // the interpolated one, because a polled property has no other
            // chance to be current.
            position_ms: self.player.position_ms,
            duration_ms: self.duration_ms(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use slipmat_core::player::protocol::Item;

    #[test]
    fn a_selected_queue_item_completes_a_paused_snapshot() {
        let mut model = Model::new();
        model.player.queue = vec![
            Item::default(),
            Item {
                occurrence_id: "run:2".into(),
                catalog_id: Some("song-b".into()),
                title: "Restored song".into(),
                artist: "Restored artist".into(),
                album: "Restored album".into(),
                duration_ms: 180_000,
                ..Default::default()
            },
        ];
        model.player.queue_position = 1;
        model.player.position_ms = 55_000;

        let snapshot = model.snapshot();

        assert_eq!(snapshot.track_id.as_deref(), Some("song-b"));
        assert_eq!(snapshot.title, "Restored song");
        assert_eq!(snapshot.position_ms, 55_000);
        assert_eq!(snapshot.duration_ms, 180_000);
        assert!(!snapshot.playing);
    }
}
