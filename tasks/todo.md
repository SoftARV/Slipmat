# Sign-out account-state cleanup task checklist

Plan: [`tasks/plan.md`](plan.md)
Specification:
[`docs/specs/SPEC-sign-out-clear-state.md`](../docs/specs/SPEC-sign-out-clear-state.md)
Branch: `fix/sign-out-clear-state`

Status: Approved on 2026-09-02. Implementation in progress.

## Task 1: Clear daemon account state and persistence

**Description:** Add one idempotent daemon operation that removes all
account-derived model, pending-command, library-cache, and playback-session
state before the signed-out stage is visible. Use it for authorization loss and
the sidecar's completed sign-out event, then publish existing empty-state
events to every client.

**Acceptance criteria:**

- [x] `Authorization { authorized: false }` clears tokens, library, player,
  artwork selection, queue verification, retry, resume, restart, and completed
  command state before `Stage::SignedOut` is published.
- [x] `SignedOut` runs the same cleanup harmlessly, the library and session
  files are absent, and volume plus global unplayable IDs remain unchanged.
- [x] Subscribers receive signed-out stage, empty queue, empty snapshot, and
  library-change events; new IPC requests return no old rows or playback data.

**Verification:**

- [x] Add focused tests that fail against the current stage-only behavior.
- [x] Run `cargo test -p slipmatd sign_out -- --nocapture`.
- [x] Run `cargo clippy -p slipmatd --all-targets -- -D warnings`.

**Dependencies:** None.

**Files likely touched:**

- `crates/slipmatd/src/serve.rs`
- `crates/slipmatd/src/state.rs`

**Estimated scope:** Medium, 2 files.

## Task 2: Reject stale library refresh results

**Description:** Tie each library refresh to the authorization generation that
started it. Discard a completed fetch if sign-out or another authorization
transition changed that generation before the result can touch persistence,
memory, or clients.

**Acceptance criteria:**

- [ ] Every authorization transition advances a monotonic generation captured
  by `refresh_library`.
- [ ] A refresh from an ended session cannot save cache data, replace library
  rows, or publish `LibraryChanged`, including after a new account is ready.
- [ ] A refresh from the current ready session still saves and publishes
  normally, and the single-refresh guard can recover after a discarded result.

**Verification:**

- [ ] Add a deterministic failing test for a late old-session result.
- [ ] Run `cargo test -p slipmatd refresh -- --nocapture`.
- [ ] Run `cargo clippy -p slipmatd --all-targets -- -D warnings`.

**Dependencies:** Task 1.

**Files likely touched:**

- `crates/slipmatd/src/serve.rs`

**Estimated scope:** Small, 1 file.

## Checkpoint A: Daemon boundary

- [ ] Tasks 1 and 2 meet their acceptance criteria.
- [ ] Focused tests prove cleanup order, idempotence, persistence removal, and
  stale-result rejection.
- [ ] `slipmatd` builds and passes clippy without warnings.
- [ ] Human review authorizes client work.

## Task 3: Clear Climat on confirmed sign-out

**Description:** Reset Climat's transient account projection when the daemon
reports `Stage::SignedOut`. Remove rows and actions from library, catalog,
pages, queue, and now-playing while retaining view, sort, and catalog-kind
preferences.

**Acceptance criteria:**

- [ ] The signed-out stage clears snapshot, queue, bars, message, browser rows,
  catalog results, open page, filters, searches, and pagination state before
  the next draw.
- [ ] No stale row can be opened, played, or queued while the sign-in prompt is
  shown.
- [ ] View, sort, and catalog-kind choices survive the reset, and other stage
  transitions keep their current behavior.

**Verification:**

- [ ] Add a focused `App::on_event` test populated with account state.
- [ ] Run `cargo test -p climat signed_out -- --nocapture`.
- [ ] Run `cargo clippy -p climat --all-targets -- -D warnings`.

**Dependencies:** Tasks 1 and 2, Checkpoint A.

**Files likely touched:**

- `crates/climat/src/main.rs`
- `crates/climat/src/browser.rs`

**Estimated scope:** Medium, 2 files.

## Task 4: Clear GTK on daemon confirmation

**Description:** Stop treating the GTK request as completed sign-out. Send the
request, then run the existing presentation cleanup only when the daemon
reports `Stage::SignedOut`; leave shared persistence to the daemon.

**Acceptance criteria:**

- [ ] `AppMsg::SignOutConfirmed` sends `Request::SignOut` without changing the
  stage or deleting client/shared state immediately.
- [ ] The daemon's signed-out stage clears GTK library, catalog, pages, queue
  projection, current-track presentation, and backdrop without deleting
  preferences or shared cache files from the client.
- [ ] A sign-out error leaves the current presentation intact and visible.

**Verification:**

- [ ] Add focused coverage for request-versus-confirmation ordering.
- [ ] Run `cargo test -p slipmat signed_out -- --nocapture`.
- [ ] Run `cargo clippy -p slipmat --all-targets -- -D warnings`.

**Dependencies:** Tasks 1 and 2, Checkpoint A.

**Files likely touched:**

- `crates/slipmat/src/app/mod.rs`
- `crates/slipmat/src/app/supervise.rs`

**Estimated scope:** Medium, 2 files.

## Checkpoint B: Client convergence

- [ ] Tasks 3 and 4 meet their acceptance criteria.
- [ ] Both clients clear account data from the same daemon transition.
- [ ] Focused client tests and clippy pass without warnings.
- [ ] Human review authorizes destructive runtime sign-out testing.

## Task 5: Verify the complete sign-out boundary

**Description:** Exercise GTK and Climat against one signed-in daemon, sign out
through GTK, verify both clients and raw IPC are empty, restart while signed
out, then sign in through Climat and record the observed result.

**Acceptance criteria:**

- [ ] Both connected clients clear without reconnecting, playback stops, and
  raw browse, queue, and snapshot responses contain no previous account data.
- [ ] A daemon restart while signed out restores nothing; a fresh sign-in loads
  the library without the previous queue.
- [ ] The specification records runtime evidence and any approved deviation;
  no credential or token is inspected or logged.

**Verification:**

- [ ] Complete the multi-client runtime sequence in the approved specification.
- [ ] Run `make check`.
- [ ] Review the final diff for unrelated changes, dependencies, protocol
  changes, debug output, weakened tests, and documentation drift.

**Dependencies:** Tasks 3 and 4, Checkpoint B.

**Files likely touched:**

- `docs/specs/SPEC-sign-out-clear-state.md`
- `tasks/plan.md`
- `tasks/todo.md`

**Estimated scope:** Small, 3 documentation files.

## Checkpoint C: Merge approval

- [ ] All five tasks meet their acceptance criteria.
- [ ] Focused tests and `make check` pass.
- [ ] Runtime evidence covers multi-client sign-out, signed-out restart, and
  clean sign-in.
- [ ] The implementation adds no dependency, cache-format, IPC, or sidecar
  protocol change.
- [ ] The human has reviewed and approved the feature before merge.
