# CLAUDE.md

Project instructions for Claude Code. Read this fully before writing code.

## What this is

**Slipmat** — a small, native GNOME desktop app to play **Apple Music** on one
personal Linux laptop. Not a product, not multi-user, not cross-platform. One
user, one machine.

It is the sibling of **Dockyard** (a native GNOME Docker manager) and **Pitwall**
(a native GNOME GitHub Actions monitor), and shares their stack, architecture and
taste. The name is the felt disc between the platter and the record: the layer
that sits between the mechanism and the music, and lets the record turn on its
own terms.

The app should be indistinguishable from a first-party GNOME application. If a
design decision would make it look like an Electron app or a generic Qt tool, it
is the wrong decision.

**Why this project exists.** Every Apple Music option on Linux — Cider, Sidra,
the various wrappers — is `music.apple.com` in a costume. You get a web page's
scroll behaviour, a web page's search field, and none of GNOME's taste. Slipmat
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
| **castLabs Electron (`wvcus`)** | **Works.** *Fetches* the CDM — see below. What Sidra uses.      |

So a 100% native Apple Music player *cannot exist*. The honest ceiling — and the
whole design — is: **everything the user sees is native; the audio decoder is
invisible.**

**`wvcus` does not ship the CDM — it fetches it.** The suffix is *Widevine CDM
Update Service*: what castLabs adds is the plumbing that lets Chromium's own
component updater pull the CDM down, exactly as Chrome does for itself. There is
no Widevine binary anywhere in the 327 MB Electron dist; verify with

```bash
find sidecar/node_modules -iname "*widevine*"   # finds nothing
```

It lands at `~/.config/Slipmat/WidevineCdm/` on first run, beside the copies
Chrome and Chromium fetched for themselves.

This matters for **packaging**, and this entry used to say "bundles", which
pointed the wrong way. Nothing Slipmat would ship contains proprietary Google
code: Electron is MIT, our code is GPL-3, and the CDM arrives on the user's own
machine through their own component updater into their own config directory. So
redistribution is not the obstacle it looked like — a Flatpak or an AUR package
would carry Electron and nothing more, and the CDM download needs only network
access and a writable, persistent config directory.

**And that is now measured rather than reasoned.** Slipmat was run inside a real
`org.gnome.Platform//49` sandbox with `--nofilesystem=home`, a private `HOME`
carrying the Apple session but **no CDM**, and playback worked:

```text
widevine ready: {"status":"updated","version":"4.10.3050.0"}
…/.config/Slipmat/WidevineCdm/4.10.3050.0/_platform_specific/linux_x64/libwidevinecdm.so
```

So the component updater reaches out over `--share=network`, writes into the
app's own config directory, and the CDM decrypts — a track played. The original
plan called a sandboxed CDM "genuinely hard" and deferred Flatpak on the
strength of it; that was reasoning, and it was wrong. What is actually hard is
the *build*, not the runtime.

Three things that experiment settled, and that a manifest has to carry:

- **`org.electronjs.Electron2.BaseApp`, for zypak.** Chromium's SUID sandbox
  cannot work inside Flatpak's — without it the sidecar aborts on
  `chrome-sandbox is owned by root and has mode 4755` and rule 6 restarts it
  forever. `ELECTRON_DISABLE_SANDBOX=1` proves the rest works but is not the
  answer to ship.
- **`--device=dri`.** Without it GTK falls back to software rendering — `egl:
  failed to create dri2 screen` — and the grids scroll badly enough to read as
  an app problem rather than a packaging one.
- **`--own-name=dev.miguelrincon.Slipmat`** and the MPRIS name. Flatpak's bus
  proxy only lets an app own names matching its ID, and `GtkApplication` exits
  0 without a window if it cannot register.

One thing that experiment appeared to find was **not real**, and the way it
resolved is the lesson. `gdk_pixbuf` decoding failed there while everything
else worked — 650 covers fetched, **0** backdrops written, against 20 on the
host, on the same files. Tile artwork and the blurred backdrop both vanished,
and both call `Pixbuf::from_file_at_scale`; `Cover::set_file` kept working
because `GtkImage` goes through `GdkTexture`. It looked like a portability
finding worth designing around.

It was an artefact of the rig. Running a *host-built* binary against a foreign
runtime mixes two variables, and gdk-pixbuf is exactly the sort of thing that
differs between them. Built properly inside the SDK, against the runtime's own
libraries, every cover and every backdrop loads. **A host binary in a foreign
runtime is not a fair test of anything subtle** — it answers "does the sandbox
allow this" and nothing finer.

## Building the Flatpak

```bash
make flatpak          # build and install it locally
make flatpak-bundle   # a single .flatpak file to carry to another machine
```

Three things in the manifest are not obvious and were each found the hard way:

- **zypak wraps `electron`, not the app.** It has to be the *direct* parent of
  Chromium, and Chromium is the app's grandchild — the launcher starts Rust,
  Rust spawns Electron. Wrapping the launcher leaves the sidecar aborting on
  `chrome-sandbox … mode 4755` exactly as if zypak were absent. So a shim
  stands where `electron_binary()` looks and wraps the real binary beside it.
- **No npm tree.** The sidecar's only dependency is Electron itself, and the
  app runs `node_modules/electron/dist/electron` directly — the other thirteen
  packages exist only to *download* that binary. So the castLabs release is one
  pinned archive rather than a generated node-sources list.
- **The build is offline**, because `flatpak-builder` forbids network. Crates
  come from `cargo-sources.json`; regenerate it with
  `packaging/flatpak/generate-sources.sh` whenever `Cargo.lock` changes.

Two Linux facts that follow, and that you must not design around:

- **No VMP signing needed.** Linux Widevine reports `PLATFORM_UNVERIFIED`; the
  castLabs EVS account is a macOS/Windows concern. Nothing to sign.
