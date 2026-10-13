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

### Hidden-page comparisons

Standalone sidecar, same two catalog tracks (`1507991241`, `1376096987`),
three samples three seconds apart after ten seconds of playback. Each run
started a fresh process against the existing warm Apple session. CPU values
are percentages of one core. The values below are medians within each run.

| Variant | Run medians, PSS MB | Run medians, CPU % | Verdict |
| --- | --- | --- | --- |
| Baseline | 573.4, 554.8, 567.7 | 6.80, 3.31, 6.48 | Reference |
| 64 x 64 hidden viewport | 565.0, 562.1 | 3.89, 4.30 | Within baseline variation; not kept |
| Images disabled | 506.5, 577.9, 577.1 | 4.54, 3.56, 4.30 | Inconsistent; not kept |
| Initial hidden painting disabled | 560.3, 561.1 | 3.89, 3.31 | Within baseline variation; not kept |
| Background throttling enabled (initial trial) | 558.2 | 3.56 | No clear saving; not kept |

Every completed run advanced playback at least 17 seconds. These short runs
measure startup and early playback, not long-session behavior or sign-in.
No visual experiment has been applied to the production sidecar.

### Idle-release review

The safe prerequisite change keeps a buffering sidecar alive even with no
clients. It still permits idle cleanup of paused, signed-out and broken
players. A broader `stage != Ready` guard was rejected in review because it
would retain unused signed-out and broken renderers indefinitely.

Connected-client hibernation is deferred until wake-up has a tested contract:

- MPRIS commands currently do not wake an absent sidecar; sends are dropped.
- Gate complete external requests before they mutate the mirror. Replaying
  low-level commands is too late for seek, move and clear operations.
- Wait for restoration completion, not merely hook readiness. An ordered write
  to stdin does not serialize the asynchronous MusicKit handlers.
- A dormant seek must survive until a current item exists. Rebuilt queues also
  replace occurrence IDs, so GoTo must resolve its target after restoration.
- Restore the retained live state rather than relying on a successful disk
  write; preserve shuffle and repeat as well as queue position.

The existing five-minute timeout and connected-client policy remain unchanged.
The buffering guard's regression test failed before the fix and passes now;
`make check` and `cargo build -p slipmatd` pass with the final predicate.
