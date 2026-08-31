# Spec: MPRIS TrackList context

- Status: Draft for review
- Issue: <https://github.com/SoftARV/Slipmat/issues/191>
- Branch: `feat/mpris-tracklist`

## Objective

Expose playback context through the optional
`org.mpris.MediaPlayer2.TrackList` interface. MPRIS clients should be able to
inspect nearby tracks and jump to one without opening a Slipmat frontend.

Slipmat will expose at most 21 queue occurrences: the current occurrence, up
to 10 before it, and up to 10 after it. Near either end of the queue, the
window shifts to include as many occurrences as possible without exceeding
21. This follows the MPRIS recommendation to expose a short context around the
current track instead of a complete playlist.

This feature belongs to `slipmatd`, which owns playback and MPRIS. It must work
without the GTK client or any other visible frontend.

### In scope

- Report `HasTrackList` as `true`.
- Export the TrackList interface from the existing MPRIS object.
- Expose an ordered, read-only `Tracks` property.
- Return metadata through `GetTracksMetadata`.
- Implement `GoTo` through MusicKit's existing queue cursor command.
- Notify clients when the context window or its metadata changes.
- Preserve every existing MPRIS Player property, method, and signal.

### Out of scope

- Exposing the entire queue when it contains more than 21 occurrences.
- Adding, removing, or reordering tracks through MPRIS.
- The MPRIS Playlists interface.
- New artwork downloads for non-current tracks.
- GTK, Climat, or IPC protocol changes.

## Behavior contract

### Context window

- An empty MusicKit queue produces an empty `Tracks` property.
- A queue of 21 or fewer occurrences exposes every occurrence.
- A longer queue exposes a window of 21 occurrences.
- The window prefers 10 occurrences on each side of the current one.
- At the start or end of the queue, the window shifts to keep its full width.
- A loaded queue with no current MusicKit item uses `queue_position` as the
  window anchor but reports `NoTrack` as the current MPRIS track.
- The projection reads `PlayerState.queue`. It never authors or repairs the
  MusicKit queue.

### Track identity

- Every exposed occurrence receives an opaque, valid D-Bus object path.
- Two occurrences of the same Apple track receive different object paths.
- Removing or inserting another occurrence does not change the identifiers of
  retained occurrences.
- Sliding the context window preserves identifiers for occurrences that remain
  visible.
- When MusicKit has a current occurrence, the Player interface's
  `mpris:trackid` matches that occurrence's TrackList identifier.
- A current item that cannot be associated with the queue may keep a standalone
  Player identifier; TrackList reports `NoTrack` as its current occurrence.
- Clients must treat identifiers as opaque and must not infer an Apple id or a
  queue position from them.

### Metadata

`GetTracksMetadata` returns one metadata map per known requested identifier, in
request order. It ignores stale or unknown identifiers. Every returned map
contains:

- `mpris:trackid`
- `mpris:length`, when the duration is known
- `xesam:title`
- `xesam:artist`, when present
- `xesam:album`, when present
- `xesam:trackNumber`, when non-zero
- `mpris:artUrl` only when Slipmat already has a local cached artwork file for
  that occurrence

TrackList metadata must not trigger Apple API requests or artwork downloads.
Lengths use signed microseconds as required by MPRIS.

### Methods and properties

- `Tracks` returns the current context window in queue order.
- `CanEditTracks` always returns `false`.
- `AddTrack` and `RemoveTrack` return `NotSupported` and do not change playback.
- `GoTo` resolves an exposed occurrence identifier to its current MusicKit
  queue index and sends `Command::ChangeToIndex`.
- `GoTo` with `NoTrack`, an unknown identifier, or an identifier that left the
  window has no effect.
- `GoTo` must not rebuild the queue with `SetQueue`.

### Change notification

- When window membership or order changes, emit `TrackListReplaced` with the
  complete new window and the current occurrence or `NoTrack`.
- Invalidate the `Tracks` property through TrackList `PropertiesChanged` when
  its value changes. Do not include the new value in that notification.
- When metadata changes without changing window membership or order, emit
  `TrackMetadataChanged` for each affected visible occurrence.