- **No persistent licences.** Therefore **Slipmat requires a network connection
  to play, always. Offline and downloaded playback are impossible** — not a v1
  cut, a permanent property of the platform. Never add a "download" affordance.

## The axis that differs from Pitwall

Pitwall polls a **remote, rate-limited HTTP API**. Slipmat supervises a
**long-lived local child process** and mirrors its state. Almost every rule below
follows from that one difference:

| Concern       | Pitwall (remote GitHub)          | Slipmat (local sidecar)                        |
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
| Async runtime  | `tokio`                | 1 (`rt-multi-thread`, `process`, `io-util`)|
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

Slipmat plays through **Apple's own MusicKit player**, using **Google's official
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
  ("Apple Music changed; Slipmat needs an update"). Never degrade silently into
  a dead player with a spinning UI.
- Do not scrape the DOM. Only `MusicKit.getInstance()` and its documented events.
  DOM scraping is what makes wrappers break monthly.

### 5. No `.unwrap()` / `.expect()` outside `main.rs` and tests

The sidecar can die, the network can drop, Apple can 403 a stale token. Every
failure becomes an `adw::Toast` or a state, never a crash. `anyhow::Result`
internally.

### 6. The sidecar is supervised, not fired-and-forgotten

If the child exits, Slipmat restarts it with backoff, replays the queue and
position, and toasts once. A dead sidecar must never present as a healthy,
silent player. This is the local analogue of Pitwall's "a present-but-dead token
must not render an empty, healthy-looking list".

### 7. Tokens never touch disk, logs, or error strings

**No token is persisted at all** — not to a file, and not to the keyring either.

This rule originally said the Music User Token should go to the Secret Service
via `oo7`. It never did: `secret.rs` was never written, `oo7` was a dependency
nothing imported, and the README told people to install a keyring they did not
need. The rule is amended rather than the code, because what the code does is
*stronger*: both tokens are re-harvested from `MusicKit.getInstance()` on every
launch, the sidecar's own session cookie is what persists the login exactly as
it would in a browser, and there is nothing at rest for anyone to find.

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
│  Slipmat — Rust · relm4 · libadwaita             │  ← 100% of what the user sees
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

### Where the ids come from, and why it matters

Apple has **two** id spaces and they are not interchangeable — a library id
(`l.…` for albums, `r.…` for artists) 404s against `/catalog`, and a catalog id
404s against `/me/library`. Every `Album` and `Artist` therefore carries a
`library: bool` set **at the parse site** by whichever client method fetched it,
never sniffed from the id's shape later. `PageKind` has separate variants for
the two, so the compiler asks which one you have before a page can be opened.

The same distinction already existed one level down: a library *song*'s own id
is not playable, and its `playParams.catalogId` is. That is why `Track` carries
both.

### Writing to the library — verified against Apple's docs, 2026-07-26

All three exist, all take the **Music User Token** we already hold, and all
answer **202 Accepted with an empty body** — "acceptable, may not have
completed", so the UI must not treat 202 as "it is now in your library" and
re-read rather than assume.

| Want | Call |
| --- | --- |
| Add to library | `POST /v1/me/library?ids[songs]=…&ids[albums]=…` — several types in one request |
| Favourite (the star) | `POST /v1/me/favorites?ids[<type>]=…` — **add only**, see below |
| Love / dislike | `PUT /v1/me/ratings/{type}/{id}`, body `{"type":"rating","attributes":{"value": 1 \| -1}}` |

**Un-favouriting is not possible over REST with this token.** Apple documents no
counterpart to the add, and `DELETE /v1/me/favorites?ids[songs]=…` — the obvious
inverse — answers `400 Insufficient Permissions`:
`'Favorites:DELETE:IdsQuery' entities require permissions that are not in the
request`. So the row menu offers no removal.

Worth knowing before anyone concludes it is impossible: **`music.apple.com` can
un-favourite**, using the same session we borrow. So a route exists — just not
this one. The likely candidate is MusicKit JS inside the sidecar, the way
playback already works, rather than a REST call from Rust. Untried, and it would
be the first time the sidecar is used for anything but audio, which is a real
cost to weigh.

**Favourites and ratings are different things.** `favorites` is the modern star;
`ratings` is the older love/dislike pair, whose only legal values are `1` and
`-1`, with *absent* meaning unrated — so removing a rating is `DELETE`, not a
value of `0`. Ratings come in catalog and library flavours per type (`songs`,
`albums`, `playlists`, `music-videos`, `stations`), which is the same
two-id-spaces trap as everything else here.

Add-to-library and favourites are wired to the row context menu. Ratings are
not — they are a third mechanism nobody has asked for yet.

**Nothing shows state.** A 202 means accepted, so a star drawn from it would be
a star that might be lying; showing it truthfully means reading it back per
track, which is a request per row. The menu therefore *acts* and toasts, and
says "Sent to your library" rather than "Added".

### Library song attributes, and what you have to ask for

`LibrarySongs.Attributes` is a **documented, closed list**, and two things about
it matter:

- **`dateAdded` cannot be had per song.** It is not in the dictionary, and
  `extend=dateAdded` does not produce it either — **measured as 0 of 541**
  against a real library. There is therefore no "Recently Added" *sort*:
  offering one that silently orders by title is worse than not offering it.

  There *is* `GET /v1/me/library/recently-added`, which returns a
  `ResourceCollectionResponse` — a mixed list of **albums and playlists** in
  recently-added order, the same thing Apple Music's own "Recently Added"
  screen shows. That is a plausible future *view*, not a sort of the songs
  list: it never promises individual songs, and it documents no `limit` or
  `offset`. Untried.
