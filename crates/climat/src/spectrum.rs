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

use libpulse_binding::def::BufferAttr;
use libpulse_binding::sample::{Format, Spec};
use libpulse_binding::stream::Direction;
use libpulse_simple_binding::Simple;
use rustfft::{FftPlanner, num_complex::Complex};

/// How many bands are analysed.
///
/// **Not how many bars are drawn.** The window decides that, and it changes
/// when somebody resizes the terminal — which the audio thread has no business
/// knowing about. So this is a generous fixed resolution and the drawing side
/// folds it down to whatever width it has.
pub const BANDS: usize = 96;

/// How much audio each transform looks at. 2048 at 44.1kHz is ~46ms, which is
/// what buys the frequency resolution — it is *not* what sets the frame rate.
///
/// **4096 is worse, which is not obvious.** It resolves the bottom of the
/// spectrum better — bands are spaced by octave, so the lowest few span a
/// handful of hertz and several share a bin at this size, drawing as one flat
/// step. But doubling the window halves the energy in each bin *and* doubles
/// the divisor below, and the ~6dB that costs takes the whole bass end under
/// the floor. Tried, measured, reverted.
const WINDOW: usize = 2048;

/// How much of it is new each frame.
///
/// **The two are separate, and tying them together is what made this feel
/// slow.** Reading a whole fresh window meant one frame per 46ms — 21 a second,
/// which reads as choppy beside a visualiser doing 60. Overlapping windows keep
/// the resolution and quadruple the rate: each frame slides 512 samples in and
/// transforms the last 2048, so the bars move every ~12ms.
const HOP: usize = 512;
const RATE: u32 = 44_100;

/// Below this a frame counts as silence, and silence is not sent: a paused
/// player should not wake the draw loop twenty times a second to render
/// nothing.
const FLOOR: f32 = 0.002;

/// The falloff: a bar drops to [`FALL_TO`] of its height in [`FALL_SECONDS`].
///
/// **This is what "responsive" actually means here**, and it is not the frame
/// rate. Measured while chasing exactly that: the thread produces 86 frames a
/// second and the screen draws 102, yet the bars still looked sluggish —
/// because a full second to fall makes them drift between levels instead of
/// punching. A quarter of a second is Winamp's, and it is the difference
/// between a spectrum and a lava lamp.
///
/// Expressed as a time rather than a per-frame factor, because a per-frame
/// constant silently means something different the moment `HOP` changes.
const FALL_TO: f32 = 0.02;
const FALL_SECONDS: f32 = 0.25;

/// The decibel window the bars span.
///
/// **Tuned against real music, not a tone.** A 0dB ceiling is right for a pure
/// sine, where one bin holds everything; music spreads its energy over hundreds
/// of bins, so no single one gets near full scale and the bars sat in the
/// bottom third of their range. Measured on an ordinary track, per-band peaks
/// land around -45dB, which this puts in the middle.
const QUIET_DB: f32 = -62.0;
const LOUD_DB: f32 = -26.0;

/// Start listening. `None` if there is no audio server to listen to — the bars
/// simply never appear, which is the right failure for decoration.
pub fn start() -> Option<tokio::sync::mpsc::Receiver<[f32; BANDS]>> {
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
    // **Ask for small fragments, or the server chooses.** Left to its defaults
    // PulseAudio hands a recording client audio in large chunks: `read` then
    // returns a burst of frames and blocks for a long time, and a depth-one
    // channel keeps only the last of each burst. Measured that way, the bars
    // reached the screen **1.9 times a second** — the frame rate of the
    // fragments, not of anything this code was doing.
    //
    // One hop per fragment is what makes delivery smooth. `!0` on the rest
    // means "your default", which is right for every field that is about
    // playback rather than capture.
    let attr = BufferAttr {
        maxlength: !0,
        tlength: !0,
        prebuf: !0,
        minreq: !0,
        fragsize: (HOP * std::mem::size_of::<f32>()) as u32,
    };
    let source = Simple::new(
        None,
        "climat",
        Direction::Record,
        Some("@DEFAULT_MONITOR@"),
        "visualiser",
        &spec,
        None,
        Some(&attr),
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

fn listen(source: Simple, tx: tokio::sync::mpsc::Sender<[f32; BANDS]>) {
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

    // The window slides over this; only `HOP` of it is new each time.
    let mut history = vec![0f32; WINDOW];
    let mut fresh_samples = vec![0f32; HOP];
    let mut bars = [0f32; BANDS];
    let mut was_silent = false;

    let hop_seconds = HOP as f32 / RATE as f32;
    let per_frame = FALL_TO.powf(hop_seconds / FALL_SECONDS);

    loop {
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(fresh_samples.as_mut_ptr().cast::<u8>(), HOP * 4)
        };
        if source.read(bytes).is_err() {
            return; // the server went away; the bars stop, nothing else does
        }
        history.copy_within(HOP.., 0);
        history[WINDOW - HOP..].copy_from_slice(&fresh_samples);

        // **Look before transforming.** A paused player still delivers silence
        // down the monitor at real time, so without this the thread runs an FFT
        // twenty times a second to discover nothing is happening. A peak over
        // the raw samples costs a pass and answers the same question.
        let loudest = history.iter().fold(0f32, |m, s| m.max(s.abs()));
        if loudest < FLOOR && bars.iter().all(|v| *v < FLOOR) {
            was_silent = true;
            continue;
        }

        let mut buf: Vec<Complex<f32>> = history
            .iter()
            .zip(&hann)
            .map(|(s, w)| Complex { re: s * w, im: 0.0 })
            .collect();
        fft.process(&mut buf);

        let fresh = bands(&buf);
        let silent = fresh.iter().all(|v| *v < FLOOR);
        for (bar, new) in bars.iter_mut().zip(fresh) {
            *bar = new.max(*bar * per_frame);
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
fn bands(spectrum: &[Complex<f32>]) -> [f32; BANDS] {
    // Only the first half is meaningful; the rest mirrors it.
    let bins = spectrum.len() / 2;
    let lowest = 30.0f32;
    let highest = 16_000.0f32;
    let hz_per_bin = RATE as f32 / spectrum.len() as f32;

    let mut out = [0f32; BANDS];
    for (i, slot) in out.iter_mut().enumerate() {
        let lo = lowest * (highest / lowest).powf(i as f32 / BANDS as f32);
        let hi = lowest * (highest / lowest).powf((i + 1) as f32 / BANDS as f32);
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
        // **Localised, not silent.** A tone spread over 96 bands lands in two
        // or three of them, and with a deliberately narrow decibel window it is
        // right that those reach the top. What would be wrong is the whole
        // spectrum reaching it — the saturation bug this test was written for.
        let lit = out.iter().filter(|v| **v > 0.0).count();
        assert!(
            (1..=6).contains(&lit),
            "a single tone lit {lit} of {BANDS} bands: {out:?}"
        );
        let loudest = out
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| i)
            .unwrap();
        // Which bar 1kHz lands in, from the same octave spacing `bands` uses.
        let expected = (BANDS as f32 * (hz / 30.0).log10() / (16_000.0f32 / 30.0).log10()) as usize;
        assert!(
            loudest.abs_diff(expected) <= 1,
            "1kHz lit bar {loudest}, expected about {expected}"
        );
    }
}