- A current-track change that leaves the same window intact relies on the
  Player `Metadata` property change. It does not emit a false
  `TrackListReplaced` event.
- This version does not emit granular `TrackAdded` or `TrackRemoved` signals.

## Tech stack

- Rust 2024 workspace, version `0.11.0-dev`
- `mpris-server` 0.10.0
- Tokio current-thread runtime with a `LocalSet`
- `mpris_server::LocalServer` and the local root, Player, and TrackList traits
- Existing `PlayerState` queue projection and sidecar `Command::ChangeToIndex`

No new dependency is required. The current ready-made `mpris_server::Player`
supports only the root and Player interfaces, so the implementation must move
the existing behavior to a manual local server before adding TrackList.

## Commands

```bash
# Build the workspace
cargo build --workspace

# Run focused unit tests
cargo test -p slipmat-core mpris

# Run every project check
make check

# Build the daemon, then run the GTK client against the repository sidecar
cargo build -p slipmatd
SLIPMAT_SIDECAR="$PWD/sidecar" cargo run -p slipmat

# Inspect the exported interfaces
busctl --user introspect org.mpris.MediaPlayer2.Slipmat /org/mpris/MediaPlayer2

# Verify the root capability
busctl --user get-property org.mpris.MediaPlayer2.Slipmat \
  /org/mpris/MediaPlayer2 org.mpris.MediaPlayer2 HasTrackList

# Read the context window
busctl --user get-property org.mpris.MediaPlayer2.Slipmat \
  /org/mpris/MediaPlayer2 org.mpris.MediaPlayer2.TrackList Tracks
```

Use `busctl call` with an identifier returned by `Tracks` to verify
`GetTracksMetadata` and `GoTo`. Confirm the visible queue and playback cursor
through either Slipmat frontend.

## Project structure

```text
docs/specs/SPEC-mpris-tracklist.md
    Feature contract and acceptance criteria.

crates/slipmat-core/src/mpris.rs
    Manual LocalServer implementation, Player state, context projection,
    occurrence identity, metadata conversion, signals, and focused tests.

crates/slipmat-core/src/player/protocol.rs
    Existing Item metadata and Command::ChangeToIndex contract. No wire change.

crates/slipmat-core/src/player/state.rs
    Existing MusicKit-owned queue projection. No new source of truth.

crates/slipmatd/src/bus.rs
    Builds the MPRIS state from the daemon model and routes GoTo to the sidecar.

crates/slipmatd/src/serve.rs
    Continues publishing MPRIS updates with daemon snapshots.
```

The TrackList projection belongs in `slipmat-core::mpris`, not in a frontend.
The daemon supplies playback facts; the MPRIS module owns D-Bus-specific
identity and notification state.

## Code style

Use small domain helpers, explicit state, and checked arithmetic. Keep MPRIS
types at the bus boundary.

```rust
const TRACK_LIST_LIMIT: usize = 21;
const TRACK_LIST_RADIUS: usize = 10;

fn context_window(queue_len: usize, anchor: usize) -> std::ops::Range<usize> {
    if queue_len == 0 {
        return 0..0;
    }

    let width = queue_len.min(TRACK_LIST_LIMIT);
    let anchor = anchor.min(queue_len - 1);
    let start = anchor
        .saturating_sub(TRACK_LIST_RADIUS)
        .min(queue_len - width);
    start..start + width
}
```

Follow existing conventions:

- Use `anyhow::Result` internally and D-Bus errors at the interface boundary.
- Do not use `.unwrap()` or `.expect()` outside tests and `main.rs`.
- Keep comments short and explain only constraints the code cannot express.
- Preserve the single-threaded `Rc` and `RefCell` model used by `slipmatd`.
- Add no abstraction before the behavior needs it.

## Testing strategy

### Unit tests in `slipmat-core::mpris`

- Empty, short, centered, start-edge, and end-edge windows.
- A queue position beyond the current queue length.
- A 500-entry queue still exposes exactly 21 occurrences.
- Duplicate Apple tracks receive distinct identifiers.
- Insertions, removals, moves, and window slides preserve retained identifiers.
- Removing one duplicate does not rename the retained duplicate.
- Track identifiers form valid D-Bus object paths.
- Metadata includes the required identifier and converts milliseconds to
  microseconds.
