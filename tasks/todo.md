# MPRIS TrackList task checklist

Plan: [`tasks/plan.md`](plan.md)
Specification: [`docs/specs/SPEC-mpris-tracklist.md`](../docs/specs/SPEC-mpris-tracklist.md)

The plan and the sidecar occurrence identity amendment are approved.

## Task 1 — Build the occurrence projection

- [x] Status: complete
- Description: Add a private, pure TrackList model that reconciles full queue snapshots into stable occurrence records and publishes a bounded context window.
- Scope: Small-to-medium. Model and unit tests only; no D-Bus interface changes.
- Dependencies: None.
- Files likely touched:
  - `crates/slipmat-core/src/mpris.rs`
  - `crates/slipmat-core/src/mpris/track_list.rs` (new)
- Acceptance criteria:
  - [x] Every live queue occurrence has a valid opaque MPRIS object path.
  - [x] Simultaneous duplicate items have distinct IDs.
  - [x] Retained occurrences preserve IDs across insertion, removal, a single move, metadata refresh, and context-window slides.
  - [x] Sidecar occurrence IDs distinguish identical duplicates without positional matching.
  - [x] The current `nowPlayingItem` occurrence ID resolves its exact queue entry even while MusicKit's queue position is pre-advanced.
  - [x] The published window contains at most 21 entries, prefers 10 before and 10 after, and shifts at queue edges.
  - [x] Empty queues and missing or out-of-range current positions produce an empty or safe projection without panicking.
  - [x] Metadata lookup and full-queue-index lookup reject unknown or stale IDs.
  - [x] A 500-entry queue reconciles correctly while exposing no more than 21 entries.
  - [x] Structural, window, and metadata-only changes are classified separately for later signal emission.
- Verification:
  - [x] Write focused tests before each behavior change.
  - [x] Run `cargo test -p slipmat-core mpris::track_list`.
  - [x] Run `cargo check -p slipmat-core`.
  - [x] Review for index-derived IDs, duplicate collisions, `.unwrap()`/`.expect()`, and unnecessary public API.

## Task 2 — Migrate existing MPRIS behavior to LocalServer

- [x] Status: complete
- Description: Replace the ready-made `mpris_server::Player` with a local root and Player implementation while preserving current behavior. TrackList remains disabled in this task.
- Scope: Medium. One core boundary plus focused parity tests.
- Dependencies: None. Schedule after Task 1 because both tasks touch `mpris.rs`.
- Files likely touched:
  - `crates/slipmat-core/src/mpris.rs`
- Acceptance criteria:
  - [x] Root identity, desktop entry, URI and MIME support, raise, and quit behavior match the current implementation.
  - [x] Play, pause, play/pause, next, previous, seek, shuffle, repeat, and volume still map to the existing `MprisCommand` variants.
  - [x] Playback status, loop status, rate bounds, shuffle, metadata, volume, position, and capability properties retain their current values.
  - [x] `Seeked` and Player property changes are emitted with the same semantics as before.
  - [x] Routine position updates change the returned property without emitting property-change traffic on every tick.
  - [x] `SetPosition` validates the supplied current track ID and ignores stale IDs.
  - [x] D-Bus startup or emission failures are logged and do not panic the process.
  - [x] `HasTrackList` remains false until Task 3.
- Verification:
  - [x] Adapt or add parity tests before replacing the ready-made player.
  - [x] Run `cargo test -p slipmat-core mpris`.
  - [x] Run `cargo check -p slipmat-core`.
  - [x] Review async methods for `RefCell` borrows held across awaits.

## Checkpoint A — Projection and Player parity

- [x] Tasks 1 and 2 meet their acceptance criteria.
- [x] Focused tests and `cargo check -p slipmat-core` pass.
- [ ] The diff introduces no dependency, IPC, sidecar, GTK, or Climat change.
- [x] Existing MPRIS behavior has a clear parity test or explicit runtime-verification item.
- [ ] Human review authorizes TrackList exposure.

## Task 3 — Expose the read-only TrackList surface

- [ ] Status: pending
- Description: Supply queue facts from the daemon and implement the local TrackList properties and read-only methods.
- Scope: Medium. Core interface wiring and daemon snapshot mapping.
- Dependencies: Tasks 1 and 2; Checkpoint A.
- Files likely touched:
  - `crates/slipmat-core/src/mpris.rs`
  - `crates/slipmat-core/src/mpris/track_list.rs`
  - `crates/slipmatd/src/bus.rs`
- Acceptance criteria:
  - [ ] `HasTrackList` is true and the TrackList interface is present on the existing MPRIS object.
  - [ ] `Tracks` returns the projection's ordered context window.
  - [ ] `GetTracksMetadata` preserves request order and returns metadata only for known IDs.
  - [ ] Each returned metadata map includes `mpris:trackid` and the available title, artist, album, length, and track-number fields required by the specification.
  - [ ] `mpris:artUrl` is included only when the current artwork is already cached locally; no TrackList call triggers an artwork or network fetch.
  - [ ] Player metadata uses the current occurrence ID from the same projection.
  - [ ] `CanEditTracks` is false; `AddTrack` and `RemoveTrack` return `NotSupported` without changing the queue.
  - [ ] No GTK, Climat, daemon IPC, or sidecar protocol changes beyond the approved `occurrenceId` field are introduced.