- **`inFavorites` *is* on it**, with `&extend=inFavorites` — **41 of 541**.
  Whether a track is starred comes back with the library, so a row shows it
  without a read-back or a request per row. This is the exception to "202 means
  you cannot know the state": for favourites on library songs, you can.

Both numbers came from a counter left in `all_library_songs`, which still logs
`starred` on every load. Two rounds were lost to guessing which attributes Apple
honours before anyone measured; the counter stays so the next question is
settled the same way.

Library membership is the same shape of fact: a track that came from
`/me/library/…` is in the library by definition, so `Track::in_library` is set
by the client method that fetched it — never guessed from the id.

**Apple sends artwork for its own playlists and not for yours.** Measured
against a real library, and the split is the whole finding rather than the
count:

| `with_art` | `without_art` |
| --- | --- |
| Edgerunners Soundtrack | Game on |
| Favorite Songs | My Shazam Tracks |
| Forza Horizon 4 Soundtrack | Red Soundtrack |
| Vos Veis | Shazams |

Curated and Apple-generated playlists carry an `artwork` object; **every
user-created one carries none**, on both endpoints — the list *and*
`/me/library/playlists/{id}`. So the 2×2 mosaic of four covers that
`music.apple.com` shows for a playlist you never gave a picture to is drawn by
the web player, not fetched. `include=catalog` cannot rescue it either, the way
it rescues a library artist's portrait: a playlist you made has no catalog twin
to borrow from.

`all_library_playlists` logs both lists on every load, **named rather than
counted**, because a bare count answered the wrong question — it described the
list endpoint while the bug was on the details endpoint, and the two are
different requests that can carry different attributes.

Two traps here, both of which cost a round:

- **A screenshot in `docs/screenshots/playlist.webp` shows Slipmat rendering
  that mosaic for "Game on" in 0.2.0.** So Apple returned it at least once and
  no longer does, on identical code — `git diff v0.2.0..` over `client.rs`,
  `types.rs` and `pages.rs` is the rename and nothing else. Treat playlist
  artwork as *inconsistent between accounts and over time*, and never conclude
  from one absence that a thing was never there.
- **Three separate silent exits** hid this: `fetch` and `decode` were `.ok()`
  and `and_then`, and `fetch_page_art` returned early on `None` artwork saying
  nothing. A page holding its placeholder therefore looked identical whether
  the fetch failed, the file was undecodable, or there was never a URL at all.
  All three now warn. **Cosmetic is not the same as unexplained.**

Drawing the mosaic ourselves means a compositor: take the first four tracks'
covers, tile them, cache under the playlist's own key. On a detail page the
tracks are already loaded and it costs nothing extra; in the grid it costs one
request per artless playlist, because a tile knows no tracks.

### Restoring the last session

Three files, three homes, and the distinction is not pedantry:

| What | Where | Why |
| --- | --- | --- |
| Preferences | `~/.config/slipmat/settings.ini` | somebody chose them |
| Unplayable ids | `~/.cache/slipmat/` | rederivable — Apple will tell us again |
| The last queue | `$XDG_STATE_HOME/slipmat/session.json` | neither chosen nor rederivable |

Two rules the restore follows:

- **It never resumes playing.** `Command::SetQueue` carries `start_playing`, and
  this is the only place that sends `false`. An app that starts making noise
  because it was launched is hostile; the point is to remove the work of finding
  your place, not to take the decision away.
- **It saves on every track change as well as on shutdown.** Shutdown is the
  only moment the position is accurate, and also the one that might not run —
  a crash, a `SIGKILL`, a session ending badly. Saving per track means the worst
  case is restoring the right track at its start rather than restoring nothing.

**The position rides in the queue descriptor, not a seek afterwards.**
`setQueue` accepts `startTime`, and it has to be used: a seek needs a *current
item* to seek within, and a queue loaded with `startPlaying: false` does not
have one yet. The first attempt sent a seek once the queue arrived and it did
nothing at all.

**A loaded-but-never-started queue has no now-playing item either**, for the
same reason. Rendering that faithfully left the Now Playing bar empty beside a
full queue, so `showing()` falls back to the queue's own current entry — which
is the honest answer to "what is this player on".

**Clearing the queue is not one documented call.** `clearQueue`,
`queue.splice` and `setQueue({songs: []})` are all plausible depending on the
MusicKit build, so the sidecar tries them in that order and throws if items
remain — and stops playback first, because an empty queue with a track still
playing is a state nothing else expects. Clearing also drops the saved session:
there is nothing to come back to.

**And `changeToMediaAtIndex` only moves the cursor.** On a queue that is loaded
but idle it moves silently, so clicking a track in the queue looked like nothing
happened. Clicking a track is a request to *play* it, so a jump now starts
playback if it is not already running.

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
- **The sidecar must not look like a second app.** `app.setName('Slipmat')` plus
  `app.setDesktopName('dev.miguelrincon.Slipmat.desktop')`, or the shell invents
  a "slipmat-sidecar" entry with a generic icon.
- **The sidecar must not publish its own MPRIS player.** Chromium registers one
  the moment a page plays media, and grabs the hardware media keys with it —
  giving two identical "Slipmat" entries in the shell and letting an invisible
  browser win the race for Play/Pause. Disabled via
  `--disable-features=MediaSessionService,HardwareMediaKeyHandling`. Neither
  affects decoding. Slipmat owns MPRIS; the sidecar owns audio.
- **Diagnose by layer, in order:** `cmd-queued` → never dispatched.
  No `cmd-recv` → renderer never ran it. `cmd-recv` but no `cmd-done` → the
  command is hanging. `cmd-done` with a full queue but a non-playing state →
  playback is blocked, not failing.