- Metadata omits unknown duration, blank optional text, and uncached artwork.
- `GoTo` resolves a visible occurrence to the correct full-queue index.
- Stale and `NoTrack` identifiers produce no command.
- Structural changes, metadata changes, and position-only changes select the
  correct notification behavior.
- Existing playback status, loop status, volume, shuffle, seeking, capability,
  and metadata tests continue to pass after the local-server migration.

### Runtime verification

- D-Bus introspection lists the TrackList interface.
- `HasTrackList` is `true`.
- `Tracks` contains at most 21 unique object paths in queue order.
- `GetTracksMetadata` describes every identifier returned by `Tracks`.
- A queue with the same song twice exposes two identifiers and `GoTo` reaches
  each occurrence.
- Advancing through a queue slides the window and keeps playback gapless.
- An MPRIS client can follow replacement and metadata signals without polling
  the daemon socket.
- Existing shell Play, Pause, Next, Previous, Seek, volume, shuffle, repeat,
  Raise, and Quit behavior remains unchanged.

## Boundaries

### Always

- Treat MusicKit as the queue source of truth.
- Keep Player metadata and TrackList current identity consistent.
- Preserve stable occurrence identifiers for retained queue entries.
- Emit the MPRIS notifications required for every exposed state change.
- Test duplicate tracks and long queues.
- Run `make check` before committing implementation changes.

### Ask first

- Add or upgrade a dependency.
- Change the 21-occurrence limit.
- Expose the complete queue.
- Add granular `TrackAdded` or `TrackRemoved` reconciliation.
- Change the daemon IPC or sidecar NDJSON protocols.
- Fetch artwork for non-current TrackList entries.

### Never

- Use catalog ids or queue positions as occurrence identifiers.
- Rebuild the MusicKit queue to implement `GoTo`.
- Let MPRIS mutate queue membership while `CanEditTracks` is false.
- Move TrackList ownership into GTK or Climat.
- Block the daemon's local runtime on network or file I/O.
- Persist or log Apple tokens.
- Weaken existing MPRIS behavior to make the migration smaller.

## Success criteria

1. D-Bus introspection reports `org.mpris.MediaPlayer2.TrackList` on
   `/org/mpris/MediaPlayer2`.
2. `org.mpris.MediaPlayer2.HasTrackList` returns `true`.
3. `Tracks` returns zero to 21 unique, ordered object paths selected by the
   context-window contract.
4. Duplicate songs receive distinct identifiers.
5. Retained occurrences keep their identifiers after unrelated queue edits or
   a context-window slide.
6. `GetTracksMetadata` returns standards-compliant metadata for every current
   TrackList identifier.
7. `CanEditTracks` returns `false`; Add and Remove cannot change the queue.
8. `GoTo` starts the selected visible occurrence through
   `Command::ChangeToIndex` and never calls `SetQueue`.
9. Unknown or stale `GoTo` identifiers leave playback unchanged.
10. Clients receive `TrackListReplaced` and invalidated `Tracks` notifications
    when the exposed sequence changes.
11. Clients receive `TrackMetadataChanged` when visible metadata changes in
    place.
12. Position ticks and unrelated Player-property changes produce no TrackList
    traffic.
13. Gapless playback remains intact across a natural queue transition while
    TrackList is active.
14. Existing bidirectional MPRIS controls and properties pass their tests and
    runtime checks.
15. `make check` passes with no new warnings.

## Open questions

None at the specification stage. Any change to the context size, signal model,
or read-only boundary requires a spec update before implementation.

## References

- MPRIS TrackList interface:
  <https://specifications.freedesktop.org/mpris/latest/Track_List_Interface.html>
- `mpris-server` 0.10.0:
  <https://docs.rs/crate/mpris-server/0.10.0>
- `LocalTrackListInterface`:
  <https://docs.rs/mpris-server/0.10.0/mpris_server/trait.LocalTrackListInterface.html>
- GitHub issue #191:
  <https://github.com/SoftARV/Slipmat/issues/191>
