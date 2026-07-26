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
        self.queue_position = queue.index().unwrap_or(0);
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

#[cfg(test)]
mod tests {
    use super::*;

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
