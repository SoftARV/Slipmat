// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Volume, where the rest of the desktop keeps it.
//!
//! **Not MusicKit's.** The player has a volume of its own, and driving that
//! left two independent gains for one application: Slipmat's slider said 50%
//! while the system mixer said 100%, they multiplied, and only one of them was
//! where a Linux user goes to look. This drives the *stream* instead — the
//! sink-input the sidecar is playing through — so Slipmat's volume and the one
//! in the desktop's audio panel are the same number.
//!
//! **Persistence is the audio server's job, not ours.** The stream disappears
//! whenever playback stops for a while, which would otherwise mean tracking a
//! desired volume and re-applying it on every new stream. `module-stream-restore`
//! already does exactly that, keyed on `sink-input-by-application-name:Slipmat`
//! — verified by setting 40%, stopping until the stream went away, and starting
//! again to find it still at 40%.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;

use libpulse_binding::callbacks::ListResult;
use libpulse_binding::context::{Context, FlagSet, State};
use libpulse_binding::mainloop::standard::{IterateResult, Mainloop};
use libpulse_binding::volume::{ChannelVolumes, Volume};

/// What the sidecar calls itself to the audio server — `app.setName('Slipmat')`
/// in `main.js`, which is also what the desktop's mixer labels the stream.
const STREAM: &str = "Slipmat";

enum Ask {
    Set(f64),
}

/// A handle to the audio server, on its own thread.
///
/// PulseAudio's API is a mainloop and callbacks, and the daemon's is a
/// single-threaded tokio `LocalSet` — so rather than interleave them, this owns
/// a thread and takes messages. Every call is fire-and-forget except the read.
///
/// **The standard mainloop, not the threaded one.** The threaded mainloop runs
/// itself and expects `wait` to be woken by a `signal` from a state callback;
/// without one it blocks forever, which is exactly what the first version of
/// this did — it hung in `connect` and reported "no audio server" two seconds
/// later, having never asked. On a thread of our own there is nothing to
/// synchronise with, so driving `iterate` directly is both simpler and the
/// thing that works.
/// Stored as ten-thousandths so it can live in an atomic, with `u32::MAX`
/// meaning "no stream to ask".
const UNKNOWN: u32 = u32::MAX;

pub struct Mixer {
    tx: mpsc::Sender<Ask>,
    /// What the stream was last seen at.
    ///
    /// **Polled on the mixer's own thread, read for free on the daemon's.**
    /// The volume can change without Slipmat asking — somebody moves the
    /// slider in the desktop's audio panel — and a client that did not hear
    /// about it would show a different number again, which is the whole
    /// problem this moved to fix. Reading it across the socket would mean
    /// blocking the daemon on the audio server twice a second (rule 8), so the
    /// thread that is already there keeps this up to date instead.
    seen: Arc<AtomicU32>,
}

impl Mixer {
    /// `None` when there is no audio server to talk to, which is not fatal:
    /// the daemon keeps its own record and nothing else changes.
    pub fn start() -> Option<Self> {
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let seen = Arc::new(AtomicU32::new(UNKNOWN));
        let mine = seen.clone();
        std::thread::Builder::new()
            .name("slipmatd-mixer".into())
            .spawn(move || serve(rx, ready_tx, mine))
            .ok()?;
        // Wait for the context to connect before promising anything: a failure
        // here should read as "no mixer", not as a mixer that silently drops
        // everything sent to it.
        ready_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .ok()?
            .then_some(Self { tx, seen })
    }

    pub fn set(&self, volume: f64) {
        let _ = self.tx.send(Ask::Set(volume.clamp(0.0, 1.0)));
    }

    /// What the stream was last seen at, if there is one.
    ///
    /// `None` while nothing is playing — there is no stream to ask, and the
    /// daemon's own record is the answer until one exists. Free to call: it
    /// reads a number the mixer's thread keeps current, rather than blocking
    /// the daemon on the audio server twice a second (rule 8).
    pub fn current(&self) -> Option<f64> {
        match self.seen.load(Ordering::Relaxed) {
            UNKNOWN => None,
            v => Some(v as f64 / 10_000.0),
        }
    }
}

/// How often to look at the stream when nothing is being asked of it. Slow
/// enough to be nothing, quick enough that moving the desktop's own slider
/// shows up before somebody wonders whether it worked.
const POLL: std::time::Duration = std::time::Duration::from_millis(500);

