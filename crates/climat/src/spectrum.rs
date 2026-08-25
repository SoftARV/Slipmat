// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The bars: what the audio actually looks like.
//!
//! **It listens rather than being told.** climat never touches the audio — the
//! sidecar owns the stream — so this reads a monitor source, the loopback every
//! output exposes. That is how `cava` and every other visualiser works, and it
//! means the bars follow whatever is playing, including a track the GTK window
//! started.
//!
//! **But only Slipmat's audio.** A sink's monitor carries everything mixed
//! together, so the bars jumped for a notification chime while nothing was
//! playing. `pa_stream_set_monitor_stream` narrows a record stream to one
//! sink-input, which is the difference between watching the speakers and
//! watching the player. It is only on the asynchronous API, which is why this
//! runs a mainloop rather than the three lines the simple API would have been.
//!
//! **PulseAudio, not PipeWire.** PipeWire ships `pipewire-pulse` and reports
//! itself as `PulseAudio (on PipeWire …)`, so one backend covers both servers
//! and every distro running either. `@DEFAULT_MONITOR@` is resolved by the
//! server, so this follows the default sink rather than pinning a device that
//! stops existing when somebody unplugs their headphones.

use libpulse_binding::callbacks::ListResult;
use libpulse_binding::context::{Context, FlagSet, State};
use libpulse_binding::def::BufferAttr;
use libpulse_binding::mainloop::standard::{IterateResult, Mainloop};
use libpulse_binding::sample::{Format, Spec};
use libpulse_binding::stream::{
    FlagSet as StreamFlagSet, PeekResult, State as StreamState, Stream,
};
use rustfft::{FftPlanner, num_complex::Complex};

/// What the sidecar calls itself to the audio server — `app.setName('Slipmat')`
/// in `main.js`.
const STREAM: &str = "Slipmat";

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
    // **One frame of depth.** A visualiser that queues is a visualiser running
    // late; dropping a frame the screen could not keep up with is exactly right.
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    std::thread::Builder::new()
        .name("climat-spectrum".into())
        .spawn(move || listen(tx))
        .ok()?;
    Some(rx)
}

/// How long to wait before looking for Slipmat's stream again.
///
/// It comes and goes: the sidecar's sink-input disappears when playback stops
/// for a while and a new one, with a new index, appears when it resumes. So
/// this is not startup logic — it is the ordinary state of things.
const RETRY: std::time::Duration = std::time::Duration::from_millis(700);

fn listen(tx: tokio::sync::mpsc::Sender<[f32; BANDS]>) {
    let Some((mut main, mut ctx)) = connect() else {
        return;
    };
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

    loop {
        let Some(mut stream) = follow(&mut main, &mut ctx) else {
            // Nothing of ours is playing. The bars are already empty, so this
            // costs a sleep and nothing else.
            if tx.is_closed() {
                return;
            }
            std::thread::sleep(RETRY);
            continue;
        };
        if watch(&mut main, &mut stream, &fft, &hann, &tx).is_none() {
            return;
        }
        let _ = stream.disconnect();
    }
}

/// A record stream carrying **only** the sidecar's audio, if it is playing.
fn follow(main: &mut Mainloop, ctx: &mut Context) -> Option<Stream> {
    let (sink_input, sink) = ask(main, ctx, |info| Some((info.index, info.sink)))?;
    let monitor = monitor_of(main, ctx, sink)?;

    let spec = Spec {
        format: Format::F32le,
        channels: 1,
        rate: RATE,
    };
    let mut stream = Stream::new(ctx, "visualiser", &spec, None)?;
    // **Before connecting, not after.** This is what turns "the speakers" into
    // "this player"; set afterwards it does nothing and the bars go back to
    // dancing for notification chimes.
    stream.set_monitor_stream(sink_input).ok()?;

    // Ask for small fragments, or the server chooses. Left to its defaults
    // PulseAudio hands a recording client audio in large chunks: reads then
    // arrive in bursts and a depth-one channel keeps only the last of each,
    // which measured at 1.9 frames a second on screen.
    let attr = BufferAttr {
        maxlength: !0,
        tlength: !0,
        prebuf: !0,
        minreq: !0,
        fragsize: (HOP * std::mem::size_of::<f32>()) as u32,
    };
    stream
        .connect_record(Some(&monitor), Some(&attr), StreamFlagSet::ADJUST_LATENCY)
        .ok()?;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        pump(main)?;
        match stream.get_state() {
            StreamState::Ready => return Some(stream),
            StreamState::Failed | StreamState::Terminated => return None,
            _ if std::time::Instant::now() > deadline => return None,
            _ => {}
        }
    }
}

