# Sidecar footprint experiments

Reduce Slipmat's battery and memory cost without changing MusicKit's ownership
of playback or the queue. Work on `perf/sidecar-footprint`, in separate commits.

## Order and acceptance

1. Tie the sleep inhibitor to active playback, including loading, seeking and
   buffering. Startup, pause, stop, navigation and renderer failure must release
   it. Repeated state reports must not accumulate inhibitors.
2. Let an inactive player release Chromium while keeping its queue and position.
   Play from GTK or MPRIS must wake it and restore the selected occurrence before
   executing the command. Never suspend active or buffering playback, sign-in,
   or an outstanding command. Preserve the native library and error reporting.
3. Compare smaller hidden viewports and unnecessary visual resource loading
   separately. Keep only repeatable savings. Sign-in restores the normal window.

## Verification

- Use existing Rust tests and Node's built-in test runner. Test state transitions
  through the real entry points; no new dependencies.
- `make check`, `node --test sidecar/*.test.js`, `node --check sidecar/main.js`.
- Run the repo sidecar explicitly with `SLIPMAT_SIDECAR=$PWD/sidecar`.
- Measure unique-process PSS and CPU with `scripts/footprint.sh`, under the same
  queue and cache conditions. Record repeated samples, not one winning sample.
- Check real playback, pause/resume, seeking, media keys and a natural queue
  transition. Keep the PipeWire stream stable at a gapless boundary. Human
  listening remains necessary to judge audible continuity.
- No tokens in logs or captured artifacts. Do not change DRM, audio decoding,
  queue ownership, or dependencies. Follow CLAUDE.md's code conventions.

## Implementation locations

`sidecar/main.js` owns the window and sleep inhibitor; `sidecar/preload.js`
reports MusicKit state. `crates/slipmatd/src/serve.rs` owns sidecar lifetime and
restoration; GTK remains a client. Tests live beside the existing tests for each
module. This document records results, including rejected experiments.

## Results

- Baseline investigation: idle development app 557 MB PSS, of which about
  435 MB was Chromium; 0.6% of one CPU core in a three-second sample.
- Corrected the footprint sampler to deduplicate overlapping process trees and
  include leftover instances without a D-Bus name. Verified on a live process
  tree and a process with no bus name; `make check` passed.
- Sleep inhibitor: regression reproduced at hidden startup, then passed across
  playback, buffering, pause, stop, navigation and renderer loss. Eight Node
  tests and `make check` passed. Real playback, MPRIS pause/resume and seeking
  worked; PipeWire stream 17427 survived the two-track transition. No listening
  verdict yet. `systemd-inhibit` did not expose Electron's inhibitor on this
  desktop, so the OS sleep effect is not verified here.
