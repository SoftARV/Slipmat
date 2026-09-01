# MPRIS TrackList review remediation plan

## Status

Awaiting human approval. This plan contains no implementation changes.

The branch `fix/mpris-tracklist-review` is stacked on
`feat/mpris-tracklist`. The original feature plan and checklist remain in
[`tasks/archive/`](archive/).

## Sources

- Feature specification: [`docs/specs/SPEC-mpris-tracklist.md`](../docs/specs/SPEC-mpris-tracklist.md)
- Feature pull request: [#192](https://github.com/SoftARV/Slipmat/pull/192)
- Review findings: packaging completeness, legacy identity handling, `GoTo`
  ordering, `Seeked` delivery, queue hot-path cost, update coalescing, sidecar
  test coverage, module size, diagnostic privacy, and dead code
- MPRIS contracts: [TrackList](https://specifications.freedesktop.org/mpris/latest/Track_List_Interface.html)
  and [Player](https://specifications.freedesktop.org/mpris/latest/Player_Interface.html)

## Goal

Resolve every required review finding before PR #192 becomes mergeable by
policy. Keep each behavior fix separate from structural cleanup, preserve the
21-occurrence contract, and add checks that fail when packaged or mixed-version
installs cannot load the sidecar.

## Architecture decisions

1. Treat `occurrenceId` as a capability boundary. A non-empty queue with
   missing, duplicate, or unmatched occurrence identities cannot provide exact
   TrackList semantics. Disable TrackList for that snapshot instead of
   inventing unstable identities. Preserve the existing Player metadata path.
2. Split MPRIS updates into hot Player state and cold queue state. Position
   ticks must not clone or reconcile the full queue.
3. Coalesce pending state rather than queueing every snapshot. One emitter owns
   reconciliation and D-Bus delivery, with at most one newer state waiting.
4. Handle current-occurrence `GoTo` as a seek-only restart. A different
   occurrence uses `ChangeToIndex` and waits for the selected item before the
   zero seek.
5. Remove the raw queue-identity diagnostic unless review establishes a current
   need for it. Keep the production occurrence allocator and its tests.
6. Refactor only after behavior and performance fixes pass. This keeps fixes
   reviewable and prevents file movement from hiding regressions.

## Dependency graph

```text
Task 1: package module ----\
                           +--> Checkpoint A
Task 2: sidecar CI --------/

Task 3: identity gate --------> Task 4: GoTo and Seeked
                                      |
Task 5: hot/cold updates ------------+--> Task 6: bounded delivery
                                              |
                                              v
                                     Checkpoint B
                                              |
                         Task 7: module split and dead code
                         Task 8: remove private diagnostic
                                      |
                                      v
                         Task 9: final runtime verification
```

Tasks 1 and 2 can run in parallel. Tasks 3 and 5 touch the MPRIS state boundary
and should run sequentially. Task 4 depends on the identity policy. Task 6
depends on the hot/cold update contract. Tasks 7 and 8 can run in parallel after
Checkpoint B.

## Task list

### Phase 1: Shipping and quality gate

- Task 1: Install the complete sidecar artifact.
- Task 2: Run sidecar contract tests in `make check`.

### Checkpoint A: Shippable sidecar

- Every install recipe contains each local module required by `preload.js`.
- Rust and sidecar checks pass from a clean tree.
- Human review confirms the packaging and CI scope before MPRIS behavior work.

### Phase 2: Correctness and hot-path behavior

- Task 3: Gate TrackList on valid occurrence identities.
- Task 4: Make `GoTo` restart ordered and observable.
- Task 5: Separate position updates from queue reconciliation.
- Task 6: Coalesce MPRIS update delivery.

### Checkpoint B: Correct and bounded runtime

- Focused tests cover invalid identities, current and duplicate `GoTo`,
  `Seeked`, position ticks, rapid updates, and stalled emission.
- A 500-item position tick performs no queue clone or projection reconciliation.
- Human review approves behavior before structural cleanup.

### Phase 3: Structure and privacy

- Task 7: Split the MPRIS module and remove unused change state.
- Task 8: Remove the raw queue-identity diagnostic.

### Phase 4: Merge verification

- Task 9: Repeat packaged and session-bus verification.

### Checkpoint C: Ready for PR review

- `make check` includes Rust, sidecar, and assembled-sidecar checks.
- Runtime evidence covers a valid branch sidecar and an incompatible legacy
  sidecar without signal churn.
- PR #192 documents fixes, verification, and remaining limits.
- The human decides whether to merge the remediation branch into the feature
  branch and approve the feature PR.

## Commit strategy

Use one verified commit per task. Keep packaging, behavior fixes, performance,
refactoring, and documentation separate. Do not squash the behavior fixes into
the module split.

## Risks and controls

| Risk | Impact | Control |
|---|---|---|
| Disabling TrackList removes Player track identity | High | Preserve the existing catalog/library-based Player metadata fallback and test mixed-version behavior. |
| Hot/cold updates miss queue metadata or artwork changes | High | Classify queue, current-item, and artwork events explicitly and test each route. |
| Coalescing drops a required intermediate signal | High | Emit committed state in order and retain the newest pending state; test rapid structural and metadata changes. |
| `GoTo` zero seek races MusicKit | High | Use seek-only handling for the current occurrence and event-gated seek for a different occurrence. |
| Refactoring hides behavior changes | Medium | Complete behavior checkpoints first and move code without changing tests or contracts. |
| Packaging checks pass without running Electron | High | Verify required local modules in the assembled sidecar and retain a focused preload smoke check. |

## Planning limits

The review used the fresh code graph generation from 2026-09-01 and direct
source reads for excluded documentation and JavaScript tests. Packaging files
sit outside the changed-file graph but are direct consumers of the new sidecar
module. A clean graph coverage result does not prove completeness.

## Open questions

1. Confirm the capability-gate policy: disable TrackList for invalid occurrence
   identities while retaining Player metadata, rather than support approximate
   duplicate matching.
2. Confirm removal of `SLIPMAT_QUEUE_IDENTITY_PROBE`; retain it only if current
   debugging still needs raw MusicKit shape information.
3. Confirm whether the remediation should return to PR #192 as individual
   commits or through a temporary stacked pull request.