- **`unknown-command` means you are running the *installed* sidecar, not the
  repo's.** `locate()` prefers `$XDG_DATA_HOME` over the build tree, so once
  anything has been installed to `~/.local/share/slipmat/sidecar`, a debug build
  runs **fresh Rust against stale JavaScript** — and says nothing about it.

  It fails in the most misleading way available: the command goes out, the
  optimistic UI updates, a toast says it worked, and only the account it was
  supposed to change disagrees. An afternoon went into "the removal does not
  work" before the log line `code=unknown-command detail=removeFromLibrary`
  was read for what it was.

  Any change under `sidecar/` needs one of:

  ```bash
  SLIPMAT_SIDECAR=$PWD/sidecar cargo run   # what the repo has, always
  make install-sidecar                     # or refresh the installed copy
  ```

  The first is the better habit while developing. Check it in one line:
  `grep -c yourNewCommand ~/.local/share/slipmat/sidecar/preload.js`.
- **A second instance never announces itself — it just wins.** relm4 apps are
  unique by default, so launching while one is already up hands off to the
  primary and exits **0 with no output**: no window, no log, nothing to
  suggest what happened. Every later run then exercises the *old* binary.

  Scanning `/proc` for the executable is not reliable — an instance started as
  `./target/debug/slipmat` whose binary has since been replaced by a rebuild no
  longer matches. Ask the bus, which is never wrong about who holds the name:

  ```bash
  busctl --user call org.freedesktop.DBus /org/freedesktop/DBus \
    org.freedesktop.DBus GetConnectionUnixProcessID s dev.miguelrincon.Slipmat
  ```

  Both of these are the same trap wearing two costumes: **old code quietly in
  charge while you test new code.** When a change appears to have no effect,
  rule this out before doubting the change.

```
src/
  main.rs            # RelmApp bootstrap, tracing, icon; locate + spawn the sidecar
  app/
    mod.rs           # root Component: AppModel, AppMsg, CommandMsg, update, view!
    view.rs          # which section is showing; the three encodings of that
    library.rs       # loading + filtering songs, albums, artists, catalog search
    queue.rs         # click → setQueue, and healing a queue MusicKit rejected
    pages.rs         # the album / artist navigation stack
    playback.rs      # mirrored state → Now Playing bar, MPRIS, notifications
    supervise.rs     # keeping the sidecar alive; folding in its events
    status.rs        # what the pane shows when it is not showing music
    chrome.rs        # the menu, its accelerators, and the three dialogs
    row_menu.rs      # the per-row context menu, and what each item sends
    writes.rs        # library writes: the optimistic row change, and taking it back
  settings.rs        # glib::KeyFile → ~/.config/slipmat/settings.ini. NEVER tokens.
  session.rs         # what was playing, → $XDG_STATE_HOME/slipmat/session.json
  style.rs           # accent colour + the cover behind the player. The only CSS.
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
    player_view/
      mod.rs         # the bar opened out into a drawer: model, view!, reducer
      transport.rs   # its scrubber and buttons, built by hand because they
                     # *move* between layouts — a child module so construction
                     # and refresh cannot land in different files
    track_row.rs     # FactoryComponent → adw::ActionRow
    queue_view.rs    # the queue, reorderable
    library.rs       # playlists / albums / songs
    cover.rs         # square record or round portrait; one of the two shows
    artwork.rs       # fetch + disk cache; MPRIS needs a file:// path, and
                     # the player's backdrop needs a deliberately tiny one
sidecar/
  package.json  main.js  preload.js    # ~200 lines of JS, total
data/
  dev.miguelrincon.Slipmat.desktop
  icons/hicolor/{scalable,symbolic}/apps/dev.miguelrincon.Slipmat{,-symbolic}.svg
                       # the PNG sizes are rendered from the SVG by `make install`,
                       # never committed — two copies of an icon always drift
Makefile             # make install → ~/.local (no sudo); make sidecar; make check
```

`app/` is split by **what a thing does**, not by layer, so a change usually
lands in one file. **A new `impl AppModel` method goes in the sibling
that owns its concern** — and if none does, that is a new sibling, not another
method in `mod.rs`. This drifts if it is not watched: `mod.rs` was split once at
3085 lines and had grown back past 2800 before anyone looked, almost all of it
helper methods added where the model happened to be. Every sibling is an `impl AppModel` block — legal, and able
to touch private fields, because a child module can see its parent's. `mod.rs`
keeps only the three things that have to be in one place: the model, the
messages, and the `Component` impl holding `view!` and the reducer. The reducer
stays whole because it is the map from action to work, and a map is only useful
entire.

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
    mpris: Option<mpris_server::Server<SlipmatPlayer>>,
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
  artwork, title + artist, a clock, prev / play-pause / next, and volume, with
  the track's progress as a thin line across the top of the bar. That line is
  **information, not a control** — a scale needs a grabbable handle and room to
  aim in, and the bar is what runs out of room first when the window is tiled
  narrow. Scrubbing lives in the drawer, which has the width for it. The bar is
  visible on every page — it is the app.
- Main content: `adw::NavigationView` over an `adw::ViewStack` (clamped) —
  **Library** (playlists / albums / songs) and **Search**. The queue is an
  `adw::Dialog` or a sidebar sheet from the bottom bar, not a page.
- Rows are `adw::ActionRow` with artwork as prefix; activating a row sends
  `PlayTrack` with the **whole containing list** as the queue and the row's index
  as `start` — never a single-track queue (rule 3).
- **Lists are `gtk::ListView` via relm4's `TypedListView`, never
  `gtk::ListBox`.** A `ListBox` builds one real widget per row, so a
  541-track library plus a 530-track queue meant over a thousand live
  `AdwActionRow`s and the app felt heavy. `ListView` recycles — it keeps about
  as many widgets as fit on screen. The consequence: `RelmListItem::bind` must
  set **every** property it cares about, because the widget it is handed was
  showing a different track a moment ago, and anything left unset keeps the old
  value. Disconnect signal handlers in `unbind` or they stack up on reuse.
