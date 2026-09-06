// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! What this client believes the player is doing.
//!
//! Rule 3, one hop further out. It used to mirror MusicKit through the sidecar;
//! it now mirrors the daemon, which mirrors MusicKit. **Nothing here is ever
//! written by a click** — a button sends a request and waits to be told, so the
//! UI can never claim a state the player is not in.

use slipmat_core::ipc::{QueueItem, Snapshot, Stage};
use slipmat_core::player::protocol::RepeatMode;

#[derive(Debug, Default)]
pub struct Mirror {
    pub snap: Snapshot,
    pub queue: Vec<QueueItem>,
    pub queue_position: usize,
    pub stage: Option<Stage>,
}

impl Mirror {
    pub fn clear_account_state(&mut self) {
        let stage = self.stage.take();
        *self = Self {
            stage,
            ..Self::default()
        };
    }

    /// What is playing, or `None` when nothing is.
    ///
    /// A title is the honest test: the daemon sends a default snapshot before
    /// anything is loaded, and every other field is defaulted in it too.
    pub fn now_playing(&self) -> Option<&Snapshot> {
        (!self.snap.title.is_empty()).then_some(&self.snap)
    }

    /// Whether there is a track after the current one.
    ///
    /// Asked of the queue rather than carried in the snapshot, because a queue
    /// event and a snapshot arrive separately and the buttons must agree with
    /// the list on screen rather than with whichever landed last.
    pub fn has_next(&self) -> bool {
        self.queue_position + 1 < self.queue.len()
    }

    pub fn has_previous(&self) -> bool {
        self.queue_position > 0 && !self.queue.is_empty()
    }

    pub fn is_playing(&self) -> bool {
        self.snap.playing
    }

    /// Still working towards audio. Not paused, and a caller that treats it as
    /// paused draws a play button over a track that is about to start.
    pub fn is_busy(&self) -> bool {
        self.snap.busy
    }

    pub fn shuffle(&self) -> bool {
        self.snap.shuffle
    }

    pub fn repeat(&self) -> RepeatMode {
        self.snap.repeat
    }

    /// Where a row sits now, from where it sat and what it was.
    ///
    /// **The position is the key, the id is the check** (#88): a queue may hold
    /// the same track twice, so resolving by id alone finds the first copy and
    /// acts on that instead. If the queue moved since the click the position is
    /// wrong, and searching by id is the better wrong answer.
    pub fn index_at(&self, at: usize, id: &str) -> Option<usize> {
        match self.queue.get(at) {
            Some(item) if item.id.as_deref() == Some(id) => Some(at),
            _ => self
                .queue
                .iter()
                .position(|item| item.id.as_deref() == Some(id)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue(ids: &[&str]) -> Vec<QueueItem> {
        ids.iter()
            .map(|id| QueueItem {
                id: Some((*id).to_owned()),
                ..Default::default()
            })
            .collect()
    }

    #[test]
    fn a_duplicated_track_resolves_to_the_copy_that_was_clicked() {
        // Play Next inserts a track a queue already holds, so this is ordinary
        // rather than exotic (#88).
        let m = Mirror {
            queue: queue(&["a", "b", "a"]),
            ..Default::default()
        };
        assert_eq!(m.index_at(2, "a"), Some(2));
        assert_eq!(m.index_at(0, "a"), Some(0));
    }

    #[test]
    fn a_moved_queue_falls_back_to_searching_by_id() {
        let m = Mirror {
            queue: queue(&["a", "b"]),
            ..Default::default()
        };
        assert_eq!(
            m.index_at(9, "b"),
            Some(1),
            "position stale, id still right"
        );
    }

    #[test]
    fn the_transport_agrees_with_the_list_rather_than_the_snapshot() {
        // `has_next` is derived from the queue on purpose: a queue event and a
        // snapshot arrive separately, and a Next button that lights up before
        // the list has the track is a button that does nothing.
        let m = Mirror {
            queue: queue(&["a", "b", "c"]),
            queue_position: 2,
            ..Default::default()
        };
        assert!(!m.has_next());
        assert!(m.has_previous());
    }

    #[test]
    fn signed_out_clears_the_player_projection() {
        let mut m = Mirror {
            snap: Snapshot {
                title: "Old song".into(),
                playing: true,
                ..Default::default()
            },
            queue: queue(&["a", "b"]),
            queue_position: 1,
            stage: Some(Stage::SignedOut),
        };

        m.clear_account_state();

        assert!(m.snap.title.is_empty());
        assert!(!m.snap.playing);
        assert!(m.queue.is_empty());
        assert_eq!(m.queue_position, 0);
        assert_eq!(m.stage, Some(Stage::SignedOut));
    }

    #[test]
    fn a_paused_restored_snapshot_keeps_its_position_and_duration() {
        let mirror = Mirror {
            snap: Snapshot {
                title: "Restored song".into(),
                position_ms: 55_000,
                duration_ms: 180_000,
                playing: false,
                ..Default::default()
            },
            ..Default::default()
        };

        let restored = mirror.now_playing().expect("restored track");
        assert_eq!(restored.position_ms, 55_000);
        assert_eq!(restored.duration_ms, 180_000);
        assert!(!restored.playing);
    }
}