fn serve(rx: mpsc::Receiver<Ask>, ready: mpsc::Sender<bool>, seen: Arc<AtomicU32>) {
    let Some((mut main, mut ctx)) = connect() else {
        tracing::info!("mixer: no audio server — volume stays the daemon's own");
        let _ = ready.send(false);
        return;
    };
    tracing::info!("mixer: connected to the audio server");
    let _ = ready.send(true);

    loop {
        match rx.recv_timeout(POLL) {
            Ok(Ask::Set(volume)) => set(&mut main, &mut ctx, volume),
            // Nothing asked: look at the stream, so a change made anywhere
            // else is noticed rather than waited for.
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let now = read(&mut main, &ctx)
                    .map(|v| (v * 10_000.0) as u32)
                    .unwrap_or(UNKNOWN);
                seen.store(now, Ordering::Relaxed);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn connect() -> Option<(Mainloop, Context)> {
    let mut main = Mainloop::new()?;
    let mut ctx = Context::new(&main, "slipmatd")?;
    ctx.connect(None, FlagSet::NOFLAGS, None).ok()?;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        pump(&mut main)?;
        match ctx.get_state() {
            State::Ready => return Some((main, ctx)),
            State::Failed | State::Terminated => return None,
            _ if std::time::Instant::now() > deadline => return None,
            _ => {}
        }
    }
}

/// One turn of the mainloop, blocking until something happens.
///
/// `None` when the loop has died, which is the only outcome worth acting on —
/// every other result is "carry on".
fn pump(main: &mut Mainloop) -> Option<()> {
    match main.iterate(true) {
        IterateResult::Success(_) => Some(()),
        IterateResult::Quit(_) | IterateResult::Err(_) => None,
    }
}

/// Run an introspection call to completion and hand back what it found.
///
/// Every operation here is asynchronous and answered from inside `iterate`, so
/// the shape is always the same: start it, turn the loop until it stops
/// running, read what the callback left behind.
fn ask<T: 'static>(
    main: &mut Mainloop,
    ctx: &Context,
    pick: impl Fn(&libpulse_binding::context::introspect::SinkInputInfo) -> Option<T> + 'static,
) -> Option<T> {
    let found = std::rc::Rc::new(std::cell::RefCell::new(None));
    let out = found.clone();
    let op = ctx.introspect().get_sink_input_info_list(move |result| {
        if let ListResult::Item(info) = result
            && is_ours(info.proplist.get_str("application.name").as_deref())
            && let Some(value) = pick(info)
        {
            *out.borrow_mut() = Some(value);
        }
    });
    while op.get_state() == libpulse_binding::operation::State::Running {
        pump(main)?;
    }
    found.borrow_mut().take()
}

fn set(main: &mut Mainloop, ctx: &mut Context, volume: f64) {
    tracing::debug!(volume, "mixer: set");
    let Some((index, channels)) = find(main, ctx) else {
        // Nothing playing: the audio server will restore this application's
        // volume onto the next stream by itself, so there is nothing to do and
        // nothing lost.
        tracing::debug!("mixer: no stream yet — the server will restore it");
        return;
    };
    let mut cv = ChannelVolumes::default();
    cv.set_len(channels);
    cv.set(channels, Volume((volume * Volume::NORMAL.0 as f64) as u32));

    let op = ctx.introspect().set_sink_input_volume(index, &cv, None);
    while op.get_state() == libpulse_binding::operation::State::Running {
        if pump(main).is_none() {
            return;
        }
    }
}

fn read(main: &mut Mainloop, ctx: &Context) -> Option<f64> {
    ask(main, ctx, |info| {
        Some(info.volume.max().0 as f64 / Volume::NORMAL.0 as f64)
    })
}

/// The sink-input's index and channel count, if the sidecar has one.
fn find(main: &mut Mainloop, ctx: &Context) -> Option<(u32, u8)> {
    ask(main, ctx, |info| Some((info.index, info.channel_map.len())))
}

fn is_ours(name: Option<&str>) -> bool {
    name == Some(STREAM)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_our_own_stream_is_touched() {
        // The mixer walks every sink-input on the machine. Matching loosely
        // here would mean turning down somebody's browser.
        assert!(is_ours(Some("Slipmat")));
        assert!(!is_ours(Some("Slipmat Helper")));
        assert!(!is_ours(Some("slipmat")));
        assert!(!is_ours(Some("Firefox")));
        assert!(!is_ours(None));
    }
}