- **A `#[watch]` on a control that also reports user changes is a two-way
  binding, and it will cycle unless you break it.** This one froze a desktop
  hard enough to need a forced power-off, so it is worth understanding rather
  than pattern-matching.

  GTK emits `value-changed` for a programmatic `set_value` exactly as it does
  for a drag — a widget cannot tell you which it was. And relm4 runs
  `update_view` after **every** message, including the one the widget just
  sent. So the sequence is: user moves the slider → the handler fires → the
  very next view update writes the *old* model value back into the widget →
  GTK emits for that → the two values ping-pong through the reducer forever.
  Each lap was one sidecar command, one MPRIS property change on the bus and
  one journal line; it managed 5,721 of them.

  Two halves fix it, and **both** are needed. Adopt the value into the model
  in the handler, so the following view update writes back what the widget
  already holds; and ignore an incoming value equal to the one held, which is
  what a programmatic set arrives as. `now_playing::volume_is_new` is the
  guard, with the reasoning next to it.

  Note that "it only emits when the value actually changed" is **true and not
  sufficient** — that was the reasoning that shipped the bug. The stale write
  *is* a change.

  Neither clippy nor the test suite can see this: the cycle only exists once
  GTK, `update_view` and the reducer are wired together. Verify a two-way
  control by counting what reaches the sidecar —

  ```bash
  RUST_LOG=slipmat=info cargo run 2>&1 | grep -c "dispatch setVolume"
  ```

  One command per change, then silence. Anything that keeps climbing is this
  bug. Log to a **file, not the journal** while testing, or a regression takes
  journald down with it.