- Verification:
  - [ ] Add tests for properties, metadata ordering, unknown IDs, partial metadata, local artwork, and unsupported edits.
  - [ ] Run `cargo test -p slipmat-core mpris`.
  - [ ] Run `cargo test -p slipmatd`.
  - [ ] Run `cargo check -p slipmat-core -p slipmatd`.

## Task 4 — Route GoTo through ChangeToIndex

- [ ] Status: pending
- Description: Resolve an exposed occurrence ID to its full queue index and pass that index through the existing daemon command path.
- Scope: Small. One new internal command variant and daemon mapping.
- Dependencies: Task 3.
- Files likely touched:
  - `crates/slipmat-core/src/mpris.rs`
  - `crates/slipmat-core/src/mpris/track_list.rs`
  - `crates/slipmatd/src/bus.rs`
- Acceptance criteria:
  - [ ] `GoTo` on a known exposed occurrence emits its full queue index.
  - [ ] The daemon maps that command to `Command::ChangeToIndex { index }`.
  - [ ] Duplicate occurrences route to their individual queue positions.
  - [ ] Navigating between identical duplicates starts the selected occurrence at zero rather than retaining the previous occurrence's playback time.
  - [ ] Unknown, stale, and no-longer-exposed IDs are harmless and do not change playback.
  - [ ] The MPRIS path never sends `Command::SetQueue`.
- Verification:
  - [ ] Add failing-first tests for valid, duplicate, duplicate-position reset, stale, and unknown IDs.
  - [ ] Run `cargo test -p slipmat-core mpris`.
  - [ ] Run `cargo test -p slipmatd`.
  - [ ] Search the changed MPRIS path for `SetQueue` and confirm no call was added.

## Task 5 — Emit precise TrackList notifications

- [ ] Status: pending
- Description: Translate projection changes into the minimum required TrackList signals and property invalidations.
- Scope: Small-to-medium. Change planning and D-Bus emission tests.
- Dependencies: Tasks 3 and 4.
- Files likely touched:
  - `crates/slipmat-core/src/mpris.rs`
  - `crates/slipmat-core/src/mpris/track_list.rs`
- Acceptance criteria:
  - [ ] A published sequence, ordering, or context-window change emits `TrackListReplaced` with the new list and current occurrence ID.
  - [ ] The same structural update invalidates `Tracks` without embedding a replacement value.
  - [ ] A metadata-only change for a retained exposed occurrence emits `TrackMetadataChanged` for that occurrence.
  - [ ] Position ticks and unrelated Player changes emit no TrackList notification.
  - [ ] Structural updates do not also emit redundant granular add/remove signals in version 1.
  - [ ] Bus errors are logged without panicking or stopping subsequent updates.
- Verification:
  - [ ] Unit-test the exact notification plan for insert, remove, move, window slide, metadata refresh, position tick, and unrelated changes.
  - [ ] Run `cargo test -p slipmat-core mpris`.
  - [ ] Run `cargo check -p slipmat-core -p slipmatd`.

## Checkpoint B — Automated feature behavior

- [ ] Tasks 3–5 meet their acceptance criteria.
- [ ] All focused `slipmat-core` and `slipmatd` tests pass.
- [ ] Crate checks pass without new warnings.
- [ ] Player and TrackList metadata share one current occurrence ID.
- [ ] Notification tests cover both required events and prohibited extra traffic.
- [ ] Human review authorizes runtime verification.

## Task 6 — Runtime and repository verification

- [ ] Status: pending
- Description: Verify the built feature on the session bus, run the project quality gate, and record the resulting evidence.
- Scope: Medium verification task. Documentation updates only unless a failure is captured by a regression test first.
- Dependencies: Tasks 1–5; Checkpoint B.
- Files likely touched:
  - `docs/specs/SPEC-mpris-tracklist.md`
  - `tasks/todo.md`
- Acceptance criteria:
  - [ ] The daemon starts and owns its expected MPRIS bus name.
  - [ ] Introspection shows root, Player, and TrackList on the existing object path.
  - [ ] Runtime properties report `HasTrackList=true`, `CanEditTracks=false`, and no more than 21 ordered track IDs.
  - [ ] Runtime metadata lookup returns matching IDs and available fields for the context window.
  - [ ] Runtime `GoTo` selects the requested occurrence, including a duplicate-item case when practical.
  - [ ] Add and remove calls return `NotSupported` and leave the queue unchanged.
  - [ ] Observed structural and metadata changes emit the required signals without tick noise.
  - [ ] Gapless playback verification still passes after MPRIS navigation.
  - [ ] The full repository quality gate passes.
  - [ ] Documentation records final verified behavior and any justified deviations.
- Verification:
  - [ ] Run `cargo build -p slipmatd` and start the daemon in a session-bus environment.
  - [ ] Use `busctl --user introspect`, `get-property`, and `call` for the TrackList contract.
  - [ ] Use `busctl --user monitor` or equivalent to inspect TrackList signals and property invalidations.
  - [ ] Exercise current, first, last, short, long, and duplicate queue scenarios where available.
  - [ ] Repeat the project's gapless playback verification.
  - [ ] Run `make check`.
  - [ ] Review the final diff for unrelated changes, panic helpers, debug output, and documentation drift.

## Checkpoint C — Merge-readiness approval

- [ ] Runtime evidence is recorded and all Task 6 criteria pass.
- [ ] `make check` passes on the final tree.
- [ ] The specification reflects the implemented contract.
- [ ] The draft pull request summarizes the implementation and verification accurately.
- [ ] The human has reviewed and approved the feature before merge.
