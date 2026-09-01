# Climat Apple Music sign-in task checklist

Plan: [`tasks/archive/climat-sign-in-plan.md`](climat-sign-in-plan.md)
Specification:
[`docs/specs/SPEC-climat-sign-in.md`](../../docs/specs/SPEC-climat-sign-in.md)
Issue: [#194](https://github.com/SoftARV/Slipmat/issues/194)

Status: Completed and approved on 2026-09-02. Archived after manual
verification.

## Task 1: Make the signed-out prompt actionable

**Description:** Route `Enter` from Climat's signed-out state to the existing
daemon sign-in request, update the visible prompt, and cover the behavior with
focused tests.

**Acceptance criteria:**

- [x] `Stage::SignedOut` displays `[Enter] Sign in to Apple Music`, and pressing
  `Enter` sends `Request::SignIn` even if a text field retained typing focus.
- [x] Climat waits for daemon stage events; it does not mark authorization as
  complete or change the daemon and sidecar protocols.
- [x] `Enter` keeps its current activation behavior in every other stage, and
  existing leave and quit keys keep their behavior.

**Verification:**

- [x] Add a failing `on_key` test through an in-memory `link::Link`, then make
  it pass.
- [x] Add prompt coverage in `ui.rs`.
- [x] Run `cargo test -p climat`.
- [x] Run `cargo clippy -p climat --all-targets -- -D warnings`.

**Dependencies:** None.

**Files likely touched:**

- `crates/climat/src/main.rs`
- `crates/climat/src/link.rs`
- `crates/climat/src/ui.rs`

**Estimated scope:** Medium, 3 files.

## Checkpoint A: Automated behavior

- [x] Task 1 meets its acceptance criteria.
- [x] Focused tests fail without the behavior and pass with it.
- [x] Climat builds and passes clippy without warnings.
- [x] Human review authorizes runtime verification.

## Task 2: Verify sign-in and document the result

**Description:** Exercise Climat against a signed-out sidecar profile in a
graphical session, verify the complete authorization transition, and update the
user-facing documentation with the observed behavior.

**Acceptance criteria:**

- [x] `Enter` reveals Apple's sign-in window without launching Slipmat; the
  sidecar hides the window after authorization.
- [x] Climat receives `Stage::Ready`, loads the library, starts playback, and a
  restart reuses the persisted Apple session.
- [x] The README and specification describe the verified sign-in action and
  retain the graphical-session requirement.

**Verification:**

- [x] Run `SLIPMAT_SIDECAR="$PWD/sidecar" cargo run -p climat` with a signed-out
  test profile under Wayland or X11.
- [x] Complete sign-in, play a track, restart Climat, and record the result in
  the specification.
- [x] Run `make check`.
- [x] Review the final diff for unrelated code, dependency changes, protocol
  changes, debug output, and documentation drift.

**Dependencies:** Task 1 and Checkpoint A.

**Files likely touched:**

- `crates/climat/README.md`
- `crates/slipmatd/src/serve.rs`
- `docs/specs/SPEC-climat-sign-in.md`
- `tasks/todo.md`

**Actual scope:** Medium, 1 daemon file and 3 documentation files. Runtime
verification exposed the missing daemon authorization transition.

## Checkpoint B: Merge approval

- [x] Tasks 1 and 2 meet their acceptance criteria.
- [x] Focused tests and `make check` pass.
- [x] Runtime evidence covers sign-in, ready state, playback, and restart.
- [x] The implementation adds no dependency or protocol change.
- [x] The human has reviewed and approved the feature before merge.