/// Read from the stream until it ends or nobody is listening.
fn watch(
    main: &mut Mainloop,
    stream: &mut Stream,
    fft: &std::sync::Arc<dyn rustfft::Fft<f32>>,
    hann: &[f32],
    tx: &tokio::sync::mpsc::Sender<[f32; BANDS]>,
) -> Option<()> {
    // The window slides over this; only `HOP` of it is new each time.
    let mut history = vec![0f32; WINDOW];
    let mut bars = [0f32; BANDS];
    let mut was_silent = false;
    let hop_seconds = HOP as f32 / RATE as f32;
    let per_frame = FALL_TO.powf(hop_seconds / FALL_SECONDS);

    loop {
        pump(main)?;
        if !matches!(stream.get_state(), StreamState::Ready) {
            return Some(()); // the stream went away; look for the next one
        }
        if tx.is_closed() {
            return None;
        }
        if stream
            .readable_size()
            .is_none_or(|n| n < HOP * std::mem::size_of::<f32>())
        {
            continue;
        }
        let Some(fresh) = take(stream) else {
            return Some(());
        };
        if fresh.is_empty() {
            continue;
        }

        let new = fresh.len().min(WINDOW);
        history.copy_within(new.., 0);
        history[WINDOW - new..].copy_from_slice(&fresh[fresh.len() - new..]);

        // **Look before transforming.** A paused player still delivers silence
        // in real time, so a peak over the raw samples costs a pass and answers
        // the same question as an FFT.
        let loudest = history.iter().fold(0f32, |m, s| m.max(s.abs()));
        if loudest < FLOOR && bars.iter().all(|v| *v < FLOOR) {
            was_silent = true;
            continue;
        }

        let mut buf: Vec<Complex<f32>> = history
            .iter()
            .zip(hann)
            .map(|(s, w)| Complex { re: s * w, im: 0.0 })
            .collect();
        fft.process(&mut buf);

        let fresh_bands = bands(&buf);
        let silent = fresh_bands.iter().all(|v| *v < FLOOR);
        for (bar, level) in bars.iter_mut().zip(fresh_bands) {
            *bar = level.max(*bar * per_frame);
        }
        if silent && was_silent {
            continue;
        }
        was_silent = silent && bars.iter().all(|v| *v < FLOOR);
        if tx.try_send(bars).is_err() && tx.is_closed() {
            return None;
        }
    }
}

/// Drain what the server has, as `f32` samples.
fn take(stream: &mut Stream) -> Option<Vec<f32>> {
    let mut out = Vec::new();
    loop {
        match stream.peek() {
            Ok(PeekResult::Data(bytes)) => {
                out.extend(
                    bytes
                        .chunks_exact(4)
                        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])),
                );
                stream.discard().ok()?;
            }
            // A hole is the server telling us it dropped audio rather than
            // stall; skipping it is the only sane answer for a visualiser.
            Ok(PeekResult::Hole(_)) => stream.discard().ok()?,
            Ok(PeekResult::Empty) => return Some(out),
            Err(_) => return None,
        }
    }
}

fn connect() -> Option<(Mainloop, Context)> {
    let mut main = Mainloop::new()?;
    let mut ctx = Context::new(&main, "climat")?;
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

/// One turn of the mainloop. `None` when it has died.
fn pump(main: &mut Mainloop) -> Option<()> {
    match main.iterate(true) {
        IterateResult::Success(_) => Some(()),
        IterateResult::Quit(_) | IterateResult::Err(_) => None,
    }
}

/// Find the sidecar's sink-input.
fn ask<T: 'static>(
    main: &mut Mainloop,
    ctx: &Context,
    pick: impl Fn(&libpulse_binding::context::introspect::SinkInputInfo) -> Option<T> + 'static,
) -> Option<T> {
    let found = std::rc::Rc::new(std::cell::RefCell::new(None));
    let out = found.clone();
    let op = ctx.introspect().get_sink_input_info_list(move |result| {
        if let ListResult::Item(info) = result
            && info.proplist.get_str("application.name").as_deref() == Some(STREAM)
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

/// The name of the monitor source for the sink our stream plays into — which is
/// not always the default sink, and using the default would quietly watch the
/// wrong device.
fn monitor_of(main: &mut Mainloop, ctx: &Context, sink: u32) -> Option<String> {
    let found = std::rc::Rc::new(std::cell::RefCell::new(None));
    let out = found.clone();
    let op = ctx
        .introspect()
        .get_sink_info_by_index(sink, move |result| {
            if let ListResult::Item(info) = result
                && let Some(name) = &info.monitor_source_name
            {
                *out.borrow_mut() = Some(name.to_string());
            }
        });
    while op.get_state() == libpulse_binding::operation::State::Running {
        pump(main)?;
    }
    found.borrow_mut().take()
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
