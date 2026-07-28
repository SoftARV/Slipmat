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
native: GTK4 and libadwaita, written in Rust, with real lists, real GNOME
search, and media controls that answer from the top bar and the lock screen.

The web engine is still there. You just never see it.

<p align="center">
  <img src="docs/screenshots/library.webp" alt="Slipmat showing a library of songs: the playing track marked in red, one Apple cannot stream greyed out, a menu button on every row, and the Now Playing bar along the bottom with the playing track's cover behind it">
</p>

## What it does

**A native GNOME app, not a browser in a costume.** GTK4 and libadwaita, written
in Rust — real lists, real keyboard navigation, a window that tiles to half a
screen. There is a web layer, because Apple's DRM leaves no choice, but it is
one hidden process that decodes audio and nothing else. You never see a web
page.

**Your library.** Songs, albums, artists and playlists, each with type-to-find
filtering and its own sorting. Click anything and the list you are looking at
becomes the queue.

**A player worth the name.** Gapless, with
a full-size view you can pull up, the queue beside it, and the cover behind it.
[Measured, not hoped for](#gapless-verified).

**Controls where you already look.** Play, pause and skip from the GNOME top
bar, from the lock screen, or with your keyboard's media keys — cover and title
alongside them, and the position honest while it plays. Close the window and the
music keeps going; it quits when you tell it to, not when you tidy your desktop.

**All of Apple Music, searchable.** Artists, albums, playlists and songs in one
list, paginated as you scroll, each opening a page you can play from and drill
through.

**Quick, and out of the way.** A five-hundred-track library scrolls without
stuttering, a section you have already opened comes back instantly, and whatever
you were playing is waiting next launch — restored, never resumed. An app that
starts making noise because you opened it is a hostile one.

<table>
  <tr>
    <td width="33%" valign="top">
      <img src="docs/screenshots/albums.webp" alt="The Albums grid with the sidebar collapsed, covers loading as tiles scroll into view">
      <p align="center"><sub>Albums, with the sidebar collapsed</sub></p>
    </td>
    <td width="33%" valign="top">
      <img src="docs/screenshots/artists.webp" alt="The Artists grid: round portraits, each pulled from the artist's catalogue twin, with their genre beneath">
      <p align="center"><sub>Artists, portraits from the catalogue</sub></p>
    </td>
    <td width="33%" valign="top">
      <img src="docs/screenshots/playlist.webp" alt="A playlist page: the four-up cover Slipmat composes from the first four tracks, the song count, Play and Shuffle, and the tracks below">
      <p align="center"><sub>A playlist — cover built from its tracks</sub></p>
    </td>
  </tr>
  <tr>
    <td colspan="2" valign="top">
      <img src="docs/screenshots/player.webp" alt="The expanded player: large artwork, transport beneath it, a 522-track queue alongside, and the cover blurred behind the whole surface">
      <p align="center"><sub>The player opened out, queue alongside</sub></p>
    </td>
    <td valign="top">
      <img src="docs/screenshots/search.webp" alt="A catalogue search for the Beatles: artists, Apple Music playlists, albums and songs in one list, each opening a page of its own">
      <p align="center"><sub>Searching the whole catalogue</sub></p>
    </td>
  </tr>
</table>

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
