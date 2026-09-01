# Spec: Climat Apple Music sign-in

- Status: Approved
- Issue: <https://github.com/SoftARV/Slipmat/issues/194>
- Branch: `feat/climat-sign-in`

## Objective

Let a person authorize Apple Music from Climat on a fresh installation without
opening the GTK client first.

When the daemon reports `Stage::SignedOut`, Climat will show an actionable
prompt. Pressing `Enter` will send the existing `Request::SignIn` request. The
daemon will forward it to the sidecar as `Command::ShowLogin`, and the sidecar
will reveal Apple's sign-in window. Climat will keep its current state until the
daemon reports the result.

Climat still needs a graphical Wayland or X11 session because the sidecar uses
Chromium for Widevine playback and for Apple's sign-in window.

### In scope

- Replace the signed-out instruction to open Slipmat with an actionable Climat
  prompt that names the `Enter` key.
- Send `Request::SignIn` when `Enter` is pressed during `Stage::SignedOut`.
- Preserve the current `Enter` activation behavior outside the signed-out
  stage.
- Handle the sidecar's authorization result in `slipmatd`, including the ready
  stage, login-window hide, session restore, and library refresh.
- Cover the key-to-request behavior with a focused test.
- Document first-run sign-in in Climat's README and key reference.

### Out of scope

- Adding or changing daemon IPC messages.
- Changing how `slipmatd` forwards sign-in to the sidecar.
- Changing the sidecar's sign-in window lifecycle.
- Supporting Climat on a headless machine or through a display-less SSH
  session.
- Adding sign-out or account management to Climat.
- Persisting Apple tokens or credentials in Climat.

## Behavior contract

### Signed-out prompt

- `Stage::SignedOut` shows a prompt such as `[Enter] Sign in to Apple Music`.
- The prompt replaces `Not signed in: open Slipmat to sign in`.
- The prompt appears in the existing non-ready player area. It does not add a
  dialog, pane, or new navigation state.

### Key handling

- `Enter` sends one `Request::SignIn` for each key press while the current stage
  is `Stage::SignedOut`.
- The signed-out handling runs before browser activation, including when a
  filter or catalog field still has typing focus.
- Climat does not set its stage to ready or assume authorization succeeded.
- `Enter` keeps its current row, page, and queue activation behavior during
  `Stage::Connecting`, `Stage::Ready`, and `Stage::Broken`.
- Existing leave and quit behavior remains unchanged.

### Daemon response

- Climat relies on the existing daemon path:
  `Request::SignIn` to `Command::ShowLogin` to the sidecar login window.
- `slipmatd` maps the sidecar's existing `Authorization` event to
  `Stage::Ready` or `Stage::SignedOut`. On success it sends the existing
  `Command::Hide`, restores once, and refreshes the library.
- Climat redraws from the next daemon `Event::Stage` message.
- `Stage::Ready` restores the normal player view and key behavior.
- `Stage::SignedOut` keeps the actionable prompt visible.
- `Stage::Broken` uses the existing playback-unavailable message.
- Existing `Event::Error` handling reports any daemon error.

## Tech stack

- Rust 2024 workspace
- Ratatui 0.29
- Crossterm 0.28 key events
- Tokio channels through Climat's existing `link::Link`
- Existing `slipmat_core::ipc::{Request, Stage}` contract

This work needs no dependency or protocol change.

## Commands

```bash
# Run focused Climat tests
cargo test -p climat

# Lint Climat and its tests
cargo clippy -p climat --all-targets -- -D warnings

# Run the repository quality gate
make check

# Exercise the flow with the repository sidecar in a graphical session
SLIPMAT_SIDECAR="$PWD/sidecar" cargo run -p climat
```

The runtime check needs a sidecar profile with no active Apple session. It must
not clear or overwrite a person's working profile to manufacture that state.

## Project structure

```text
docs/specs/SPEC-climat-sign-in.md
    Feature contract and acceptance criteria.

crates/climat/src/main.rs
    Stage-aware Enter handling and its focused request test.

crates/climat/src/ui.rs
    Actionable signed-out prompt and prompt test.

crates/climat/README.md
    First-run sign-in instructions and key reference.

crates/slipmat-core/src/ipc.rs
sidecar/main.js
    Existing sign-in protocol. These files keep their current behavior.

crates/slipmatd/src/serve.rs
    Authorization-stage transition, window hide, restore, refresh, and its
    focused regression test.
```

