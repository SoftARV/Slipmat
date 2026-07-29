// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The mirror of the sidecar's playback state.
//!
//! **This is a projection, not a source of truth** (CLAUDE.md rule 3). The UI
//! never mutates it directly: a click sends a `Command`, and the change lands
//! here only when the sidecar echoes it back. That round trip is what keeps
//! Rust and MusicKit from disagreeing about what is playing — and it is why
//! `apply()` is the only way to write to this struct.
//!
//! The seek-bar helpers (`interpolated_position_ms`, `progress`) and the
//! volume/shuffle/repeat fields land in M2's Now Playing bar; they are here now
//! because their edge cases are what the tests below pin down. Hence the allow.
#![allow(dead_code)]

use std::time::Instant;

use super::protocol::{Event, Item, PlaybackState, Queue, RepeatMode};

#[derive(Debug, Default)]
pub struct PlayerState {
    pub state: PlaybackState,
    pub now_playing: Option<Item>,
    /// Queue as MusicKit reports it. Reconciled, never authored here.
    pub queue: Vec<Item>,
    pub queue_position: usize,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub volume: f64,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    /// When `position_ms` was last set, so the UI can interpolate between the
    /// sidecar's coarse `playbackTimeDidChange` ticks without lying when paused.
    last_tick: Option<Instant>,
}

impl PlayerState {
    pub fn new() -> Self {
        Self {
            volume: 1.0,
            ..Default::default()
        }
    }

    /// Fold one sidecar event into the mirror.
    ///
    /// Returns `true` when something the *MPRIS metadata* depends on changed,
    /// so `app.rs` knows whether to re-export properties. Position ticks alone
    /// return `false` — MPRIS `Position` is polled, not signalled, and emitting
    /// `PropertiesChanged` every second is what makes Shell applets stutter.
    pub fn apply(&mut self, event: &Event) -> bool {
        match event {
            Event::PlaybackState { state } => {
                self.state = *state;
                if !state.is_playing() {
                    self.last_tick = None;
                }
                true
            }
            Event::NowPlaying { item, queue } => {
                self.now_playing = item.clone();
                self.duration_ms = item.as_ref().map(|i| i.duration_ms).unwrap_or(0);
                self.position_ms = 0;
                self.last_tick = Some(Instant::now());
                self.apply_queue(queue);
                true
            }
            Event::Position {
                position_ms,
                duration_ms,
            } => {
                self.position_ms = *position_ms;
                if *duration_ms > 0 {
                    self.duration_ms = *duration_ms;
                }
                self.last_tick = Some(Instant::now());
                false
            }
            Event::Modes { shuffle, repeat } => {
                self.shuffle = *shuffle;
                self.repeat = *repeat;
                true
            }
            Event::Queue(queue) => {
                self.apply_queue(queue);
                true
            }
            _ => false,
        }
    }

    fn apply_queue(&mut self, queue: &Queue) {
        self.queue = queue.items.clone();
        // `None` (MusicKit's -1) means a queue is loaded but nothing is current
        // yet. Treat that as position 0 for display, but keep `has_previous`
        // honest via the same value — you cannot go back from "not started".
        let reported = queue.index().unwrap_or(0);
        self.queue_position = corrected_position(reported, &self.queue, self.now_playing.as_ref());
    }

    /// Move an item, before MusicKit has confirmed it.
    ///
    /// **Optimistic, and the projection is where that has to happen.** The
    /// queue view is rebuilt from this, so reordering locally there would be
    /// undone by the next sync — which arrives before MusicKit's echo does.
    ///
    /// `to` is the item's index *after* the move, which is what a drop means
    /// and what the sidecar's remove-then-insert produces. Verified against a
    /// real queue in both directions.
    ///
    /// The position needs no arithmetic: it is re-derived from `now_playing`,
    /// which a reorder never changes.
    pub fn move_item(&mut self, from: usize, to: usize) -> bool {
        if from == to || from >= self.queue.len() || to >= self.queue.len() {
            return false;
        }
        let item = self.queue.remove(from);
        self.queue.insert(to, item);
        self.queue_position =
            corrected_position(self.queue_position, &self.queue, self.now_playing.as_ref());
        true
    }

    /// Position interpolated to *now*, clamped to the track length.
    ///
    /// The sidecar only ticks a few times a second; without this the seek bar
    /// visibly steps. Clamping matters because a stale `last_tick` across a
    /// suspend/resume would otherwise report a position past the end.
    pub fn interpolated_position_ms(&self) -> u64 {
        let base = self.position_ms;
        if !self.state.is_playing() {
            return base;
        }
        let drift = self
            .last_tick
            .map(|t| t.elapsed().as_millis() as u64)
            .unwrap_or(0);
        let pos = base.saturating_add(drift);
        if self.duration_ms > 0 {
            pos.min(self.duration_ms)
        } else {
            pos
        }
    }

