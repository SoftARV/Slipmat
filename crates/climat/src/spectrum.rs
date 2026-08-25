// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The bars: what the audio actually looks like.
//!
//! **It listens rather than being told.** climat never touches the audio — the
//! sidecar owns the stream — so this reads the sink's *monitor* source, the
//! loopback every output exposes. That is how `cava` and every other visualiser
//! works, and it means the bars follow whatever is playing, including a track
//! the GTK window started.
//!
//! **PulseAudio, not PipeWire.** PipeWire ships `pipewire-pulse` and reports
//! itself as `PulseAudio (on PipeWire …)`, so one backend covers both servers
//! and every distro running either. `@DEFAULT_MONITOR@` is resolved by the
//! server, so this follows the default sink rather than pinning a device that
//! stops existing when somebody unplugs their headphones.

use libpulse_binding::sample::{Format, Spec};
use libpulse_binding::stream::Direction;
use libpulse_simple_binding::Simple;
use rustfft::{FftPlanner, num_complex::Complex};

/// How many bars are drawn.
pub const BARS: usize = 28;

/// Samples per frame. 2048 at 44.1kHz is ~46ms — around 21 frames a second,
/// which reads as motion without asking the terminal to repaint at audio rates.
const WINDOW: usize = 2048;
const RATE: u32 = 44_100;

/// Below this a frame counts as silence, and silence is not sent: a paused
/// player should not wake the draw loop twenty times a second to render
/// nothing.
const FLOOR: f32 = 0.002;

/// How fast a bar falls when the music stops holding it up. Winamp's falloff,
/// and the reason a spectrum reads as music rather than noise.
const DECAY: f32 = 0.82;

/// The decibel window the bars span.
///
/// **Tuned against real music, not a tone.** A 0dB ceiling is right for a pure
/// sine, where one bin holds everything; music spreads its energy over hundreds
/// of bins, so no single one gets near full scale and the bars sat in the
/// bottom third of their range. Measured on an ordinary track, per-band peaks
/// land around -45dB, which this puts in the middle.
const QUIET_DB: f32 = -70.0;
const LOUD_DB: f32 = -18.0;

/// Start listening. `None` if there is no audio server to listen to — the bars
/// simply never appear, which is the right failure for decoration.
pub fn start() -> Option<tokio::sync::mpsc::Receiver<[f32; BARS]>> {
    let spec = Spec {
        format: Format::F32le,
        channels: 1,
        rate: RATE,
    };
    if !spec.is_valid() {
        return None;
    }
    // Opened here rather than in the thread so a missing server is answered now
    // — the caller learns there are no bars instead of a thread failing quietly
    // somewhere behind the screen.
    let source = Simple::new(
        None,
        "climat",
        Direction::Record,
        Some("@DEFAULT_MONITOR@"),
        "visualiser",
        &spec,
        None,
        None,
    )
    .ok()?;

    // **One frame of depth.** A visualiser that queues is a visualiser running
    // late; dropping a frame the screen could not keep up with is exactly right.
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    std::thread::Builder::new()
        .name("climat-spectrum".into())
        .spawn(move || listen(source, tx))
        .ok()?;
    Some(rx)
}

fn listen(source: Simple, tx: tokio::sync::mpsc::Sender<[f32; BARS]>) {
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(WINDOW);
    // The Hann window, precomputed: without it every bar smears into its
    // neighbours and the whole thing looks like one lump moving.
    let hann: Vec<f32> = (0..WINDOW)
        .map(|i| {
            let t = i as f32 / (WINDOW - 1) as f32;
            0.5 - 0.5 * (std::f32::consts::TAU * t).cos()
        })
        .collect();

    let mut samples = vec![0f32; WINDOW];
    let mut bars = [0f32; BARS];
    let mut was_silent = false;

    loop {
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(samples.as_mut_ptr().cast::<u8>(), WINDOW * 4)
        };
        if source.read(bytes).is_err() {
            return; // the server went away; the bars stop, nothing else does
        }

        // **Look before transforming.** A paused player still delivers silence
        // down the monitor at real time, so without this the thread runs an FFT
        // twenty times a second to discover nothing is happening. A peak over
        // the raw samples costs a pass and answers the same question.
        let loudest = samples.iter().fold(0f32, |m, s| m.max(s.abs()));
        if loudest < FLOOR && bars.iter().all(|v| *v < FLOOR) {
            was_silent = true;
            continue;
        }

        let mut buf: Vec<Complex<f32>> = samples
            .iter()
            .zip(&hann)
            .map(|(s, w)| Complex { re: s * w, im: 0.0 })
            .collect();
        fft.process(&mut buf);

        let fresh = bands(&buf);
        let silent = fresh.iter().all(|v| *v < FLOOR);
        for (bar, new) in bars.iter_mut().zip(fresh) {
            *bar = new.max(*bar * DECAY);
        }

        // Send the frame that settles the bars at rest, then stop until there
        // is something to say again.
        if silent && was_silent {
            continue;
        }
        was_silent = silent && bars.iter().all(|v| *v < FLOOR);
        if tx.try_send(bars).is_err() && tx.is_closed() {
            return;
        }
    }
}

