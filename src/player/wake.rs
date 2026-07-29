// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Noticing that the machine was asleep, without asking anyone.
//!
//! **Linux Widevine has no persistent licences**, which is the same platform
//! limit that makes offline playback impossible — and it means a decrypt
//! session cannot survive a suspend. What comes back is a loaded item that will
//! never make sound again: `play()` on it is a well-formed call that resolves
//! and does nothing, while loading a *different* track fetches a fresh licence
//! and works. That is exactly the shape of the bug as reported — paused, slept,
//! and then play did nothing until the track was changed.
//!
//! Detecting the suspend needs no D-Bus and no permission, because the kernel
//! keeps two clocks that disagree about it. `CLOCK_MONOTONIC` stops while
//! suspended; the wall clock does not. Measured on the developer's laptop, mid
//! session:
//!
//! ```text
//! CLOCK_MONOTONIC:     43805.01s
//! CLOCK_BOOTTIME:      67108.12s     <- the difference is 6.5 hours of sleep
//! ```
//!
//! Rust documents `Instant` as `clock_gettime(CLOCK_MONOTONIC)` on UNIX, so
//! `SystemTime` minus `Instant` over the same interval **is** the time spent
//! suspended.

use std::time::{Duration, Instant, SystemTime};

/// How much unaccounted wall-clock time counts as having been asleep.
///
/// The position tick is 500ms and the two clocks otherwise agree to within
/// scheduling jitter, so anything at this scale is a suspend — or a clock step
/// large enough that re-seating the track is a reasonable thing to do anyway.
const SLEPT: Duration = Duration::from_secs(20);

/// The two clocks, read together.
#[derive(Debug, Clone, Copy)]
pub struct Clocks {
    monotonic: Instant,
    wall: SystemTime,
}

impl Clocks {
    pub fn now() -> Self {
        Self {
            monotonic: Instant::now(),
            wall: SystemTime::now(),
        }
    }

    /// How long the machine was suspended between `self` and `later`.
    ///
    /// Zero for every ordinary interval. The wall clock going *backwards* — an
    /// NTP correction — reads as zero rather than as a negative sleep, because
    /// `duration_since` fails on it and there is nothing to recover from.
    pub fn slept_since(&self, later: &Self) -> Duration {
        let wall = later.wall.duration_since(self.wall).unwrap_or_default();
        let monotonic = later.monotonic.duration_since(self.monotonic);
        wall.saturating_sub(monotonic)
    }
}

/// Was the gap between these two readings a suspend?
pub fn woke(before: &Clocks, after: &Clocks) -> bool {
    before.slept_since(after) >= SLEPT
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a pair of readings that differ by exactly these amounts, so the
    /// arithmetic can be tested without suspending a machine.
    fn pair(wall: Duration, monotonic: Duration) -> (Clocks, Clocks) {
        let before = Clocks::now();
        let after = Clocks {
            monotonic: before.monotonic + monotonic,
            wall: before.wall + wall,
        };
        (before, after)
    }

    #[test]
    fn an_ordinary_tick_is_not_a_suspend() {
        // Both clocks advance together, give or take scheduling jitter.
        let (a, b) = pair(Duration::from_millis(503), Duration::from_millis(500));
        assert_eq!(a.slept_since(&b), Duration::from_millis(3));
        assert!(!woke(&a, &b));
    }

    #[test]
    fn wall_clock_running_on_alone_is_a_suspend() {
        // Ten minutes of wall time against half a second of monotonic: the
        // machine was asleep for the difference.
        let (a, b) = pair(Duration::from_secs(600), Duration::from_millis(500));
        assert!(woke(&a, &b));
        assert_eq!(a.slept_since(&b), Duration::from_millis(599_500));
    }

    #[test]
    fn a_short_stall_is_not_a_suspend() {
        // A busy machine, a slow disk, a GC pause: the tick is late and both
        // clocks are late with it. Re-seating the track over that would be a
        // gap in the music to fix a problem nobody had.
        let (a, b) = pair(Duration::from_secs(5), Duration::from_secs(5));
        assert!(!woke(&a, &b));
    }

    #[test]
    fn the_clock_going_backwards_is_not_a_negative_sleep() {
        // An NTP step the other way. `duration_since` fails rather than
        // wrapping, and there is nothing to recover from.
        let before = Clocks::now();
        let after = Clocks {
            monotonic: before.monotonic + Duration::from_millis(500),
            wall: before.wall - Duration::from_secs(60),
        };
        assert_eq!(before.slept_since(&after), Duration::ZERO);
        assert!(!woke(&before, &after));
    }
}
