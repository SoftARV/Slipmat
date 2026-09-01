# MPRIS TrackList implementation plan

## Status

Approved. The occurrence identity amendment was approved after live MusicKit
probing on 2026-08-31.

## Source of truth

- Feature specification: [`docs/specs/SPEC-mpris-tracklist.md`](../docs/specs/SPEC-mpris-tracklist.md)
- GitHub issue: [#191](https://github.com/SoftARV/Slipmat/issues/191)
- MPRIS TrackList contract: [freedesktop.org TrackList interface](https://specifications.freedesktop.org/mpris/latest/Track_List_Interface.html)
- Rust API: `mpris-server` 0.10.0, as locked by the workspace

The feature remains bounded by the specification: read-only TrackList support, a maximum 21-occurrence context window, no new dependencies, no GTK, Climat, or daemon IPC changes, and one approved sidecar field named `occurrenceId`.

## Assumptions

1. The MPRIS recommendation selected during specification review is the required behavior for the first version.
2. MusicKit remains the queue owner. TrackList is a projection and `GoTo` reuses `Command::ChangeToIndex`.
3. Occurrence IDs are process-local opaque D-Bus object paths. They do not encode a queue index, URL, or catalog identifier.
4. The sidecar assigns a process-local identifier to each MusicKit queue object. Live probing confirmed that duplicate objects are distinct, retained objects preserve identity through insertion, removal, and moves, and `nowPlayingItem` is the exact queue object once MusicKit completes a transition.
5. The existing root and Player interfaces must retain behavior while moving from the ready-made `mpris_server::Player` to `LocalServer`.

If implementation evidence contradicts an assumption, update and reapprove the specification before widening scope.

## Architecture

`slipmat-core::mpris` remains the D-Bus boundary. A private `mpris::track_list` submodule will own the pure queue projection, the mapping from sidecar occurrence IDs to opaque MPRIS paths, context-window selection, metadata comparison, and TrackList change classification. Keeping this logic free of D-Bus I/O makes duplicate and window behavior deterministic to test.

The daemon will continue to publish one `MprisState` snapshot from its existing player model. That snapshot will include the full queue facts and current queue position needed by the projection. The core MPRIS layer will retain the full occurrence registry internally but publish only the bounded context window.

The existing ready-made player will be replaced with `mpris_server::LocalServer` because TrackList requires one implementation of the root, Player, and TrackList local interfaces. Property and signal emission will remain centralized in `Mpris::update` so one old/new-state comparison controls both Player and TrackList notifications.

```text
Task 1: occurrence projection ----\
                                  +--> Task 3: read-only TrackList
Task 2: LocalServer parity -------/              |
                                                   v
                                      Task 4: GoTo routing
                                                   |
                                                   v
                                      Task 5: notifications
                                                   |
                                                   v
                                      Task 6: runtime gate
```

Tasks 1 and 2 are conceptually independent but will be implemented in order because both touch `mpris.rs`. Later tasks build on the same interface implementation and are also sequential.

## Task breakdown

### Task 1 — Build the occurrence projection

Add a private pure model for sidecar occurrence reconciliation, opaque MPRIS ID allocation, the 21-item window, current-occurrence lookup, metadata lookup, and structural-versus-metadata change classification.

Likely files: `crates/slipmat-core/src/mpris.rs`, `crates/slipmat-core/src/mpris/track_list.rs`.

Acceptance and verification are detailed in [`tasks/todo.md`](todo.md). This task is complete only when duplicate, edit, move, edge-window, empty-queue, stale-ID, and 500-item cases pass focused unit tests.

### Task 2 — Migrate existing MPRIS behavior to LocalServer

Implement the local root and Player interfaces and replace the ready-made `Player` without enabling TrackList yet. Preserve all current commands, properties, capability flags, metadata, seek behavior, and signals. Keep routine position ticks silent on D-Bus.

Likely file: `crates/slipmat-core/src/mpris.rs`.

This is an explicit regression-control step: Player parity is reviewed and checked before TrackList is added.

### Checkpoint A — Pure projection and Player parity

Run focused core tests and `cargo check -p slipmat-core`. Review the diff for Player behavior changes, long-lived `RefCell` borrows across awaits, accidental new dependencies, and forbidden panic helpers. Do not continue until parity is credible.

### Task 3 — Expose the read-only TrackList surface

Feed queue facts from the daemon into `MprisState`, construct the local server with TrackList support, and implement `Tracks`, `GetTracksMetadata`, `CanEditTracks`, `AddTrack`, and `RemoveTrack`. Use the same projection for TrackList metadata and Player `mpris:trackid`.

Likely files: `crates/slipmat-core/src/mpris.rs`, `crates/slipmat-core/src/mpris/track_list.rs`, `crates/slipmatd/src/bus.rs`.

### Task 4 — Route GoTo through the existing queue command

Resolve a TrackList occurrence ID to its full MusicKit queue index, add the corresponding MPRIS command, and map it in the daemon to `Command::ChangeToIndex`. Unknown or stale IDs must be harmless; this path must never send `SetQueue`. When MusicKit moves between identical songs and retains the old playback time, seek to zero after the selected `nowPlayingItem` arrives.

Likely files: `crates/slipmat-core/src/mpris.rs`, `crates/slipmat-core/src/mpris/track_list.rs`, `crates/slipmatd/src/bus.rs`.

### Task 5 — Emit precise TrackList notifications

Emit `TrackListReplaced` and invalidate `Tracks` when the published sequence or window changes. Emit `TrackMetadataChanged` only for retained occurrences whose exposed metadata changed. Position ticks and unrelated player changes must not produce TrackList traffic.

Likely files: `crates/slipmat-core/src/mpris.rs`, `crates/slipmat-core/src/mpris/track_list.rs`.

### Checkpoint B — Complete automated behavior

Run all focused MPRIS and daemon tests plus crate checks. Inspect notification tests for both required signals and prohibited extra traffic. Confirm each MPRIS ID shown in Player metadata and TrackList metadata comes from one projection.

### Task 6 — Runtime and repository verification

Build and run the daemon, inspect D-Bus interfaces and properties, exercise metadata lookup and `GoTo`, observe signals during queue and metadata changes, and verify gapless playback remains intact. Finish with the full repository quality gate and update the feature documents with the verified result.

Likely files: `docs/specs/SPEC-mpris-tracklist.md`, `tasks/todo.md`. Production code changes are only allowed here when a runtime failure is first captured by a regression test.

### Checkpoint C — Human approval for merge readiness

Review runtime evidence, the full `make check` result, the final diff, and documentation. The feature is not done until the human approves it for merge.

## Risk controls

| Risk | Impact | Control |
|---|---|---|
| LocalServer migration regresses existing MPRIS behavior | High | Land and verify Player parity before exposing TrackList. |
| Duplicate or moved entries receive the wrong occurrence ID | High | Key reconciliation by the sidecar occurrence ID and test duplicate edits, moves, and current-item transitions explicitly. |
| MusicKit carries playback time into an identical duplicate | Medium | After duplicate `GoTo`, seek to zero when the selected `nowPlayingItem` arrives. |
| Player metadata and TrackList disagree about the current ID | High | Generate both from the same projection snapshot. |
| Position polling floods D-Bus with property changes | Medium | Keep position updates in shared state and test that ticks produce no TrackList or Player property traffic. |
| Queue changes emit incomplete or excessive signals | High | Classify projection changes before I/O and assert the exact notification plan in unit tests. |
| Async interface methods hold mutable borrows across awaits | Medium | Copy required state before awaiting and keep borrow scopes short. |
| `GoTo` disrupts gapless playback | High | Reuse `ChangeToIndex`, prohibit `SetQueue`, and repeat the existing gapless runtime check. |

## Definition of done

Each task must meet its acceptance criteria and focused verification before it is checked off. The feature also requires:

- runtime behavior verified through the session bus;
- new behavior covered by tests that fail without it;
- existing tests, formatting, linting, and `make check` passing;
- no unrelated refactors, new dependencies, debug output, or dead code;
- public behavior and architectural decisions reflected in current documentation;
- backward compatibility of the existing MPRIS Player interface reviewed;
- a practical rollback path: revert the feature commits to restore the ready-made Player implementation;
- human review before merge.

## Planning evidence and limits

The codebase graph generation from 2026-08-31 was used at Verify tier to trace `Daemon::publish_snapshot` into `bus::state` and `Mpris::update`, and to inspect the MPRIS, player protocol/state, daemon bus, and daemon publish boundaries. Coverage reported no recorded gaps for the Rust source and manifest paths used here. `docs/` and `Cargo.lock` are excluded from the graph by design, so the specification and exact locked crate version were read directly. A clean graph coverage result is best-effort evidence, not proof of completeness.

## Open questions

None for planning. Any newly discovered conflict between `mpris-server` 0.10.0 and the approved contract blocks the affected task until the specification and plan are revised.