## Code style

Keep the stage check beside Climat's existing key routing. Send user intent to
the daemon and wait for its event, as every other Climat action does.

```rust
if matches!(app.stage, Stage::SignedOut) && code == KeyCode::Enter {
    link.send(Request::SignIn);
    return true;
}
```

Follow existing conventions:

- Keep comments sparse and explain only ordering constraints.
- Use the existing IPC type instead of adding a Climat-specific command.
- Add the smallest test seam needed to inspect the request sent through
  `link::Link`.
- Do not add `.unwrap()` or `.expect()` outside tests and `main.rs`.

## Testing strategy

### Unit tests

- Pressing `Enter` during `Stage::SignedOut` sends `Request::SignIn` through an
  in-memory `link::Link`.
- Pressing `Enter` during `Stage::Ready` keeps the existing activation path and
  does not send `Request::SignIn`.
- The signed-out status text names `Enter` and Apple Music sign-in.
- Other stage text remains unchanged.
- A sidecar `Authorization { authorized: true }` event changes the daemon stage
  from signed out to ready.

The request test should exercise `on_key` rather than duplicate its condition
in a helper test. A test-only in-memory link is enough; this feature does not
need a frontend command abstraction.

### Runtime verification

- Start Climat with a signed-out sidecar profile under Wayland or X11.
- Confirm that Climat shows the `Enter` sign-in prompt.
- Press `Enter` and confirm that Apple's sign-in window appears.
- Complete authorization and confirm that the sidecar hides its window.
- Confirm that Climat receives `Stage::Ready`, loads the library, and can start
  playback.
- Restart Climat and confirm that the persisted sidecar session reaches
  `Stage::Ready` without another sign-in prompt.

### Runtime result

Verified under Wayland on 2026-09-01 with the repository sidecar. A signed-out
Climat showed the `Enter` prompt and opened Apple's window without launching
the GTK client. The first pass exposed a daemon bug: the sidecar emitted its
authorization result, but `slipmatd` ignored that event and left the window
open. A focused regression test now covers the missing stage transition, and
the daemon handles that result through the existing protocol.

After rebuilding, the authenticated profile reached `Stage::Ready`, the
sidecar window was hidden, and the daemon loaded 535 library songs. Playback
started for “505” by Arctic Monkeys. Restarting Climat reused the persisted
Apple session and returned directly to `Stage::Ready` without another prompt.

## Boundaries

### Always

- Reuse `Request::SignIn` and the daemon's existing login route.
- Wait for daemon events before changing the displayed stage.
- Keep `Enter` activation unchanged outside `Stage::SignedOut`.
- Keep the signed-out prompt and key behavior covered by tests.
- Run `make check` before committing implementation changes.

### Ask first

- Choose a key other than `Enter`.
- Add or upgrade a dependency.
- Change daemon IPC or sidecar NDJSON.
- Disable normal keys beyond the signed-out `Enter` override.
- Add sign-out or other account controls to Climat.

### Never

- Launch the GTK client to perform sign-in.
- Reimplement `Command::ShowLogin` in Climat.
- Mark the client ready before the daemon reports `Stage::Ready`.
- Store, log, or display Apple tokens or credentials.
- Claim support for a headless session.

## Success criteria

1. A signed-out Climat session shows an `Enter` action for Apple Music sign-in.
2. Pressing `Enter` in that stage sends `Request::SignIn` and no other request
   through the existing daemon connection.
3. The daemon reveals Apple's sidecar sign-in window without starting Slipmat.
4. Climat waits for daemon stage events and returns to its normal player view
   after `Stage::Ready`.
5. `Enter` retains its existing behavior in every other stage.
6. Climat's README explains the graphical-session requirement and the sign-in
   action.
7. Focused tests, Climat clippy, and `make check` pass.

## Open questions

None. The `Enter` key and the existing IPC route are approved.

## References

- GitHub issue #194: <https://github.com/SoftARV/Slipmat/issues/194>
- Existing request: `crates/slipmat-core/src/ipc.rs`
- Existing daemon route: `crates/slipmatd/src/serve.rs`
- Existing GTK caller: `crates/slipmat/src/app/mod.rs`
