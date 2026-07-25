# CLAUDE.md

Project instructions for Claude Code. Read this fully before writing code.

## What this is

**Tonearm** — a small, native GNOME desktop app to play **Apple Music** on one
personal Linux laptop. Not a product, not multi-user, not cross-platform. One
user, one machine.

It is the sibling of **Dockyard** (a native GNOME Docker manager) and **Pitwall**
(a native GNOME GitHub Actions monitor), and shares their stack, architecture and
taste. The name is the arm that tracks the record groove: the precision part that
sits between the catalogue and the sound.

The app should be indistinguishable from a first-party GNOME application. If a
design decision would make it look like an Electron app or a generic Qt tool, it
is the wrong decision.

**Why this project exists.** Every Apple Music option on Linux — Cider, Sidra,
the various wrappers — is `music.apple.com` in a costume. You get a web page's
scroll behaviour, a web page's search field, and none of GNOME's taste. Tonearm
is the first one where the *interface is actually native*; the web engine is
present but never rendered.

**The headline feature is playback itself** — gapless transport with correct,
bidirectional **MPRIS** in the GNOME Shell applet and on the lock screen. Build
the rest in service of that. Browsing stays deliberately thin until transport is
perfect.

## Author context — read this, it changes how you should respond

The author is a senior frontend engineer (~10 years: TypeScript, React, React
Native, Node) who is **new to Rust**. Consequences:

- When you introduce ownership, borrowing, lifetimes, `Rc`/`Arc`/`RefCell`, or
  `async` pinning, **briefly explain why** in a comment or in your reply. Do not
  silently sprinkle `.clone()` to quiet the borrow checker — say what the
  ownership problem was and why the clone is the right or pragmatic fix.
- Analogies to React/Redux land well. relm4 *is* the Elm architecture; say so.
- Do not dumb down the Rust. Idiomatic code with explanation, not beginner code.
- Prefer clarity over cleverness. No macro tricks, no premature generics.
- The **sidecar is JavaScript**, which is home turf. Keep it small anyway —
  every line there is a line that isn't native.

## The constraint that shapes everything

Apple Music full-track playback is HLS + **Widevine** DRM (FairPlay only in
Safari). On Linux the only Widevine CDM that exists is the one Google ships
inside Chrome x86_64 and Chromium shells that bundle it.

| Path                            | Verdict                                                        |
| ------------------------------- | -------------------------------------------------------------- |
| WebKitGTK + MusicKit JS         | **Dead.** Ships Clear Key only; no Widevine CDM.                |
| Rust + GStreamer direct         | **Dead.** Cannot decrypt Widevine-protected HLS.                |
| Stock Electron                  | **Dead.** `navigator.requestMediaKeySystemAccess` is absent.    |
| `pywidevine` + an extracted CDM | **Rejected.** See rule 1. Out of scope, permanently.            |
| **castLabs Electron (`wvcus`)** | **Works.** Bundles the real CDM. What Sidra uses.               |

So a 100% native Apple Music player *cannot exist*. The honest ceiling — and the
whole design — is: **everything the user sees is native; the audio decoder is
invisible.**

Two Linux facts that follow, and that you must not design around:

- **No VMP signing needed.** Linux Widevine reports `PLATFORM_UNVERIFIED`; the
  castLabs EVS account is a macOS/Windows concern. Nothing to sign.
- **No persistent licences.** Therefore **Tonearm requires a network connection
  to play, always. Offline and downloaded playback are impossible** — not a v1
  cut, a permanent property of the platform. Never add a "download" affordance.

## The axis that differs from Pitwall

Pitwall polls a **remote, rate-limited HTTP API**. Tonearm supervises a
**long-lived local child process** and mirrors its state. Almost every rule below
follows from that one difference:

| Concern       | Pitwall (remote GitHub)          | Tonearm (local sidecar)                        |
| ------------- | -------------------------------- | ---------------------------------------------- |
| Transport     | HTTPS, latency, rate limits      | NDJSON over the child's stdin/stdout, sub-ms   |
| State         | We own it; poll refreshes it     | **The sidecar owns playback state; we mirror** |
| Failure mode  | A failed request → a toast       | Child death → **supervised restart**, not a toast and forget |
| Cadence       | Poll every 45s                   | Event-driven push; a 1s tick only for position |
| Auth          | OAuth device flow                | Apple's own login, shown once, cookie persists |
| Killer feat   | Desktop notifications            | **Gapless playback + MPRIS**                   |