    /// 0.0–1.0 for the seek bar. Zero-length tracks must not divide by zero.
    pub fn progress(&self) -> f64 {
        if self.duration_ms == 0 {
            return 0.0;
        }
        (self.interpolated_position_ms() as f64 / self.duration_ms as f64).clamp(0.0, 1.0)
    }

    pub fn has_next(&self) -> bool {
        self.queue_position + 1 < self.queue.len()
    }

    pub fn has_previous(&self) -> bool {
        self.queue_position > 0
    }
}

/// Which index the playing track is *actually* at.
///
/// **MusicKit does not re-index its own position when the queue is edited.**
/// Measured: with index 32 playing, removing index 30 leaves the audio alone
/// and shifts the track to 31 — while `position` still reports 32, which now
/// names a different song. Moving an item across the current track does the
/// same. Verified against a real queue for both a removal and a splice.
///
/// So the reported index is a hint, and `now_playing` is the fact. Everything
/// downstream reads this: the row marker, the saved session's start index, and
/// the reload that recovers a dead playback — each of which would otherwise
/// name the wrong track after any edit above the one playing.
///
/// **The queue may hold the same track twice**, which is what Play Next and
/// Add to Queue are for (#88). So a search picks the candidate *nearest* the
/// reported index rather than the first: an edit shifts indices by a little,
/// never across the whole queue.
fn corrected_position(reported: usize, queue: &[Item], playing: Option<&Item>) -> usize {
    let id_of = |item: &Item| item.catalog_id.clone().or_else(|| item.id.clone());
    // Nothing to check against, or the report already agrees.
    let Some(wanted) = playing.and_then(id_of) else {
        return reported;
    };
    if queue.get(reported).and_then(id_of) == Some(wanted.clone()) {
        return reported;
    }
    queue
        .iter()
        .enumerate()
        .filter(|(_, item)| id_of(item) == Some(wanted.clone()))
        .min_by_key(|(i, _)| i.abs_diff(reported))
        .map(|(i, _)| i)
        // Not in the queue at all — a race between the edit and the event.
        // Keeping the report is no worse than inventing a number.
        .unwrap_or(reported)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(titles: &[&str]) -> Vec<Item> {
        titles
            .iter()
            .map(|t| Item {
                id: Some((*t).to_owned()),
                title: (*t).to_owned(),
                ..Item::default()
            })
            .collect()
    }

    #[test]
    fn an_optimistic_move_keeps_the_marker_on_the_playing_track() {
        let mut s = PlayerState::new();
        let items = q(&["a", "b", "c", "d", "e"]);
        s.apply(&Event::NowPlaying {
            item: Some(items[2].clone()),
            queue: Queue {
                position: 2,
                items: items.clone(),
            },
        });

        // Drag "a" from the top to the end. "c" is playing and slides up to 1.
        assert!(s.move_item(0, 4));
        assert_eq!(
            s.queue.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(),
            ["b", "c", "d", "e", "a"]
        );
        assert_eq!(
            s.queue_position, 1,
            "the marker follows the track, not the index"
        );

        // And back, which puts everything where it started.
        assert!(s.move_item(4, 0));
        assert_eq!(
            s.queue.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(),
            ["a", "b", "c", "d", "e"]
        );
        assert_eq!(s.queue_position, 2);
    }

    #[test]
    fn a_move_that_cannot_happen_is_refused() {
        let mut s = PlayerState::new();
        s.apply(&Event::Queue(Queue {
            position: 0,
            items: q(&["a", "b"]),
        }));
        assert!(!s.move_item(0, 0), "a move to itself is not a move");
        assert!(!s.move_item(0, 9), "out of range");
        assert!(!s.move_item(9, 0), "out of range");
        assert_eq!(s.queue.len(), 2, "and nothing was disturbed");
    }

    #[test]
    fn the_projection_re_finds_the_playing_track_after_an_edit() {
        // The measured bug, end to end through `apply` rather than against the
        // helper — so this fails if the correction is ever unwired, which
        // testing `corrected_position` alone would not catch.
        let mut s = PlayerState::new();
        let items = q(&["a", "b", "c", "d"]);
        s.apply(&Event::NowPlaying {
            item: Some(items[2].clone()),
            queue: Queue {
                position: 2,
                items: items.clone(),
            },
        });
        assert_eq!(s.queue_position, 2, "baseline");

        // "a" is removed. MusicKit shifts the items and does *not* re-index:
        // it still says 2, which now names "d".
        s.apply(&Event::Queue(Queue {
            position: 2,
            items: q(&["b", "c", "d"]),
        }));
        assert_eq!(
            s.queue_position, 1,
            "the playing track moved to 1; a stale report said 2"
        );
    }

    #[test]
    fn a_report_that_agrees_is_taken_as_it_stands() {
        let items = q(&["a", "b", "c"]);
        let playing = items[1].clone();
        assert_eq!(corrected_position(1, &items, Some(&playing)), 1);
    }

    #[test]
    fn removing_a_track_above_the_playing_one_is_corrected() {
        // The measured bug: playing index 2, something above it is removed, and
        // MusicKit still reports 2 while the track is now at 1.
        let items = q(&["a", "c", "d"]);
        let playing = Item {
            id: Some("c".into()),
            title: "c".into(),
            ..Item::default()
        };
        assert_eq!(corrected_position(2, &items, Some(&playing)), 1);
    }

    #[test]
    fn a_duplicate_track_resolves_to_the_nearest_copy() {
        // Play Next and Add to Queue both put the same song in twice (#88), so
        // the first match is the wrong answer — an edit shifts an index by a
        // little, never across the whole queue.
        let items = q(&["x", "dup", "y", "z", "dup"]);
        let playing = Item {
            id: Some("dup".into()),
            title: "dup".into(),
            ..Item::default()
        };
        assert_eq!(corrected_position(4, &items, Some(&playing)), 4);
        assert_eq!(corrected_position(2, &items, Some(&playing)), 1);
    }

    #[test]
    fn nothing_playing_leaves_the_report_alone() {
        // Restoring a session loads a queue with no current item; there is
        // nothing to check against and no reason to distrust the report.
        assert_eq!(corrected_position(7, &q(&["a", "b"]), None), 7);
    }

    #[test]
    fn a_track_that_has_left_the_queue_keeps_the_report() {
        // A race between the edit and the event. Inventing a number would be
        // worse than keeping the one we were given.
        let items = q(&["a", "b"]);
        let gone = Item {
            id: Some("vanished".into()),
            title: "vanished".into(),
            ..Item::default()
        };
        assert_eq!(corrected_position(1, &items, Some(&gone)), 1);
    }

    fn item(title: &str, duration_ms: u64) -> Item {
        Item {
            title: title.to_owned(),
            duration_ms,
            ..Default::default()
        }
    }

    #[test]
    fn now_playing_resets_position_and_takes_duration() {
        let mut s = PlayerState::new();
        s.position_ms = 90_000;
        s.apply(&Event::NowPlaying {
            item: Some(item("Heart of the Sunrise", 665_000)),
            queue: Queue::default(),
        });
        assert_eq!(s.position_ms, 0, "a new track starts at zero");
        assert_eq!(s.duration_ms, 665_000);
    }

    #[test]
    fn position_events_do_not_request_an_mpris_refresh() {
        let mut s = PlayerState::new();
        let changed = s.apply(&Event::Position {
            position_ms: 1_000,
            duration_ms: 0,
        });
        assert!(
            !changed,
            "per-second ticks must not signal PropertiesChanged"
        );
    }

    #[test]
    fn a_zero_duration_track_does_not_divide_by_zero() {
        let s = PlayerState::new();
        assert_eq!(s.progress(), 0.0);
    }

    #[test]
    fn position_never_runs_past_the_end() {
        let mut s = PlayerState::new();
        s.apply(&Event::PlaybackState {
            state: PlaybackState::Playing,
        });
        s.apply(&Event::Position {
            position_ms: 500_000,
            duration_ms: 500_000,
        });
        // Even if the clock drifts (suspend/resume), the bar can't overrun.
        assert!(s.interpolated_position_ms() <= 500_000);
        assert!(s.progress() <= 1.0);
    }

    #[test]
    fn paused_position_does_not_drift() {
        let mut s = PlayerState::new();
        s.apply(&Event::Position {
            position_ms: 42_000,
            duration_ms: 300_000,
        });
        s.apply(&Event::PlaybackState {
            state: PlaybackState::Paused,
        });
        assert_eq!(s.interpolated_position_ms(), 42_000);
    }

    #[test]
    fn queue_navigation_edges() {
        let mut s = PlayerState::new();
        s.apply(&Event::Queue(Queue {
            position: 0,
            items: vec![item("a", 1), item("b", 1)],
        }));
        assert!(s.has_next());
        assert!(!s.has_previous(), "nothing before the first track");

        // The -1 case: queue loaded, nothing current yet.
        s.apply(&Event::Queue(Queue {
            position: -1,
            items: vec![item("a", 1), item("b", 1)],
        }));
        assert_eq!(s.queue_position, 0);
        assert!(!s.has_previous(), "cannot go back from 'not started'");

        s.apply(&Event::Queue(Queue {
            position: 1,
            items: vec![item("a", 1), item("b", 1)],
        }));
        assert!(!s.has_next(), "nothing after the last track");
        assert!(s.has_previous());
    }
}
