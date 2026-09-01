# Spec: Clear account state on sign-out

- Status: Approved on 2026-09-02
- Branch: `fix/sign-out-clear-state`
- Supersedes: client-only sign-out cleanup

## Objective

Make sign-out an account boundary across Slipmat, Climat, and `slipmatd`.
After Apple Music authorization ends, no client may show or act on library,
queue, current-track, search, page, token, or saved playback state from the
previous account.

Today the daemon changes to `Stage::SignedOut` but keeps its cached library and
player model. The GTK client clears its own copy when it requests sign-out,
while Climat continues to receive the daemon's old rows and snapshot. That
makes a signed-out session look playable and lets connected clients disagree.

Success means the daemon owns one idempotent cleanup path, every client observes
the cleared state, and a later daemon start or Apple sign-in cannot resurrect
data from the signed-out account.

## Scope

### In scope

- Clear account-derived daemon state when authorization becomes false and when
  the sidecar confirms `SignedOut`.
- Clear the library cache and saved playback session from disk.
- Clear library rows, catalog results, open pages, queue, current-track
  metadata, artwork selection, and pending playback state in connected clients.
- Stop serving old rows or snapshots through IPC after sign-out.
- Prevent a library refresh started by the old session from writing data after
  sign-out or after a different account signs in.
- Keep cleanup idempotent because the sidecar can report both authorization
  loss and completed sign-out.

### Out of scope

- Changing Apple's cookie or web-storage clearing behavior in the sidecar.
- Adding a Climat sign-out key or account-management screen.
- Deleting reusable artwork files.
- Deleting user preferences such as volume, sort order, theme, or key choices.
- Deleting the global unplayable-track cache, which describes catalog failures
  rather than one account.
- Adding or changing daemon IPC messages.

## Behavior contract

### Daemon ownership

`slipmatd` is the source of truth for account state. A client sends
`Request::SignOut` and waits for daemon events. It does not erase shared cache
files or claim sign-out succeeded on its own.

The daemon runs the same cleanup when either of these existing sidecar events
arrives:

- `Authorization { authorized: false }`, which removes access immediately.
- `SignedOut`, which confirms the sidecar finished clearing its stored Apple
  session and acts as an idempotent backstop.

Cleanup happens before the signed-out stage is published. Once a client sees
`Stage::SignedOut`, every new snapshot, queue response, and browse response must
already be empty.

### State removed

The daemon clears:

- Music user tokens and storefront-bound client state.
- Library tracks, albums, artists, and playlists.
- Queue items and position.
- Current-track identity, metadata, playback state, position, duration,
  shuffle, and repeat state.
- Current artwork path.
- Pending queue verification, retry, resume, restart, and command-follow-up
  state tied to the old player session.
- The saved library cache and playback-session files.

The daemon keeps:

- Desktop stream volume.
- Global unplayable catalog IDs.
- Non-account application configuration.
- Reusable artwork files that are no longer referenced by the model.

### Events sent to clients

The daemon publishes enough existing events for every connected client to
converge without reconnecting:

- `Event::Stage(Stage::SignedOut)`
- An empty `Event::Queue`
- An empty `Event::Snapshot`
- `Event::LibraryChanged`

Repeating cleanup must remain harmless and must not republish old data.

### Client behavior

On `Stage::SignedOut`, Climat clears its local snapshot, queue, library rows,
catalog results, open page, filters, searches, and pending pagination before it
draws the sign-in prompt. Navigation and activation cannot reach a stale row.

The GTK client waits for the daemon's signed-out event, then clears the same
presentation state. Its existing optimistic cleanup moves out of the request
path so a client cannot disagree with a daemon that reports a sign-out error.
Cache-file ownership moves to the daemon.

If several clients are connected, all of them show the signed-out state and
empty content from the same daemon event sequence.

### Persistence and asynchronous work

A daemon restarted after sign-out starts without the previous library or
playback session. Signing in again refreshes the library normally and starts
with no restored queue from the old account.

Every asynchronous library refresh is tied to the authorization session that
started it. A result from an older session must be discarded before it can
write the library cache, replace the daemon library, or publish
`LibraryChanged`, even if a new account is already ready.

## Tech stack

- Rust 2024 workspace
- Tokio local tasks and broadcast events
- Existing `slipmat_core::ipc::{Event, Request, Stage}` contract
- Existing `slipmat_core::{library_cache, session}` persistence helpers
- Ratatui/Crossterm Climat client
- GTK4/libadwaita Slipmat client

No dependency or protocol change is required.

## Commands

```bash
# Focused daemon behavior
cargo test -p slipmatd sign_out -- --nocapture

# Client state handling
cargo test -p climat signed_out -- --nocapture
cargo test -p slipmat signed_out -- --nocapture

# Workspace quality gate
make check

# Manual multi-client verification with the repository sidecar
SLIPMAT_SIDECAR="$PWD/sidecar" cargo run -p climat
cargo run -p slipmat
```

