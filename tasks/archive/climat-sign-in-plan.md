# Climat Apple Music sign-in implementation plan

## Status

Completed and approved on 2026-09-02. Archived after manual verification.

## Source of truth

- Specification:
  [`docs/specs/SPEC-climat-sign-in.md`](../../docs/specs/SPEC-climat-sign-in.md)
- GitHub issue: [#194](https://github.com/SoftARV/Slipmat/issues/194)
- Archived MPRIS plan:
  [`tasks/archive/mpris-tracklist-plan.md`](mpris-tracklist-plan.md)

## Overview

Climat lets a signed-out user open Apple's sign-in window by pressing `Enter`.
The client sends the existing `Request::SignIn` request and waits for daemon
stage events. Runtime verification found that the daemon ignored the
sidecar's authorization event, so Task 2 also fixes that existing event path.

## Architecture decisions

1. Handle signed-out `Enter` after the global `Ctrl+C` check and before typing
   consumes the key. This keeps sign-in reachable if a field retained focus.
2. Send `Request::SignIn` through `link::Link`. The daemon maps it to
   `Command::ShowLogin`; the IPC and sidecar protocols need no change.
3. Keep the prompt in `ui::stage_text`, where every non-ready stage already
   gets its status text.
4. Test `on_key` through an in-memory `link::Link`. Compile the channel
   constructor under `cfg(test)` instead of introducing a frontend command
   abstraction.
5. Wait for `Event::Stage` before changing the displayed state. Climat does not
   predict authorization success.
6. Map the sidecar's existing authorization event to the daemon stage and hide
   the login window after success. No protocol change is needed.

## Dependency graph

```text
Task 1: signed-out key, prompt, and focused tests
    |
    v
Checkpoint A: automated behavior
    |
    v
Task 2: signed-out runtime verification and documentation
    |
    v
Checkpoint B: ready for implementation review
```

The tasks are sequential. Runtime verification needs the behavior from Task 1,
and the change is too small to gain from parallel work.

## Task list

### Phase 1: Behavior

- [x] Task 1: Make Climat's signed-out prompt actionable.

### Checkpoint A: Automated behavior

- [x] Focused tests prove the signed-out request and preserve normal `Enter`.
- [x] Climat builds and passes clippy without warnings.
- [x] Human review authorizes runtime verification.

### Phase 2: Runtime and documentation

- [x] Task 2: Verify sign-in from Climat and document the result.

### Checkpoint B: Complete

- [x] The signed-out flow works from Climat without launching Slipmat.
- [x] `make check` passes.
- [x] The specification and README describe the verified behavior.
- [x] Human review approves the feature for merge.

Detailed acceptance criteria and commands live in
[`tasks/archive/climat-sign-in-todo.md`](climat-sign-in-todo.md).

## Risks and controls

| Risk | Impact | Control |
|---|---|---|
| `Enter` starts sign-in during a ready session | Medium | Gate on `Stage::SignedOut` and test the ready path. |
| Typing focus swallows the sign-in key | Medium | Check signed-out `Enter` before the typing branch. |
| Test support leaks into production API | Low | Compile the in-memory link constructor under `cfg(test)`. |
| Runtime setup damages a working Apple session | High | Use an existing signed-out profile or a separate test profile; do not delete user data. |
| An installed sidecar shadows repository code | Medium | Run with `SLIPMAT_SIDECAR="$PWD/sidecar"`. |

## Definition of done

- Task acceptance criteria and focused tests pass.
- Runtime verification covers the Apple sign-in window and the resulting ready
  state.
- `make check` passes with no new warning.
- The diff contains no dependency, protocol, sidecar, or unrelated refactor.
- The daemon change stays limited to handling the existing authorization event.
- User-facing documentation matches the verified behavior.
- The human approves the result before merge.

## Planning evidence and limits

The code graph generation from 2026-09-01 was used at Verify tier to inspect
Climat key routing, stage rendering, the IPC request, and the daemon sign-in
handler. Coverage reported no recorded gaps for the Rust paths used here. The
graph excludes `docs/`, so the approved specification and Climat README were
read from source. A clean coverage result remains best-effort evidence.

## Open questions

None. The specification approves `Enter`, the existing IPC route, and the
graphical-session boundary.