/// Fold the spectrum into bars, spaced by octave rather than by hertz.
///
/// A linear split puts almost everything in the first two bars — half the bins
/// of a 44.1kHz signal describe 11kHz and up, where music has very little. Ears
/// hear pitch logarithmically, so the bars are spaced that way too.
fn bands(spectrum: &[Complex<f32>]) -> [f32; BARS] {
    // Only the first half is meaningful; the rest mirrors it.
    let bins = spectrum.len() / 2;
    let lowest = 30.0f32;
    let highest = 16_000.0f32;
    let hz_per_bin = RATE as f32 / spectrum.len() as f32;

    let mut out = [0f32; BARS];
    for (i, slot) in out.iter_mut().enumerate() {
        let lo = lowest * (highest / lowest).powf(i as f32 / BARS as f32);
        let hi = lowest * (highest / lowest).powf((i + 1) as f32 / BARS as f32);
        let from = ((lo / hz_per_bin) as usize).min(bins - 1);
        let to = (((hi / hz_per_bin) as usize).max(from + 1)).min(bins);

        let peak = spectrum[from..to]
            .iter()
            .map(|c| c.norm())
            .fold(0f32, f32::max);
        // **Normalised by the window first.** An FFT's magnitudes scale with
        // the number of samples, so a bin carrying an ordinary signal comes out
        // in the tens — and a decibel scale built for 0..1 then clamps almost
        // every bar to full. That does not look like a bug in a test, it looks
        // like a solid block on screen.
        let level = peak / (spectrum.len() as f32 / 2.0);
        // Decibels, then squashed into 0..1. Amplitude alone spends the whole
        // range on the loudest bar and leaves the rest flat on the floor.
        let db = 20.0 * level.max(1e-9).log10();
        *slot = ((db - QUIET_DB) / (LOUD_DB - QUIET_DB)).clamp(0.0, 1.0);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_reads_as_empty_bars() {
        let quiet = vec![Complex { re: 0.0, im: 0.0 }; WINDOW];
        assert!(bands(&quiet).iter().all(|v| *v == 0.0));
    }

    #[test]
    fn a_tone_lights_its_own_band_and_not_the_others() {
        // The check that catches a wrong bin-to-bar mapping, which otherwise
        // looks plausible on screen — bars move, just not with the music.
        let hz = 1000.0f32;
        // Windowed, exactly as `listen` does. Without it a tone whose period
        // does not divide the window leaks across every bin, and the test
        // measures the leak rather than the tone.
        let mut buf: Vec<Complex<f32>> = (0..WINDOW)
            .map(|i| {
                let t = i as f32 / (WINDOW - 1) as f32;
                let hann = 0.5 - 0.5 * (std::f32::consts::TAU * t).cos();
                Complex {
                    re: 0.5 * hann * (std::f32::consts::TAU * hz * i as f32 / RATE as f32).sin(),
                    im: 0.0,
                }
            })
            .collect();
        FftPlanner::new().plan_fft_forward(WINDOW).process(&mut buf);

        let out = bands(&buf);
        assert!(
            out.iter().filter(|v| **v >= 1.0).count() <= 1,
            "bars are saturating: {out:?}"
        );
        let loudest = out
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| i)
            .unwrap();
        // Which bar 1kHz lands in, from the same octave spacing `bands` uses.
        let expected = (BARS as f32 * (hz / 30.0).log10() / (16_000.0f32 / 30.0).log10()) as usize;
        assert!(
            loudest.abs_diff(expected) <= 1,
            "1kHz lit bar {loudest}, expected about {expected}"
        );
    }
}