- **A property that drives an `AdwAnimation` is written on an edge, never on a
  level.** This is the trap next door to the one above, and the one above is
  what makes it easy to get wrong: that rule is about a *feedback loop*, and
  this has no loop at all.

  `#[watch]` re-asserts its value after **every** message, and during playback
  those never stop — a `refreshTokens` a second, a position tick twice a
  second. Free for a label. For an animated property it is a request to
  start-or-re-aim an animation, forever, at whatever rate messages happen to
  arrive. It wedged the app at 100% CPU inside
  `adw_spring_animation_set_value_to`, reached from `update_view` while
  handling a routine token refresh.

  Three properties are governed by this, and all three are written only from
  `sync_animated`: the sidebar's `show-sidebar`, the drawer's `open`, the bar's
  `reveal-bottom-bar`. Two details make it actually hold:

  - Compared against **what was last written**, not against the widget. A
    widget and a model that disagree persistently disagree on every message
    too, so asking the widget is the level trigger again wearing a guard's
    clothes.
  - Synced on **both** update paths. Sidecar events arrive through
    `update_cmd_with_view`, not `update_with_view`, and one of them is what
    moves `stage` to `Ready` and reveals the Now Playing bar. Syncing on one
    path left the bar hidden for a whole session.

  **Diagnosing a wedge, and the one measurement that splits it:** a GTK layout
  loop logs *nothing*, because no message is being processed. A runaway reducer
  (#37) logs a dispatch per lap. One symptom, opposite causes, and the log
  tells them apart before you touch anything else.

  `ptrace_scope` is 1 on this machine, so a debugger cannot attach to a process
  it did not start. Make the kernel write the core instead:

  ```bash
  kill -ABRT $(busctl --user call org.freedesktop.DBus /org/freedesktop/DBus \
    org.freedesktop.DBus GetConnectionUnixProcessID s dev.miguelrincon.Slipmat \
    | awk '{print $2}')
  coredumpctl debug --debugger=gdb --debugger-arguments="-batch -ex 'bt 30'"
  ```

  Both diagnoses of this bug came from that, and neither from reasoning.
- **The app stylesheet is `style.rs`, and it is the sanctioned CSS.** An accent
  colour is not a widget: libadwaita 1.6 exposes it only as CSS variables
  (`--accent-bg-color` and friends), so a `CssProvider` is the only route.
  Two providers, kept apart — a **base** one replaced when the accent
  preference changes, and a **backdrop** one carrying the cover behind the
  player, replaced on every track — so a new cover does not reparse the accent
  rules. Anything else wanting CSS still has to argue for itself first.
- **The player's backdrop is the cover, and the blur is the upscale.**
  `artwork::backdrop` writes a 48px copy beside the cover and CSS stretches it
  across the surface. GTK interpolates when it scales a texture, so it arrives
  soft on its own: no blur pass, no shader, nothing per-frame. A real Gaussian
  would be a CPU convolution per track change for a result nobody could tell
  apart once it is behind a scrim.

  The scrim is `@window_bg_color`, **not black**. Every label and icon on both
  surfaces has a colour chosen for contrast against the theme, so a photograph
  behind them would be guessing — and a dark veil would only guess right in one
  theme. The bar and the drawer share one rule and differ by two numbers: the
  bar's veil is lighter, because a thin strip shows less of the record, and its
  image is sized `cover` rather than 150%, because a square already overflows a
  wide strip's height and the drift has room without asking for more.

  Cross-faded between tracks with CSS `cross-fade()`, rewritten per frame — a
  provider that is *replaced* is not a state change GTK will transition
  between, so the interpolation has to be ours. Same clock as everything else
  that changes with the cover; two readings of one sleeve must not finish apart.

  This replaced a **tonal scrim**, a flat wash of the sleeve's dominant colour.
  The colour analysis went with it — `artwork::tint`, `dominant`, `hsl`, `rgb`
  — and is in the history if the neutral scrim ever wants tinting.
- **Use libadwaita widgets, not raw GTK.** `adw::ActionRow`,
  `adw::PreferencesGroup`, `adw::AboutDialog`, `adw::StatusPage`,
  `adw::ToastOverlay`. That's where the native feel comes from. No custom CSS
  unless there is no libadwaita widget for the job — say why before adding any.
- **First run**: a **modal that cannot be dismissed**, wearing the app's own
  icon, saying what Slipmat is, that it needs an active subscription, and —
  before the button, not after — that Apple's own sign-in page opens in a
  separate window.

  Blocking is the correct behaviour, not a nicety. Signed out, every control in
  the app is a control that cannot work: the sidebar sections fire library
  loads, the search box queries a catalog that answers 403, and the transport
  talks to a player with no session. Leaving them reachable produced a 403 per
  second against Apple for as long as the window was open, because the sidecar
  pushes `refreshTokens` every second and each one re-ran the auto-load.

  Raised and lowered from **one** place (`sync_onboarding`, called after every
  message), because four different paths change `stage` and three of them would
  have been easy to forget. A browser
  window appearing out of a native app is alarming when it is a surprise, and
  this is the one moment the web engine cannot be hidden, so it is explained
  instead. After it succeeds the sidecar hides forever. Never re-show it except
  on explicit Sign Out → Sign In.
- **Sign Out** lives in the primary menu, in its own section — an account action
  is not app furniture and should not sit beside About. It asks first, because
  it drops Apple's session and getting back in means the login window and
  whatever two-factor prompt that involves. It then forgets *everything* that
  belonged to that account: library, grids, catalog results, pushed pages,
  queue, the bar. The unplayable-id cache stays, because that is a fact about
  Apple's catalog rather than about the user.
- Sidecar restarting, no subscription, offline, no results: distinct
  `adw::StatusPage`s. Errors: `adw::Toast`.

## MPRIS (the v1 bar)

`org.mpris.MediaPlayer2.Slipmat`, via `mpris-server` 0.10.

- Metadata: `mpris:trackid`, `mpris:length`, `mpris:artUrl`, `xesam:title`,
  `xesam:album`, `xesam:artist`, `xesam:trackNumber`.
- `mpris:artUrl` **must be a `file://` path** — the Shell will not fetch an
  `https://` URL reliably. Apple serves artwork as a *template* URL containing
  `{w}x{h}bb.jpg`; substitute the size, fetch, and cache under
  `~/.cache/slipmat/artwork/` keyed by catalog id. That is what `artwork.rs` is
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
  **Gapless verified 2026-07-26** — see below; the architecture's central claim
  is no longer taken on trust.
- ✅ **M3 — MPRIS.** `org.mpris.MediaPlayer2.Slipmat`, bidirectional, artwork as
  a `file://` URL. Verified over `busctl`: properties read, and `PlayPause` /
  `Next` from the bus reach the sidecar.
- ✅ **M4 — Queue view.** MusicKit's queue as an `adw::OverlaySplitView`
  sidebar: jump via `changeToMediaAtIndex`, remove in place. Drag-reorder is
  deliberately not shipped — `queue.splice` is undocumented and the risk is to
  the gapless buffer.
- ✅ **M5 — Library.** Saved songs in a native list, type-to-find search,
  click-to-play enqueuing the whole visible list. Verified against a real
  library: 539 tracks over 6 pages, 4 correctly detected as unplayable.
  Playlists and albums are still to come.
- ✅ **M6 — Catalog.** Search across the whole Apple Music catalog from the
  sidebar, paginated as you scroll. Results mix artists and albums above the
  songs; either pushes a detail page (`adw::NavigationView`) you can play from
  and drill through — artist → album → track.
**Released so far:** v0.1.0 and v0.1.1 on 2026-07-26 — M1–M9, gapless
verified, and 0.1.1 exists because 0.1.0 could not be packaged at all.
v0.2.0 on 2026-07-28: sorting, the player drawer, the artwork backdrop, the
Flatpak and CI. **v0.3.0 is the rename to Slipmat**, and it carries a minor
bump rather than a patch because nothing about it is a drop-in update — the
app ID, the binary, the bus name and all three XDG directories move, so no
channel will offer it as an upgrade over 0.2.0.

- ✅ **M9 — Playlists.** Your library's playlists as a fourth sidebar section,
  opening onto the same detail page. Their tracks come from the relationship
  endpoint through the ordinary paginator rather than `include=tracks`, which
  caps at 100 — playlists routinely run longer, and silently showing the first
  100 of 400 is the kind of wrong answer that looks right.
- ✅ **M8 — Library browsing.** Albums and Artists as `gtk::GridView` tiles in
  the sidebar, alongside Songs. Covers are fetched lazily as tiles bind and
  cached to disk; clicking one pushes the same detail page the catalog uses,
  pointed at `/me/library` instead of `/catalog`. Library artists carry no
  artwork of their own, so the portrait is pulled from the catalog twin with
  `include=catalog` — free, on the request we were already making.
- ✅ **M7 — Polish.** Preferences (theme, notifications), keyboard shortcuts,
  About, the app icon, `.desktop`, `make install`, and opt-in track-change
  notifications.

**Stay lean — flag the drift, don't gatekeep.** Not the default focus: lyrics,
Discord presence, podcasts, radio, multi-account, an equaliser, scrobbling,
cross-platform. Downloads and anything decrypting are not "later", they are
rule 1. When a change drifts, **name the cost and the direction** so it's a
conscious choice — then build it if it genuinely helps on this one machine.

### Lists, and where they live

There are now three kinds of list, and they share their state deliberately:

| List | Registry | Model |
| --- | --- | --- |
| Results (library or catalog) | `AppModel::library_icons` | `TypedListView`, **virtualised** |
| Queue sidebar | `QueueView`'s own | `TypedListView`, virtualised |
| Album / artist page | the page's own | `TypedListView`, **not** virtualised |
| Albums grid | `AppModel::album_art_widgets` | `TypedGridView`, virtualised |
| Artists grid | `AppModel::artist_art_widgets` | `TypedGridView`, virtualised |
| Playlists grid | `AppModel::playlist_art_widgets` | `TypedGridView`, virtualised |

`CurrentTrack` and `DeadTracks` are shared by all of them; the widget registry
is **per list**. It is keyed by catalog id, so one shared registry would have
the same song on a page and in the results behind it overwrite each other's
entry — the marker would land on whichever bound last. `set_row_playing` asks
every registry instead.

The grids' registries hold `gtk::Image`s rather than row widgets, and they are
keyed by the **artwork's** cache key rather than by a catalog id — a tile that
requested a cover has very likely been recycled onto a different album by the
time the file lands, so the arrival is delivered to whichever tile is showing
that artwork *now*, or to none. The disk cache (`AppModel::tile_art`) is shared
across both grids, because it is keyed by the image itself.

**Whether a view is actually virtualised is measurable, not a matter of
opinion.** `RelmListItem::setup` runs once per *widget*, not once per item, so
`components::count_widget` counts them and logs at `trace`:

```bash
RUST_LOG=slipmat=trace cargo run 2>&1 | grep "list widget built"
```

Scroll a 500-track library and watch where the count stops. A few dozen means
recycling. Five hundred means every row is real, and something upstream — a
`Clamp` in the wrong place, a `Box` between the view and its `ScrolledWindow` —
is asking the view for its full height.

**`AdwClampScrollable`, not `AdwClamp`, around a scrollable child.** A plain
clamp does not implement `GtkScrollable`, so putting one between a
`ScrolledWindow` and a `GtkListView` breaks the list's height allocation and it
stops materialising rows part way down. Moving the clamp *outside* the scroller
fixes that and costs something else: the clamp is then what the window sizes,
so the scroller is only as wide as the clamp and its scrollbar sits in the
middle of the window instead of at the edge. `AdwClampScrollable` passes the
scrollable interface through to its child and settles both — verified the only
way that matters, by counting widgets: 205 for 534 tracks, unchanged.

A `GtkScrolledWindow` that spans the window then paints the `view` background
across it, a shade darker than the window. That was invisible while it was
clamped and centred, because it read as the list's own surface.
`.plain-scroller` in `style.rs` clears it.

**And a `bottom_bar` is drawn over the content, not beside it.** The Now
Playing bar is `AdwBottomSheet`'s, so the last row of every scrollable sat
behind it — *reachable*, since the scroller had already run to its end, and
invisible, which is the worst pair because nothing suggests there is more to
see. Maximising appeared to fix it and sent one diagnosis chasing a ten-pixel
measurement discrepancy that was real and irrelevant. `bottom-bar-height`
notifies, so the content's bottom margin follows the bar rather than guessing
at a height that changes with theme and text scale. It is set once, on the
sheet's content, which is every scrollable in the app at one stroke — the queue
is the only one outside it, and it lives in the drawer, above the bar.

**A rebuild is expensive, so do not do one that changes nothing.** Every tile
that binds decodes its cover on the GTK thread — measured at **2.5ms per
cover** — and a rebuild binds far more of them than are visible. Switching
sections used to rebuild unconditionally, so returning to a section already on
screen cost ~500ms of re-decoding every time.

Each section now records the fingerprint its widgets were built for (the query,
plus sort and direction for the songs list) and returns early when it already
matches. Anything that changes the underlying data sets the fingerprint to
`None`. The first build of a section still costs its ~500ms, once, behind the
spinner that is already showing.

`RUST_LOG=slipmat=debug` reports `rebuild what=… ms=…` and `section switch`, so
this stays measurable rather than remembered.

A pushed page is not virtualised on purpose: its list sits in a `Box` under the
header so the header scrolls with the content, which means GTK asks the list for
its full height. An album is a dozen tracks. That is the right trade.

## Known issues

- **Removing a queue track scrolls the list to the top** (#6). Four approaches
  tried and ruled out; the issue records them so they are not retried. Playing
  and jumping are unaffected, and the library list no longer does it.
- **Nothing stands between a runaway reducer and the session** (#37). The
  sidecar journals every dispatch, commands are not coalesced, and there is no
  rate ceiling — so a message loop costs a disk write and a bus round trip per
  lap. That is why the binding bug above reached the compositor instead of
  staying an app bug. Rule 6 says a dead sidecar must not look healthy; this is
  the same argument reversed.

## Commands

```bash
cargo run                                    # dev (expects ./sidecar/node_modules)
RUST_LOG=slipmat=debug cargo run             # traces the NDJSON protocol both ways
make sidecar                                 # npm install castLabs Electron (~200 MB)
                                             # — `make install` already does this
make sidecar-run                             # sidecar alone, window VISIBLE — isolates DRM bugs
make gapless                                 # watch the audio stream across a track boundary
cargo clippy --all-targets -- -D warnings    # the bar, before any commit
make check                                   # sizes + fmt + clippy + test
make sizes                                   # the size budget alone, instant
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

### Gapless — verified 2026-07-26

**It works.** Measured on *A Thousand Suns (Deluxe)*, four consecutive
boundaries:

- Every transition logged `prompted_by="nothing — MusicKit advanced itself"`.
  Rust sent nothing at any boundary; MusicKit advanced a queue it already held,
  which is exactly what rule 3 exists to guarantee.
- Wall-clock between transitions matched each track's length to the second
  (57s / 254s / 18s), so every track ran out rather than being cut short.
- PipeWire sink-input **#26158 was created once, at the first play, and was
  still alive after all four boundaries.** The decoder never stopped. That is
  the half you cannot hear and cannot infer.
- The listener heard no gap.

Two things that run cost, both now fixed, both worth not re-learning:

- **The two logs are on different clocks.** `tracing` prints UTC, the script
  printed local time. A `remove` two hours adrift looked like a gap at a
  boundary and was in fact a screenshot shutter. The script now prints the
  offset, and only reports Slipmat's *own* stream — any notification sound
  opens and closes a stream of its own, and reporting those made the whole
  instrument untrustworthy.
- **`left_ms` was sampled at the wrong moment.** Read live when
  `nowPlayingItemDidChange` arrives, MusicKit has usually already zeroed the
  position — but not always, so it read as the full duration on three
  boundaries and zero on the fourth, purely on which event won the race. It is
  a high-water mark now.

### Verifying gapless

The headline feature, and the one that fails silently. It has two halves, and
they fail for different reasons:

| Half | How it breaks | How to see it |
| --- | --- | --- |
| Rust must not drive the queue | feeding tracks one at a time (rule 3) | the `track transition` log line |
| the decoder must not stop | MusicKit re-buffering at the boundary | the PipeWire stream disappearing |

```bash
make gapless                                  # terminal 1: watch the audio stream
RUST_LOG=slipmat=info cargo run               # terminal 2: watch the transitions
```

Queue an album with a **true** gapless transition — a live record, a DJ mix,
something segued — and let it run across the boundary. Then check all three:

1. **The log.** Every boundary prints a `track transition` line. A natural one
   must read `prompted_by="nothing — MusicKit advanced itself"` with `left_ms`
   near zero. If `prompted_by` names a command on a track that ran to its end,
   Rust is driving the queue and rule 3 is broken.
2. **The stream.** `make gapless` should print *nothing* at the boundary. A
   `remove` followed by a `new` is a torn-down decoder, which is audible no
   matter what the log says.
3. **Your ears.** Neither of the above can hear a 20 ms hiccup. They tell you
   *where* a gap came from, not whether there was one.

`left_ms` is also how you tell a boundary from a skip: a skip leaves seconds on
the clock, a boundary leaves almost none.

Debugging, in order — always isolate the layer first:

0. **Is the code you changed the code that is running?** Two ways it is not,
   both silent, both in the sidecar rules above: an *installed* sidecar
   shadowing the repo's, and a *second instance* handing off to one already
   running. Rule these out before doubting the change — they cost more time
   than anything else on this list.
1. `make sidecar-run` — if a track won't play with the window visible, it's DRM
   or Apple, not Rust. (Uses `SLIPMAT_SHOW_SIDECAR=1`; `--debug` never reached
   the app, Electron eats it as Node's deprecated flag.)
2. `RUST_LOG=slipmat=debug cargo run` — watch the NDJSON both ways.
3. `playerctl -p Slipmat metadata` / `busctl --user introspect
   org.mpris.MediaPlayer2.Slipmat /org/mpris/MediaPlayer2` — the MPRIS surface.
4. `pavucontrol` — confirm the stream exists and is named Slipmat.

## Conventions

- `cargo clippy --all-targets -- -D warnings` is the bar, not `cargo build`.
- **600 lines is the size budget, and it is a ratchet.** Anything over must be
  listed in `scripts/size-exceptions.txt` with the size it may reach and why; a
  listed file that grows past its number fails `make check`.

  Recording an exception is a legitimate answer — `view!` and a reducer are long
  by nature, and `protocol.rs` is deliberately one file. Doing it silently is
  not, which is the whole point: the placement rule in the architecture section
  was already written down, and `mod.rs` still grew from a post-split 1500 lines
  back past 2800 before anyone looked. This surfaces it in the diff that causes
  it rather than months later.
- Commits: conventional commits (`feat:`, `fix:`, `refactor:`, `chore:`).
- **Licence: GPL-3.0-or-later.** Full text in `COPYING`; declared in
  `Cargo.toml`. Every source file carries the two-line SPDX header
  (`SPDX-FileCopyrightText` + `SPDX-License-Identifier: GPL-3.0-or-later`).
- App ID: `dev.miguelrincon.Slipmat`. It must match the `.desktop` file name, the
  GResource prefix (`/dev/miguelrincon/Slipmat/`), `RelmApp::new()`, and the
  MPRIS bus name suffix. The app is called **Slipmat** in the window title and
  `.desktop` `Name=`.
- Versioning: SemVer in `Cargo.toml`; `main` carries a `-dev` pre-release.
  Releasing is: bump to the release version, tag `vX.Y.Z` on the merge commit,
  then set `main` back to the next `-dev`. There is no CHANGELOG — the release
  notes are written from the merged PRs, which is where the reasoning already
  is.
- `sidecar/node_modules` is **never** committed — it is ~200 MB of Chromium,
  fetched by `make sidecar`.

### The README is not a feature list

`## What it does` says **six** things, and that is the whole budget:

1. a native GNOME app, with a small hidden web layer
2. the library — songs, albums, artists, playlists
3. a player worth the name, gapless
4. controls from the shell, the lock screen and the media keys, and playback
   that survives closing the window
5. all of Apple Music, searchable
6. quick, and out of the way

It grew to eight dense paragraphs once — MPRIS, the queue view, and "the GNOME
furniture" (preferences, shortcuts, About) each had their own — and reading it
felt like an inventory rather than a reason to install anything. Preferences are
table stakes; nobody chooses a music player for its About dialog.

**A new feature earns a line only if it changes what the app is for.** Not "it
is new", not "it was hard" — the test is whether somebody deciding between this
and the alternatives would want to know. Most things belong in the release notes
instead, which are written from the merged PRs and where the reasoning already
is. When something does earn a line, take one out or fold it in; six is a
ceiling, not a starting point.

Say it the way a user would. **Never "MPRIS"** — that is a bus name, not a
feature. It is "play, pause and skip from the GNOME top bar, the lock screen, or
your media keys", because that is the thing they can picture.
