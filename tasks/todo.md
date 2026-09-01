# MPRIS TrackList review remediation checklist

Plan: [`tasks/plan.md`](plan.md)
Archived feature checklist:
[`tasks/archive/mpris-tracklist-todo.md`](archive/mpris-tracklist-todo.md)

Status: awaiting human approval. Do not implement these tasks before approval.

## Task 1: Install the complete sidecar artifact

**Description:** Add `queue-identity.js` to every supported installation path
and add an artifact check that catches missing local preload modules.

**Acceptance criteria:**
- [ ] Makefile, Flatpak, stable AUR, and AUR-git installs contain every local module required by `preload.js`.
- [ ] An assembled-sidecar check fails when `queue-identity.js` or another local preload dependency is absent.
- [ ] The change does not copy tests or development-only files into release artifacts.

**Verification:**
- [ ] Run the assembled-sidecar check against each recipe's installed file list.
- [ ] Run `python3 packaging/flatpak/check-sources.py`.
- [ ] Run `make check`.

**Dependencies:** None.

**Files likely touched:**
- `Makefile`
- `packaging/flatpak/dev.miguelrincon.Slipmat.yml`
- `packaging/aur/slipmat/PKGBUILD`
- `packaging/aur/slipmat-git/PKGBUILD`

**Estimated scope:** Medium, 4 files.

## Task 2: Run sidecar contract tests in the quality gate

**Description:** Make the dependency-free queue identity tests part of the
repository command used locally and in CI.

**Acceptance criteria:**
- [ ] `make check` runs `node --test sidecar/queue-identity.test.js`.
- [ ] A broken occurrence allocator fails the same CI job as Rust tests.
- [ ] The check requires no Electron download or `npm install`.

**Verification:**
- [ ] Run `node --test sidecar/queue-identity.test.js`.
- [ ] Run `make check`.
- [ ] Confirm the PR workflow invokes `make check`.

**Dependencies:** None.

**Files likely touched:**
- `Makefile`
- `sidecar/queue-identity.test.js`

**Estimated scope:** Small, 1-2 files.

## Checkpoint A: Shippable sidecar

- [ ] Tasks 1 and 2 meet their acceptance criteria.
- [ ] Packaging checks and sidecar tests fail for the defects they protect against.
- [ ] `make check` passes.
- [ ] Human review authorizes MPRIS behavior fixes.

## Task 3: Gate TrackList on valid occurrence identities

**Description:** Validate non-empty, unique occurrence IDs and an exact current
match before exposing TrackList state. Invalid or legacy snapshots retain Player
behavior but expose no approximate TrackList.

**Acceptance criteria:**
- [ ] Missing, duplicate, or unmatched occurrence IDs cannot produce unstable TrackList paths or an incorrect current occurrence.
- [ ] Invalid identity snapshots do not emit repeated replacement traffic on position ticks.
- [ ] Valid queues, including duplicate songs with distinct occurrence IDs, retain current behavior.

**Verification:**
- [ ] Add failing-first tests for missing, duplicate, unmatched, empty, and valid identity snapshots.
- [ ] Run `cargo test -p slipmat-core mpris` and `cargo test -p slipmatd`.
- [ ] Run a mixed-version session-bus check with an old sidecar fixture or captured event stream.

**Dependencies:** Checkpoint A and approval of the capability-gate policy.

**Files likely touched:**
- `crates/slipmat-core/src/mpris.rs`
- `crates/slipmat-core/src/mpris/track_list.rs`
- `crates/slipmat-core/src/player/protocol.rs`

**Estimated scope:** Medium, 3 files.

## Task 4: Make GoTo restart ordered and observable

**Description:** Use a seek-only path for the current occurrence, retain
event-gated zero seek for a different occurrence, and emit the MPRIS `Seeked`
signal for discontinuous position changes.

**Acceptance criteria:**
- [ ] Current-occurrence `GoTo` sends no redundant `ChangeToIndex` and restarts at zero.
- [ ] Different-occurrence `GoTo` seeks only after the exact selected item arrives.
- [ ] Each confirmed restart emits exactly one matching `Seeked` signal.

**Verification:**
- [ ] Add failing-first ordering tests for current, different, duplicate, cancelled, and stale targets.
- [ ] Run focused core and daemon MPRIS tests.
- [ ] Monitor commands and `Seeked` on the session bus during both `GoTo` paths.

**Dependencies:** Task 3.

**Files likely touched:**
- `crates/slipmat-core/src/mpris.rs`
- `crates/slipmatd/src/bus.rs`
- `crates/slipmatd/src/serve.rs`

**Estimated scope:** Medium, 3 files.

## Task 5: Separate position updates from queue reconciliation

**Description:** Introduce distinct hot Player updates and cold queue updates so
the 500 ms position path performs no full-queue clone, identity reconciliation,
or TrackList allocation.

**Acceptance criteria:**
- [ ] Position-only snapshots update the polled MPRIS position in work independent of queue length.
- [ ] Queue, current-item, metadata, and artwork events still update the correct Player and TrackList state.
- [ ] Reconciliation uses an O(n) occurrence lookup rather than repeated vector removal.

