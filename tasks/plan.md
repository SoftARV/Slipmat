# Sign-out account-state cleanup implementation plan

## Status

Completed on 2026-09-02.

## Source of truth

- Specification:
  [`docs/specs/SPEC-sign-out-clear-state.md`](../docs/specs/SPEC-sign-out-clear-state.md)
- Branch: `fix/sign-out-clear-state`
- Task checklist: [`tasks/todo.md`](todo.md)

## Overview

Move sign-out cleanup into `slipmatd`, the process that owns shared account
state. The daemon will clear memory and persistence before publishing the
signed-out stage, invalidate work started by the old authorization session,
and publish existing empty-state events. Climat and GTK will clear their local
presentation only after the daemon reports sign-out.

## Architecture decisions

1. Use one idempotent daemon cleanup operation for
   `Authorization { authorized: false }` and `SignedOut`. The first removes
   access promptly; the second is the completion backstop.
2. Reset only account-derived model and command state. Preserve stream volume,
   application settings, artwork files, and global unplayable IDs.
3. Delete the library cache and saved playback session in the daemon. Remove
   shared-cache deletion from the GTK request path.
4. Add a monotonically increasing authorization generation to the daemon.
   Library refreshes capture it and discard results if it changes before the
   fetch completes. A stage-only check is insufficient because another account
   may already be ready when the old result arrives.
5. Reuse `Stage`, `Queue`, `Snapshot`, and `LibraryChanged` for cleanup. Add the
   approved `LibraryRefreshing` event so both clients can show the daemon's
   whole-library refresh state.
6. Let clients preserve preferences while clearing transient account content.
   Climat keeps view, sort, and catalog-kind choices; GTK keeps settings and
   chrome preferences.

## Dependency graph

```text
Task 1: daemon account reset and empty-state events
    |
    v
Task 2: reject stale library refreshes by authorization generation
    |
    v
Checkpoint A: daemon boundary proven
    |
    +--------------------+
    |                    |
    v                    v
Task 3: Climat reset     Task 4: GTK confirmed reset
    |                    |
    +----------+---------+
               |
               v
Checkpoint B: clients converge
               |
               v
Task 5: multi-client runtime verification and evidence
               |
               v
Checkpoint C: ready for implementation review
```

Tasks 3 and 4 are technically independent after Checkpoint A. They should
still land as separate commits so each client remains reviewable and
revertible. The current single-branch workflow can execute them sequentially.

## Task list

### Phase 1: Daemon account boundary

- [x] Task 1: Clear daemon account state and persistence on sign-out.
- [x] Task 2: Reject library refresh results from an ended authorization
  session.

### Checkpoint A: Daemon boundary

- [x] A populated daemon becomes empty before publishing `Stage::SignedOut`.
- [x] Cleanup is idempotent and preserves non-account state.
- [x] A late refresh cannot repopulate memory or disk.
- [x] Focused `slipmatd` tests and clippy pass.
- [x] Human review authorizes client work.

### Phase 2: Client convergence

- [x] Task 3: Clear Climat's transient account presentation on the daemon's
  signed-out stage.
- [x] Task 4: Move GTK cleanup from sign-out request time to daemon
  confirmation.

### Checkpoint B: Client convergence

- [x] Climat and GTK clear stale account content without reconnecting.
- [x] Both clients preserve non-account preferences.
- [x] Focused client tests and clippy pass.
- [x] Human review authorizes destructive runtime sign-out testing.

### Phase 3: Runtime and documentation

- [x] Task 5: Verify multi-client sign-out, restart, and fresh sign-in; record
  the result.

### Checkpoint C: Complete

- [x] All specification success criteria are met.
- [x] `make check` passes.
- [x] Runtime evidence covers two clients, daemon restart, and fresh sign-in.
- [x] The final diff contains no dependency, cache-format, or sidecar protocol
  change; its only IPC addition is the approved refresh-status event.
- [x] Human review approves the feature for merge.

Detailed task acceptance criteria and commands live in
[`tasks/todo.md`](todo.md).

## Risks and controls

| Risk | Impact | Control |
|---|---|---|
| Cleanup publishes the stage before state is empty | High | Reset memory and persistence first, then publish stage and empty-state events; assert event order. |
| An old refresh completes after sign-out or account replacement | High | Capture and compare an authorization generation before any save, model write, or event. |
| Repeated authorization and signed-out events erase preferences | Medium | Keep cleanup idempotent and test preserved volume and global state. |
| GTK clears optimistically while the daemon reports an error | Medium | Send only `Request::SignOut`; run presentation cleanup from daemon stage handling. |
| Climat leaves a stale row actionable behind the prompt | High | Clear browser, page, catalog, queue, and snapshot state in one signed-out transition test. |
| A refresh looks like an empty or frozen library | Medium | Publish daemon-owned refresh status and render it in both clients without hiding existing content. |
| Cache tests write into the developer profile | High | Reuse isolated XDG test paths; never clear the live profile from automated tests. |
| Runtime verification signs out a working account unexpectedly | High | Require Checkpoint B approval and explicit human participation before the manual sign-out. |

## Verification strategy

Each task starts with a focused failing test. Checkpoint A proves the shared
daemon contract before client changes. Checkpoint B proves both projections.
Task 5 uses one daemon with GTK and Climat connected, then verifies raw browse,
queue, and snapshot responses after sign-out.

Required commands:

```bash
cargo test -p slipmatd sign_out -- --nocapture
cargo clippy -p slipmatd --all-targets -- -D warnings
cargo test -p climat signed_out -- --nocapture
cargo clippy -p climat --all-targets -- -D warnings
cargo test -p slipmat signed_out -- --nocapture
cargo clippy -p slipmat --all-targets -- -D warnings
make check
```

## Definition of done

- Every task meets its acceptance criteria and focused tests fail before the
  implementation, then pass afterward.
- The daemon owns shared cleanup and clients only project confirmed state.
- No old account data survives in daemon memory, library cache, playback
  session, client presentation, or a late refresh.
- Runtime verification proves sign-out, signed-out restart, and clean sign-in.
- Documentation records the observed result.
- The human approves the result before merge.

## Planning evidence and limits

The code graph generation on `main` at `fa61415` was used at Verify tier.
`set_authorization` currently changes only the stage, `Model::new` loads the
library cache, and `refresh_library` writes every successful result without a
session check. Climat assigns `Stage` without clearing browser or queue state.
GTK calls `forget_session` immediately after sending `Request::SignOut`, and
that client method currently deletes shared cache files. Coverage reported no
recorded gaps for the inspected Rust and sidecar paths. That signal is
best-effort, and the approved spec was read directly.

## Open questions

None. The approved specification fixes the cleanup boundary and the retained
non-account state.