## Stack (pinned — do not swap these out)

| Layer          | Crate                  | Version                                    |
| -------------- | ---------------------- | ------------------------------------------ |
| UI framework   | `relm4`                | 0.11 (features: `libadwaita`, `gnome_49`)  |
| Widgets        | `gtk4`, `libadwaita`   | via relm4 (do **not** add directly)        |
| MPRIS          | `mpris-server`         | 0.10                                       |
| HTTP           | `reqwest`              | 0.12 (`json`, `rustls-tls`, no default features) |
| Serde          | `serde`, `serde_json`  | 1                                          |
| Secret storage | `oo7`                  | 0.6 (Secret Service / GNOME Keyring)       |
| Async runtime  | `tokio`                | 1 (`rt-multi-thread`, `process`, `io-util`)|
| Timestamps     | `chrono`               | 0.4                                        |
| Logging        | `tracing`              | 0.1                                        |
| Sidecar shell  | castLabs ECS           | `github:castlabs/electron-releases#v43.0.0+wvcus` |

Rust edition 2024, plus `anyhow` 1 (rule 5) and `tracing-subscriber` 0.3 with
`env-filter` for `RUST_LOG`. Toolchain ≥ 1.93 (relm4 0.11's MSRV); libadwaita
≥ 1.8 / GTK ≥ 4.20 (the `gnome_49` floor). Verified on this machine: rustc
1.97.1, GTK 4.22.4, libadwaita 1.9.2, node v26.4.0, PipeWire 1.6.8.

**relm4 0.11's docs.rs build is broken.** Read the vendored source, which is the
exact version we compile against:

```bash
ls ~/.cargo/registry/src/*/relm4-0.11.0/src/
```

**relm4, not raw gtk4-rs.** Every component is a relm4 `Component` or
`FactoryComponent`. Reaching for `Rc<RefCell<>>` to share widget state is a sign
the state belongs in a model and the change belongs in an `update()`.

## Hard rules

### 1. The line we do not cross

Tonearm plays through **Apple's own MusicKit player**, using **Google's official
CDM**, inside a licensed session that requires an active Apple Music
subscription. It is a native front-end and a remote control for a player that
Apple ships.

It does **not**, and will never:

- strip or circumvent DRM;
- use an extracted CDM (`pywidevine` and the `device_client_id_blob` /
  `device_private_key` route that Music Assistant takes) — that means a Widevine
  device blob pulled off a rooted Android phone;
- persist, cache or re-encode decrypted audio;
- implement downloading, ripping, or "export to MP3".

If a request or a refactor heads that way, **name it and stop**. This is not a
style preference; it is the reason the project can exist in the open.

### 2. Never trust your training data for `mpris-server` or MusicKit JS

`mpris-server` went **0.9 → 0.10 in April 2026** and the API changed. Check
<https://docs.rs/mpris-server/0.10> before writing any call against it.
`mpris-player` is unmaintained and points here instead — never use it.

MusicKit JS has three major versions in the wild with different surfaces, and
`music.apple.com` ships whichever it likes. **Feature-detect, never assume** (see
rule 4).

### 3. MusicKit owns the queue. Rust mirrors it.

This is the single rule that makes gapless possible, and the easiest one to
break by accident.

MusicKit's gapless transition happens *inside its own queue advance*. If Rust
feeds tracks one at a time — play a song, wait for `ended`, send the next — every
boundary gets a gap and the headline feature is gone.

- Enqueue **once**: `setQueue({ songs: [...ids], startPosition: n })`.
- Skip with `changeToMediaAtIndex(i)`. **Never** a fresh `setQueue` to move
  within a queue that is already loaded.
- `PlayerState.queue` on the Rust side is a **projection**, reconciled from the
  sidecar's `queueDidChange` event. It is never the source of truth, and the UI
  never mutates it directly — it sends a command and waits for the echo.

Same discipline as Pitwall's "update rows in place": reconcile by id, rebuild
widgets only when membership actually changes.

### 4. The injected hook script is the one fragile surface

`sidecar/preload.js` reaches into a page Apple can change without warning. Keep
it **tiny and defensive**:

- Feature-detect every property (`MusicKit?.getInstance?.()`), never assume.
- Poll for readiness with a timeout, then **fail loudly** — a
  `sidecar/hook-failed` event that becomes an `adw::Toast` naming the fix
  ("Apple Music changed; Tonearm needs an update"). Never degrade silently into
  a dead player with a spinning UI.
- Do not scrape the DOM. Only `MusicKit.getInstance()` and its documented events.
  DOM scraping is what makes wrappers break monthly.

### 5. No `.unwrap()` / `.expect()` outside `main.rs` and tests

The sidecar can die, the network can drop, Apple can 403 a stale token. Every
failure becomes an `adw::Toast` or a state, never a crash. `anyhow::Result`
internally.

### 6. The sidecar is supervised, not fired-and-forgotten

If the child exits, Tonearm restarts it with backoff, replays the queue and
position, and toasts once. A dead sidecar must never present as a healthy,
silent player. This is the local analogue of Pitwall's "a present-but-dead token
must not render an empty, healthy-looking list".

### 7. Tokens never touch disk, logs, or error strings

The **Music User Token** goes to the Secret Service (GNOME Keyring) via `oo7`.
The **developer token** is **re-harvested from `MusicKit.getInstance()` on every
launch and never cached** — if Apple rotates it we follow automatically.
`settings.rs` persists *preferences*; a token is not one. Never `tracing` a
token, never interpolate one into an error.

### 8. Never block the GTK main thread

All HTTP and all sidecar I/O go through relm4 `Command`s. The sidecar's stdout
reader is a streaming `command` (not `oneshot_command`) — it is a genuine stream,
the one case Pitwall reserved it for. `update()` stays synchronous and fast.

### 9. Apple types must not leak into the UI

Map `api.music.apple.com` JSON into our own `Track` / `Album` / `Artist` /
`Playlist` / `Artwork` in `music/types.rs` at the boundary — "parse, don't
validate". Likewise, protocol types live only in `player/protocol.rs`. The
`view!` macro and `components/` see our types, never raw JSON and never
`reqwest`.

## Architecture

```
┌──────────────────────────────────────────────────┐
│  Tonearm — Rust · relm4 · libadwaita             │  ← 100% of what the user sees
│  library · search · queue · Now Playing          │
│  MPRIS · notifications · media keys · artwork    │
│  HTTPS ─────────────────► api.music.apple.com    │
└───────────────────────┬──────────────────────────┘
                        │  NDJSON over the child's stdin/stdout
                        │  ↓ setQueue · play · pause · seek · skip
                        │  ↑ tokens · nowPlayingItem · playbackState · position
┌───────────────────────▼──────────────────────────┐
│  sidecar/ — castLabs Electron, BrowserWindow      │  ← invisible after first login
│  show:false, loading real music.apple.com         │
│  MusicKit.getInstance()  +  Widevine CDM          │
│  → audio straight to PipeWire, untouched          │
└───────────────────────────────────────────────────┘
```

**Why the sidecar loads real `music.apple.com`** rather than our own MusicKit JS
page: it is the origin Apple's licence server already trusts, its session cookie
survives restarts, and it hands us a live developer token for free. A custom
origin would add DRM risk *and* the $99 Apple Developer Program, and buy nothing
— the page is never rendered.

This is **verified, not assumed**. The harvested developer token is a JWT whose
payload reads:

```json
{ "iss": "AMPWebPlay", "iat": …, "exp": …, "root_https_origin": ["apple.com"] }
```

`root_https_origin` pins it to `apple.com`. A custom-origin page could not use
this token at all, so serving our own MusicKit page would have forced the paid
developer account — the design is not merely convenient, it's the only free
route. Note also `exp`: the token is good for roughly **70 days**, which is why
rule 7 says re-harvest every launch instead of caching. Never log the token
itself; the claims above are safe to reason about, the signature is not.

**Why stdio and not a localhost socket:** no port to allocate, no auth token to
invent, and no other local process can drive your player. Chromium logs to
stderr; **stdout carries protocol only** — anything that prints to stdout in the
sidecar corrupts the channel.

### Sidecar rules learned the hard way

M1 took five rounds to get audio out, and every failure was silent. These are
the specific traps; do not re-introduce them.

- **The API token is origin-locked.** Send `Origin: https://music.apple.com` and
  a matching `Referer` on every `api.music.apple.com` request. Without them
  everything 401s no matter how valid the tokens are. A browser sets these
  automatically, so this bites *only* a native client.
- **Never invalidate the hook on `did-start-loading`.** It fires for SPA route
  changes and subresource loads, so it latches `hookReady` to false within
  seconds and every later command parks in `pending` forever. Use
  `did-start-navigation` filtered to main-frame, cross-document navigations —
  that is what actually replaces a preload context.
- **Never queue a command silently.** Parking one emits `cmd-queued`. A queued
  command and a dropped one are indistinguishable otherwise, and that ambiguity
  cost three debugging rounds.
- **`refreshTokens` bypasses `dispatch()`.** It is sent by `main.js` straight to
  the renderer, so it proves the renderer is alive and proves *nothing* about
  the Rust→sidecar path. Do not read it as evidence that commands are arriving.
- **MusicKit's queue position is signed.** It reports `-1` between `setQueue`
  and the first item becoming current. Use `Queue::index()`.
- **Timers in `main.js` must be module-level and cleared before re-arming.** The
  probe re-arms on every navigation; `const` locals shadowed the handles and
  leaked a nudger per navigation.
- **The window is genuinely unmapped (`WINDOW_MODE=hidden`), and that is
  verified.** The `--disable-renderer-backgrounding` /
  `--disable-background-timer-throttling` /
  `--disable-backgrounding-occluded-windows` switches were already in place when
  it was verified, so treat them as load-bearing rather than leftovers — pulling
  them may reintroduce a frozen renderer. `concealed` (mapped, 1x1,
  transparent) remains as an escape hatch for a compositor that needs it.
- **The sidecar must not look like a second app.** `app.setName('Tonearm')` plus
  `app.setDesktopName('dev.miguelrincon.Tonearm.desktop')`, or the shell invents
  a "tonearm-sidecar" entry with a generic icon.
- **The sidecar must not publish its own MPRIS player.** Chromium registers one
  the moment a page plays media, and grabs the hardware media keys with it —
  giving two identical "Tonearm" entries in the shell and letting an invisible
  browser win the race for Play/Pause. Disabled via
  `--disable-features=MediaSessionService,HardwareMediaKeyHandling`. Neither
  affects decoding. Tonearm owns MPRIS; the sidecar owns audio.
- **Diagnose by layer, in order:** `cmd-queued` → never dispatched.
  No `cmd-recv` → renderer never ran it. `cmd-recv` but no `cmd-done` → the
  command is hanging. `cmd-done` with a full queue but a non-playing state →
  playback is blocked, not failing.

```
src/
  main.rs            # RelmApp bootstrap, tracing, icon; locate + spawn the sidecar
  app.rs             # root Component: AppModel, AppMsg, CommandMsg, update, view
  settings.rs        # glib::KeyFile → ~/.config/tonearm/settings.ini. NEVER tokens.
  secret.rs          # oo7 wrapper: store / load / clear the Music User Token
  mpris.rs           # mpris-server 0.10 ↔ AppMsg bridge (both directions)
  notify.rs          # gio::Notification on track change (opt-in)
  player/
    mod.rs
    protocol.rs      # serde types for both directions. The whole contract, one file.
    sidecar.rs       # locate / spawn / supervise the Electron child; NDJSON codec
    state.rs         # PlayerState: now playing, position, queue *projection* (+ tests)
  music/
    mod.rs
    client.rs        # reqwest → api.music.apple.com; storefront; errors that name the fix
    types.rs         # Track / Album / Artist / Playlist / Artwork (+ tests). JSON stops here.
  components/
    mod.rs
    now_playing.rs   # the persistent bottom bar
    track_row.rs     # FactoryComponent → adw::ActionRow
    queue_view.rs    # the queue, reorderable
    library.rs       # playlists / albums / songs
    artwork.rs       # fetch + disk cache; MPRIS needs a file:// path
sidecar/
  package.json  main.js  preload.js    # ~200 lines of JS, total
data/
  dev.miguelrincon.Tonearm.desktop
  icons/hicolor/{16x16,...,512x512,scalable}/apps/dev.miguelrincon.Tonearm.{png,svg}
Makefile             # make install → ~/.local (no sudo); make sidecar; make check
```

Dependency direction is strictly one-way, as in the siblings:
`main → app → components/*`, and `app → player|music → types`.
`components/` never imports `reqwest` or `serde_json`.

The root model is roughly:

```rust
struct AppModel {
    player: PlayerState,               // mirror of the sidecar (rule 3)
    conn: SidecarState,                // Starting | AwaitingLogin | Ready | Restarting(u32)
    tokens: Option<Tokens>,            // developer + music user; never persisted (rule 7)
    library: FactoryVecDeque<TrackRow>,
    query: String,
    mpris: Option<mpris_server::Server<TonearmPlayer>>,
    settings: Settings,
    toast_overlay: adw::ToastOverlay,
}

enum AppMsg {
    // user intent
    PlayPause, Next, Previous, Seek(u64), SetVolume(f64),
    PlayTrack { queue: Vec<TrackId>, start: usize },   // one setQueue, never per-track
    JumpTo(usize), SetShuffle(bool), SetRepeat(Repeat),
    SearchChanged(String), SignIn, SignOut,
    ShowAbout, ShowPreferences, ShowShortcuts,
    Error(String),
}

// Pushed up from the sidecar's stdout, and from HTTP commands.
enum CommandMsg {
    SidecarEvent(player::protocol::Event),   // playback state, now playing, queue, tokens
    SidecarDied(String),                     // → supervised restart (rule 6)
    LibraryLoaded(Vec<Track>),
    ArtworkReady(TrackId, PathBuf),          // MPRIS needs file://, so cache to disk
    LoadFailed(String),
}
```

This is Redux with a compiler: actions in, one reducer, view derived from state.

## UI shape

- `adw::ApplicationWindow` > `adw::ToolbarView` > `adw::HeaderBar`, with a
  **persistent bottom bar** (`add_bottom_bar`) that is the Now Playing strip:
  artwork, title + artist, prev / play-pause / next, a seek `gtk::Scale` with a
  live position label, and volume. It is visible on every page — it is the app.
- Main content: `adw::NavigationView` over an `adw::ViewStack` (clamped) —
  **Library** (playlists / albums / songs) and **Search**. The queue is an
  `adw::Dialog` or a sidebar sheet from the bottom bar, not a page.
- Rows are `adw::ActionRow` with artwork as prefix; activating a row sends
  `PlayTrack` with the **whole containing list** as the queue and the row's index
  as `start` — never a single-track queue (rule 3).
- **Use libadwaita widgets, not raw GTK.** `adw::ActionRow`,
  `adw::PreferencesGroup`, `adw::AboutDialog`, `adw::StatusPage`,
  `adw::ToastOverlay`. That's where the native feel comes from. No custom CSS
  unless there is no libadwaita widget for the job — say why before adding any.
- **First run**: an `adw::StatusPage` explaining that Apple's sign-in window will
  open once. It is the genuine Apple login (with 2FA); after it succeeds the
  sidecar hides forever. Never re-show it except on explicit Sign Out → Sign In.
- Sidecar restarting, no subscription, offline, no results: distinct
  `adw::StatusPage`s. Errors: `adw::Toast`.

## MPRIS (the v1 bar)

`org.mpris.MediaPlayer2.Tonearm`, via `mpris-server` 0.10.

- Metadata: `mpris:trackid`, `mpris:length`, `mpris:artUrl`, `xesam:title`,
  `xesam:album`, `xesam:artist`, `xesam:trackNumber`.
- `mpris:artUrl` **must be a `file://` path** — the Shell will not fetch an
  `https://` URL reliably. Apple serves artwork as a *template* URL containing
  `{w}x{h}bb.jpg`; substitute the size, fetch, and cache under
  `~/.cache/tonearm/artwork/` keyed by catalog id. That is what `artwork.rs` is
  for.
- Bidirectional: `PlayPause` / `Next` / `Previous` / `Seek` / `SetPosition` /
  `Volume` from the Shell must reach the sidecar, and every sidecar event must
  update the exported properties. Half-working MPRIS is the most common failure
  of the wrappers — do not ship it.
- Emit `Seeked` on discontinuous jumps, and keep `Position` honest between
  events with a 1s tick that is **removed when paused**.

## Milestones

Playback engine first. One vertical slice, one PR each.

- ✅ **M1 — Handshake.** Scaffold, sidecar, NDJSON round-trip, one-time Apple
  login, tokens harvested. Verified: Widevine → MusicKit → PipeWire, window
  never mapped.
- ✅ **M2 — Transport.** The Now Playing bar, sidecar-owned queue, supervision.
- ✅ **M3 — MPRIS.** `org.mpris.MediaPlayer2.Tonearm`, bidirectional, artwork as
  a `file://` URL. Verified over `busctl`: properties read, and `PlayPause` /
  `Next` from the bus reach the sidecar.
- **M4 — Queue view.** Native list, reorder, jump via `changeToMediaAtIndex`.
- ✅ **M5 — Library.** Saved songs in a native list, type-to-find search,
  click-to-play enqueuing the whole visible list. Verified against a real
  library: 539 tracks over 6 pages, 4 correctly detected as unplayable.
  Playlists and albums are still to come.
- **M6 — Catalog.** Search, album and artist pages.
- **M7 — Polish.** Preferences, shortcuts, About, icon, `.desktop`,
  `make install`, opt-in track-change notifications.

**Stay lean — flag the drift, don't gatekeep.** Not the default focus: lyrics,
Discord presence, podcasts, radio, multi-account, an equaliser, scrobbling,
cross-platform. Downloads and anything decrypting are not "later", they are
rule 1. When a change drifts, **name the cost and the direction** so it's a
conscious choice — then build it if it genuinely helps on this one machine.

## Commands

```bash
cargo run                                    # dev (expects ./sidecar/node_modules)
RUST_LOG=tonearm=debug cargo run             # traces the NDJSON protocol both ways
make sidecar                                 # npm install castLabs Electron (~200 MB)
make sidecar-run                             # sidecar alone, window VISIBLE — isolates DRM bugs
cargo clippy --all-targets -- -D warnings    # the bar, before any commit
make check                                   # fmt + clippy + test
```

System deps (CachyOS / Arch):

```bash
sudo pacman -S --needed base-devel pkgconf rust gtk4 libadwaita librsvg nodejs npm
# A keyring provider must be running for oo7 (gnome-keyring is default on GNOME).
# playerctl is handy for testing MPRIS.
```

Two gotchas around installing the sidecar, both of which fail confusingly:

- **`sidecar/.npmrc` is load-bearing — do not delete it.** npm 12 disables
  git-type dependencies by default, and castLabs Electron ships only as a GitHub
  release. Without `allow-git=root` you get `EALLOWGIT`. It must live in
  `sidecar/`, not the repo root: npm reads the project `.npmrc` from the
  directory holding the `package.json` it is installing.
- **`npm install` alone is not enough.** castLabs ships **no postinstall hook**
  — the ~200 MB Chromium is fetched by an explicit
  `node node_modules/electron/install.js`. Skip it and you get a 14 MB
  `node_modules` with no binary, and the failure only surfaces later as
  "Electron not installed". `make sidecar` runs both steps.

Debugging, in order — always isolate the layer first:

1. `make sidecar-run` — if a track won't play with the window visible, it's DRM
   or Apple, not Rust.
2. `RUST_LOG=tonearm=debug cargo run` — watch the NDJSON both ways.
3. `playerctl -p Tonearm metadata` / `busctl --user introspect
   org.mpris.MediaPlayer2.Tonearm /org/mpris/MediaPlayer2` — the MPRIS surface.
4. `pavucontrol` — confirm the stream exists and is named Tonearm.

## Conventions

- `cargo clippy --all-targets -- -D warnings` is the bar, not `cargo build`.
- Commits: conventional commits (`feat:`, `fix:`, `refactor:`, `chore:`).
- **Licence: GPL-3.0-or-later.** Full text in `COPYING`; declared in
  `Cargo.toml`. Every source file carries the two-line SPDX header
  (`SPDX-FileCopyrightText` + `SPDX-License-Identifier: GPL-3.0-or-later`).
- App ID: `dev.miguelrincon.Tonearm`. It must match the `.desktop` file name, the
  GResource prefix (`/dev/miguelrincon/Tonearm/`), `RelmApp::new()`, and the
  MPRIS bus name suffix. The app is called **Tonearm** in the window title and
  `.desktop` `Name=`.
- Versioning: SemVer in `Cargo.toml`; `main` carries a `-dev` pre-release.
- `sidecar/node_modules` is **never** committed — it is ~200 MB of Chromium,
  fetched by `make sidecar`.
