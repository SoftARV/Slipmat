<!--
SPDX-FileCopyrightText: 2026 Miguel Rincon
SPDX-License-Identifier: GPL-3.0-or-later
-->

<p align="center">
  <img src="docs/screenshots/icon.png" width="128" alt="Slipmat icon">
</p>

# Slipmat

A native GNOME client for Apple Music.

Every other Apple Music option on Linux is `music.apple.com` in a costume —
Electron wrapped around the website, with the website's scroll behaviour and the
website's search field. Slipmat is the first one where the interface is actually
native: GTK4 and libadwaita, written in Rust, with real libadwaita lists, real
GNOME search, and MPRIS that works in both directions.

The web engine is still there. You just never see it.

![Slipmat showing a library of songs, each starred as a favourite, with a menu
button on every row and the Now Playing bar along the bottom — the playing
track's cover behind it, and its progress as a thin line across the
top](docs/screenshots/library.webp)

## What it does

**Gapless playback.** The feature the whole architecture exists to protect, and
the one every wrapper gets wrong. Slipmat hands MusicKit the entire queue in one
call and then keeps its hands off it, so a segued album crosses its boundaries
the way it was mastered to. This is measured, not hoped for — see
[Gapless, verified](#gapless-verified) below.

**Your library, natively.** Songs as a virtualised list; albums, artists and
playlists as grids. Type-to-find filtering on all four. Artist portraits come
from Apple's catalogue, and covers are cached to disk as they scroll into view.

![The Albums grid, covers loading as tiles scroll into
view](docs/screenshots/albums.webp)

**The whole catalogue.** Search Apple Music, paginated as you scroll. Results
mix artists and albums above the songs, and either one opens a page you can play
from and drill through — artist → album → track.

![An artist page reached from a catalogue search: the portrait, the genre, and
25 albums each with a chevron into its own page](docs/screenshots/search.webp)

![A playlist page: the four-up cover Apple builds from its tracks, the count,
Play and Shuffle, and the tracks below](docs/screenshots/playlist.webp)

**The player opens out.** Drag the Now Playing bar up, or press the queue
button, and it becomes a full player: the artwork large, the transport under it,
and the queue beside it. The cover sits behind the whole thing, blurred and
drifting, and cross-fades when the track changes.

![The expanded player: large artwork, transport beneath it, the queue alongside,
and the cover blurred behind the whole surface](docs/screenshots/player.webp)

**A queue you can see.** Inside that player rather than a modal, opening on the
track that is playing. Jump to any track, remove any track, without disturbing
playback. Right-click any row — or use its menu button — to play it next, add
it to the queue, save it to your library or favourite it.

**MPRIS, properly.** `org.mpris.MediaPlayer2.Slipmat`, bidirectional. The GNOME
Shell applet and the lock screen show correct metadata and artwork, and their
controls reach the player. Hardware media keys work. Half-working MPRIS is the
most common failure of the wrappers; this one is tested over `busctl`.

**The GNOME furniture.** Preferences, keyboard shortcuts, an About dialog,
opt-in track-change notifications, a proper app icon and `.desktop` entry.

## Gapless, verified

Verified 2026-07-26 across four consecutive boundaries of a segued album:

- Every transition happened **unprompted** — Slipmat sent nothing at any
  boundary. MusicKit advanced a queue it already held, which is the only way the
  transition can be seamless.
- Wall-clock between transitions matched each track's length to the second, so
  no track was cut short.
- The PipeWire stream was **created once and never torn down**. One sink-input
  survived all four boundaries, which means the decoder ran continuously.
- No audible gap.

You can re-run it: `make gapless` in one terminal, `RUST_LOG=slipmat=info cargo
run` in another. The procedure is in [CLAUDE.md](CLAUDE.md).

## How it works, honestly

Apple Music full tracks are HLS + **Widevine** DRM. On Linux the only Widevine
CDM that exists is the one Google ships inside Chromium. There is no way around
that — WebKitGTK has no CDM, and GStreamer cannot decrypt the stream. **A 100%
native Apple Music player cannot be built.**

So Slipmat splits the problem:

```
┌──────────────────────────────────────────────────┐
│  Slipmat — Rust · relm4 · libadwaita             │  ← everything you see
│  library · search · queue · Now Playing          │
│  MPRIS · notifications · media keys · artwork    │
│  HTTPS ─────────────────► api.music.apple.com    │
└───────────────────────┬──────────────────────────┘
                        │  newline-delimited JSON over stdio
┌───────────────────────▼──────────────────────────┐
│  sidecar — castLabs Electron                      │  ← invisible after login
│  hidden music.apple.com  +  MusicKit  +  Widevine │
│  → audio straight to PipeWire, untouched          │
└───────────────────────────────────────────────────┘
```

All browsing, search and metadata is native code talking to Apple's REST API and
drawing native widgets. Only the **audio decode** happens in the sidecar — a
Chromium window with `show: false`, displayed exactly once for Apple's own
sign-in and then never again. It is never rendered, it does not appear in the
dash, and it does not publish an MPRIS player of its own.

Slipmat plays through Apple's own MusicKit player with Google's official CDM. It
is a native front-end and a remote control for a licensed session. It does not
strip DRM, does not use extracted CDMs, and does not download anything.

## Requirements

- An **active Apple Music subscription**
- x86_64 (Widevine on Linux is x86_64 only)
- **A network connection, always** — see Limitations

To *build* it you also need GTK ≥ 4.20, libadwaita ≥ 1.8 and Rust ≥ 1.93
(relm4 0.11's MSRV), plus Node and npm — verified on Node 26. That floor is
recent enough to rule out most distributions today: Debian stable, Ubuntu 24.04
and Fedora ≤ 42 ship an older libadwaita and cannot build it.

**The Flatpak carries its own runtime, so none of that applies to running it.**

```bash
# CachyOS / Arch — for building from source
sudo pacman -S --needed base-devel pkgconf rust gtk4 libadwaita librsvg nodejs npm
```

## Install

### Flatpak — any distribution

The Flatpak bundles the GNOME 49 runtime, so your system's libadwaita does not
matter. This is the route for anything that cannot build Slipmat, which today is
most things.

```bash
flatpak install ./Slipmat.flatpak
flatpak run dev.miguelrincon.Slipmat
```

It is **not on Flathub** and is not intended to be. Build one yourself with
`make flatpak-bundle`, which needs `org.flatpak.Builder` and takes a few
minutes; see [`packaging/flatpak/README.md`](packaging/flatpak/README.md).

### Arch and derivatives

PKGBUILDs live in [`packaging/aur/`](packaging/aur/) — `slipmat` for the latest
release, `slipmat-git` to track `main`.

### From source

```bash
make install     # build + install to ~/.local, no sudo
```

Then launch **Slipmat** from the app grid, or run `slipmat`. If that command is
not found, `~/.local/bin` is not on your `PATH` — most shells add it, some do
not.

Every route fetches castLabs Electron — **about 200 MB of Chromium**, once.
That is the Widevine boundary and there is no smaller version of it; see
[Limitations](#limitations). From source, `make sidecar` does that step on its
own if you want it separately.

On first run, Apple's sign-in window opens once; after you authenticate it hides
for good. Notifications need the app to be installed, and the first time may
need a fresh login so the shell picks up the new `.desktop` entry and icon.

**No token is ever written to disk.** Both the developer token and the Music
User Token are re-harvested from the running MusicKit instance on every launch;
what persists your login is the sidecar's own session cookie, exactly as it
would in a browser. If Apple rotates a token, Slipmat follows automatically, and
there is nothing cached for anyone to find.

## Limitations

These are properties of the platform, not a to-do list:

- **No offline or downloaded playback.** Linux's Widevine CDM does not support
  persistent licences (it reports `PLATFORM_UNVERIFIED`), so every track must be
  licensed live. Slipmat cannot work on a plane.
- **~200 MB on disk for the sidecar.** It is a full Chromium. That is the cost
  of the only CDM that exists.
- **x86_64 only.** No ARM Widevine on Linux.
- **Apple can change `music.apple.com`.** The hook that drives MusicKit is small
  and defensive, but it is the one surface outside our control. If Apple moves
  something, Slipmat says so rather than silently spinning.
- **A few library tracks may be unplayable.** Apple delists tracks while leaving
  them in your library. Slipmat finds them on first attempt, remembers them
  across sessions, and dims them rather than letting one break a queue.

Known bugs live in [the issue tracker](https://github.com/SoftARV/Slipmat/issues).

## Development

```bash
cargo run                                    # dev
RUST_LOG=slipmat=debug cargo run             # trace the sidecar protocol
make sidecar-run                             # sidecar alone, window VISIBLE
make gapless                                 # watch the audio stream across a boundary
cargo clippy --all-targets -- -D warnings    # the bar
make check                                   # fmt + clippy + test
```

Debug in layers — `make sidecar-run` first. If a track won't play with the
window visible, the problem is DRM or Apple, not Rust.

See [CLAUDE.md](CLAUDE.md) for the full architecture, the rules the code
follows, and the traps that cost real debugging time.

## Related

Slipmat is the third in a series of native GNOME apps: **Dockyard** (Docker) and
**Pitwall** (GitHub Actions).

Prior art worth crediting — both take the wrapper approach Slipmat is reacting
to, and both worked out the castLabs Electron path first:
[Sidra](https://github.com/wimpysworld/sidra) and
[Cider](https://cider.sh).

## Licence

GPL-3.0-or-later. See [COPYING](COPYING).
