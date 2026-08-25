// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Noticing a sidecar that is alive and not working.
//!
//! Rule 6 supervises a sidecar that *dies*. This is the other half: one that
//! stays up and stops playing. Found after a 13-hour suspend — every signal
//! said healthy (ten Electron processes, `stage: ready`, `playing: true`) while
//! MusicKit answered no commands, reported the same position for twelve hours,
//! and had no PipeWire stream at all. `play` was correctly ignored, because as
//! far as the daemon knew it was already playing.
//!
//! The tell is free: the daemon already reads a reported position twice a
//! second, and a player that claims to be playing must move it.

use std::rc::Rc;
use std::time::{Duration, Instant};

use slipmat_core::ipc::Event;

use crate::serve::Daemon;

/// How long a *playing* sidecar may report the same position before it counts
/// as wedged.
///
/// Only `Playing` and `Seeking` reach here — `Loading`, `Waiting` and `Stalled`
/// are not `is_playing`, so buffering a slow track can never trip this. That is
/// what lets the window be short enough to be useful.
const STALL_AFTER: Duration = Duration::from_secs(12);

#[derive(Default)]
pub struct Watch {
    /// The last position seen, and when it last changed.
    seen: Option<(u64, Instant)>,
}

impl Watch {
    /// Not playing, or no sidecar to watch: forget what was seen, so coming
    /// back from a pause never counts the paused time as a stall.
    pub fn disarm(&mut self) {
        self.seen = None;
    }

    /// Has the position stopped moving for too long?
    pub fn stalled(&mut self, position_ms: u64) -> bool {
        match self.seen {
            Some((at, since)) if at == position_ms => since.elapsed() >= STALL_AFTER,
            _ => {
                self.seen = Some((position_ms, Instant::now()));
                false
            }
        }
    }
}

/// Called on every position tick.
pub fn check(daemon: &Rc<Daemon>, watch: &mut Watch) {
    // No sidecar means the supervisor already has it — respawning, or waiting
    // to be asked. Watching would only race it.
    if daemon.sidecar.borrow().is_none() {
        watch.disarm();
        return;
    }

    // Read and release: `restart` borrows the same cell.
    let (playing, position_ms) = {
        let model = daemon.model.borrow();
        (model.player.state.is_playing(), model.player.position_ms)
    };
    if !playing {
        watch.disarm();
        return;
    }
    if !watch.stalled(position_ms) {
        return;
    }

    watch.disarm();
    tracing::error!(
        position_ms,
        stalled_for = ?STALL_AFTER,
        "the sidecar says it is playing but the position has not moved — restarting it"
    );
    // **Told, not just fixed.** The restart resumes *paused* (a queue that is
    // loaded but never started has no current item), so somebody coming back to
    // a stopped player deserves to know why rather than guessing.
    daemon.publish(Event::Error {
        detail: "Playback stopped responding — the player was restarted".into(),
    });
    // **Killed, not asked.** Closing stdin is how the idle drop puts a healthy
    // sidecar down, and it is useless here: the child has stopped listening, so
    // it never notices and the supervisor never wakes. `kill` takes the whole
    // process group, the reader task gets its EOF, and `Died` goes out — the
    // same path a crash takes. `idle` is deliberately *not* set: this is a
    // fault, not a sidecar put down on purpose, so it must not wait to be asked.
    if let Some(handle) = daemon.sidecar.borrow().as_ref() {
        handle.kill();
    }
    daemon.sidecar.borrow_mut().take();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_moving_position_never_stalls() {
        let mut watch = Watch::default();
        for ms in (0..60_000).step_by(500) {
            assert!(!watch.stalled(ms));
        }
    }

    #[test]
    fn a_frozen_position_stalls_only_after_the_window() {
        let mut watch = Watch::default();
        assert!(
            !watch.stalled(30_000),
            "the first reading cannot be a stall"
        );
        assert!(!watch.stalled(30_000), "nor a second one moments later");

        // Reach back in time rather than sleeping twelve seconds in a test.
        watch.seen = Some((30_000, Instant::now() - STALL_AFTER));
        assert!(watch.stalled(30_000));
    }

    #[test]
    fn a_pause_does_not_accumulate_towards_a_stall() {
        // The failure this guards: pausing for a minute and pressing play would
        // otherwise look like a minute of frozen position and restart a sidecar
        // that is working perfectly.
        let mut watch = Watch::default();
        watch.stalled(30_000);
        watch.seen = Some((30_000, Instant::now() - STALL_AFTER));
        watch.disarm();
        assert!(!watch.stalled(30_000), "paused time was counted as a stall");
    }

    #[test]
    fn a_track_that_restarts_at_the_same_position_rearms() {
        // Two different tracks can report the same millisecond. What matters is
        // that the clock is moving, and `disarm` on any non-playing state is
        // what keeps a boundary from being read as a freeze.
        let mut watch = Watch::default();
        watch.stalled(1_000);
        watch.disarm();
        watch.stalled(1_000);
        assert!(!watch.stalled(1_000));
    }
}