The manual test uses an account that can be signed out safely. It must not
inspect, log, or copy Apple credentials or tokens.

## Project structure

```text
docs/specs/SPEC-sign-out-clear-state.md
    Behavior contract and verification record.

crates/slipmatd/src/serve.rs
    Authorization events, account cleanup, stale-refresh rejection, and event
    publication.

crates/slipmatd/src/state.rs
    Daemon model fields that become empty at the account boundary.

crates/climat/src/main.rs
    Climat's local signed-out reset and focused test.

crates/slipmat/src/app/supervise.rs
crates/slipmat/src/app/mod.rs
    GTK daemon-event handling and presentation reset.

crates/slipmat-core/src/library_cache.rs
crates/slipmat-core/src/session.rs
    Existing persistence cleanup helpers, reused without a new format.
```

## Code style

Keep one daemon operation responsible for clearing account state. Call it from
both sidecar events instead of duplicating field resets.

```rust
fn clear_account_state(daemon: &Rc<Daemon>) {
    let mut model = daemon.model.borrow_mut();
    model.tokens = None;
    model.library = Library::default();
    model.player = PlayerState::new();
    model.art_path = None;
    drop(model);

    library_cache::clear();
    session::clear();
}
```

The final implementation may use existing constructors with equivalent
behavior. Keep comments for ordering and stale-result constraints, not for
restating assignments. Run rustfmt and preserve the repository's existing
naming and error-handling conventions.

## Testing strategy

### Daemon unit tests

- Start with a populated model, saved library cache, saved playback session,
  and pending playback fields.
- Deliver `Authorization { authorized: false }` and assert that account state
  and persistence are empty while volume and non-account state remain.
- Deliver `SignedOut` after authorization loss and prove the second cleanup is
  harmless.
- Subscribe before cleanup and assert that stage, empty queue, empty snapshot,
  and library-change events are published.
- Start a library refresh under one authorization session, sign out or replace
  the session, then prove its late result cannot mutate memory or disk.

### Client unit tests

- Populate Climat's browser, queue, snapshot, search, and page state; deliver
  `Stage::SignedOut`; assert that only the sign-in state remains.
- Populate the GTK presentation state; deliver the daemon's signed-out stage;
  assert that rows, pages, queue projections, and current-track presentation
  are cleared.
- Prove that requesting sign-out alone does not make a client claim completion.

### Runtime verification

1. Start GTK and Climat against the same daemon while signed in.
2. Load library rows and play a track.
3. Request sign-out from one client.
4. Confirm both clients stop showing the track, queue, pages, searches, and
   library, and Climat shows only its sign-in prompt.
5. Confirm browse, queue, and snapshot IPC requests return empty state.
6. Restart the daemon while still signed out and confirm no old data returns.
7. Sign in again and confirm the library refreshes without restoring the old
   queue.

## Boundaries

### Always

- Treat authorization loss as an account-data boundary in the daemon.
- Clear memory before publishing `Stage::SignedOut`.
- Clear library and playback-session persistence in the daemon.
- Reject late asynchronous results from an older authorization session.
- Test repeated cleanup and multi-client convergence.
- Run `make check` before committing implementation changes.

### Ask first

- Add or change an IPC event or request.
- Add a dependency or change cache formats.
- Delete artwork, preferences, or other non-session data.
- Change sidecar cookie and web-storage behavior.

### Never

- Log tokens, cookies, account identifiers, or credentials.
- Make one client solely responsible for shared cleanup.
- Leave old rows actionable behind a signed-out prompt.
- Accept a late refresh after its authorization session ended.
- Remove or weaken an existing test to make the cleanup pass.

## Success criteria

1. After authorization becomes false, daemon library, queue, snapshot, tokens,
   artwork selection, and pending playback state contain no old account data.
2. Library-cache and playback-session files are absent after sign-out.
3. Every connected client clears its account-derived presentation and shows
   the signed-out experience without reconnecting.
4. Browse, queue, and snapshot requests cannot return old account data after
   `Stage::SignedOut`.
5. A daemon restart while signed out does not restore the old library or queue.
6. A late refresh from the old authorization session cannot repopulate memory
   or disk, including after another account signs in.
7. Signing in again refreshes the new account's library and does not restore
   the previous account's queue.
8. Volume, preferences, reusable artwork files, and global unplayable IDs
   survive sign-out.
9. No dependency, cache-format, or IPC protocol change is introduced.
10. Focused tests and `make check` pass, followed by the multi-client runtime
    verification.

## Open questions

None. Account-derived memory and persistence are cleared on sign-out;
non-account caches and preferences remain.