**Verification:**
- [ ] Add an instrumented 500-item test proving a position tick performs zero reconciliations.
- [ ] Add unchanged and edited 500-item reconciliation tests that catch quadratic matching.
- [ ] Run focused tests and compare runtime tick cost before and after.

**Dependencies:** Tasks 3 and 4.

**Files likely touched:**
- `crates/slipmat-core/src/mpris.rs`
- `crates/slipmat-core/src/mpris/track_list.rs`
- `crates/slipmatd/src/bus.rs`
- `crates/slipmatd/src/serve.rs`

**Estimated scope:** Medium, 4 files.

## Task 6: Coalesce MPRIS update delivery

**Description:** Replace the unbounded batch queue with a single-flight emitter
that keeps at most the newest pending state and does not enqueue empty updates.

**Acceptance criteria:**
- [ ] Empty position updates allocate no pending D-Bus batch.
- [ ] Slow emission leaves at most one newer pending state.
- [ ] Replacement payloads, TrackList properties, and metadata describe the same committed state.

**Verification:**
- [ ] Add deterministic stalled-emitter and rapid-update tests.
- [ ] Assert bounded pending state and final signal/property agreement.
- [ ] Run `cargo test -p slipmat-core mpris` and `cargo check -p slipmat-core -p slipmatd`.

**Dependencies:** Task 5.

**Files likely touched:**
- `crates/slipmat-core/src/mpris.rs`
- `crates/slipmat-core/src/mpris/track_list.rs`

**Estimated scope:** Small-to-medium, 2 files.

## Checkpoint B: Correct and bounded runtime

- [ ] Tasks 3-6 meet their acceptance criteria.
- [ ] Focused tests cover identity failures, `GoTo`, `Seeked`, hot ticks, and stalled emission.
- [ ] A 500-item position tick performs no queue reconciliation.
- [ ] `make check` passes without TrackList tick traffic.
- [ ] Human review authorizes structural cleanup.

## Task 7: Split the MPRIS module and remove dead change state

**Description:** Separate public MPRIS types, interface implementations, update
delivery, and tests. Remove `Change::queue` if no production behavior needs it.

**Acceptance criteria:**
- [ ] The facade, LocalServer adapter, update pump, projection, and tests live in focused modules.
- [ ] The MPRIS facade stays below 500 lines, and each production submodule stays below 800 lines with one clear responsibility.
- [ ] Removing `Change::queue` changes no notification or public behavior.

**Verification:**
- [ ] Move code without changing behavior tests in the same commit.
- [ ] Run focused MPRIS tests and `make check`.
- [ ] Review graph callers and dead-code warnings after the split.

**Dependencies:** Checkpoint B.

**Files likely touched:**
- `crates/slipmat-core/src/mpris.rs`
- `crates/slipmat-core/src/mpris/interface.rs` (new)
- `crates/slipmat-core/src/mpris/updates.rs` (new)
- `crates/slipmat-core/src/mpris/tests.rs` (new)
- `crates/slipmat-core/src/mpris/track_list.rs`

**Estimated scope:** Medium, 5 files.

## Task 8: Remove the raw queue-identity diagnostic

**Description:** Remove production probing that reads and logs broad MusicKit
identity-like properties. Retain only the occurrence allocator required by the
feature.

**Acceptance criteria:**
- [ ] Production code no longer logs raw queue, account, device, or playback identity values.
- [ ] `createOccurrenceId` retains stable per-object and per-context behavior.
- [ ] No unused probe exports, handlers, environment flags, or tests remain.

**Verification:**
- [ ] Run sidecar tests.
- [ ] Search production sidecar code for `queue-identity-probe` and `SLIPMAT_QUEUE_IDENTITY_PROBE`.
- [ ] Run `make check`.

**Dependencies:** Checkpoint B and approval to remove the probe. Safe to run in parallel with Task 7.

**Files likely touched:**
- `sidecar/main.js`
- `sidecar/preload.js`
- `sidecar/queue-identity.js`
- `sidecar/queue-identity.test.js`

**Estimated scope:** Medium, 4 files.

## Task 9: Repeat packaged and session-bus verification

**Description:** Verify the remediated feature from an assembled sidecar and on
the live session bus, then update review evidence without changing production
code.

**Acceptance criteria:**
- [ ] Packaged-like startup loads every sidecar module and exports the expected MPRIS interfaces.
- [ ] Valid and legacy sidecars show bounded, stable behavior with no tick noise; `GoTo` and `Seeked` pass.
- [ ] Long-queue navigation preserves decoder continuity and all repository checks pass.

**Verification:**
- [ ] Run `make check` and the packaged-sidecar smoke check.
- [ ] Use `busctl` or equivalent to inspect properties, methods, signals, and legacy gating.
- [ ] Repeat the stream continuity check and record justified runtime limits.

**Dependencies:** Tasks 7 and 8.

**Files likely touched:**
- `docs/specs/SPEC-mpris-tracklist.md`
- `tasks/todo.md`

**Estimated scope:** Small, documentation and verification only.

## Checkpoint C: Merge decision

- [ ] Every required review finding has a regression test and verified remedy.
- [ ] Optional privacy and dead-code findings are resolved or explicitly retained with justification.
- [ ] PR #192 accurately describes final behavior and verification.
- [ ] Human review approves the remediation commits before they reach the feature branch.
