<!--
SPDX-FileCopyrightText: 2026 Miguel Rincon
SPDX-License-Identifier: GPL-3.0-or-later
-->

<p align="center">
  <img src="docs/screenshots/icon.png" width="128" alt="Slipmat icon">
</p>

# Slipmat

Apple Music for Linux, drawn natively.

Apple ships no Linux client, and the DRM makes a fully native one impossible —
so Slipmat draws its own interface, written in Rust, and keeps the web engine
strictly for decoding audio. A GNOME app, a terminal player, and one background
engine they share.

The web engine is still there. You just never see it.

<p align="center">
  <img src="docs/screenshots/library.webp" alt="Slipmat showing a library of songs with the sidebar collapsed: the playing track marked in red, two Apple cannot stream greyed out, a menu button on every row, and the Now Playing bar along the bottom with the playing track's cover behind it">
</p>

## What it does

**Native, twice.** A GNOME app in GTK4 and libadwaita — real lists, real
keyboard navigation, a window that tiles to half a screen. And `climat`, a
player for the terminal, in the shape of Winamp. Both drive the same engine, so
they show the same queue and the same track at the same moment. There is a web
layer, because Apple's DRM leaves no choice, but it is one hidden process that
decodes audio and nothing else. You never see a web page.

**Your library.** Songs, albums, artists and playlists, each with type-to-find
filtering and its own sorting. Click anything and the list you are looking at
becomes the queue.

**A player worth the name.** Gapless, with a full-size view you can pull up, the
queue beside it, and the cover behind it.

**Controls where you already look.** Play, pause and skip from the GNOME top
bar, from the lock screen, or with your keyboard's media keys — cover and title
alongside them, and the position honest while it plays. Close every window and
the music keeps going — playback lives in a daemon, not in whichever front-end
you happened to open. It stops when you say so, not when you tidy your desktop.

**All of Apple Music, searchable.** Artists, albums, playlists and songs in one
list, paginated as you scroll, each opening a page you can play from and drill
through.

**Quick, and out of the way.** It opens on your library rather than on a
spinner, a five-hundred-track list scrolls without stuttering, and whatever you
were playing is waiting next launch — restored, never resumed. An app that
starts making noise because you opened it is a hostile one.

<table>
  <tr>
    <td colspan="2" width="33%">
      <img src="docs/screenshots/albums.webp" alt="The Albums grid in a dark theme: four covers to a row, each with its title and artist beneath, and the sidebar open alongside">
    </td>
    <td colspan="2" width="33%">
      <img src="docs/screenshots/artists.webp" alt="An artist page for the Foo Fighters: a round portrait pulled from the catalogue, their genre, and their four albums listed below">
    </td>
    <td colspan="2" width="33%">
      <img src="docs/screenshots/playlist.webp" alt="A playlist page: the four-up cover Slipmat composes from the first four tracks, the song count, Play and Shuffle, and the tracks below">
    </td>
  </tr>
  <tr>
    <td colspan="2" align="center"><sub>Albums, sidebar collapsed</sub></td>
    <td colspan="2" align="center"><sub>Artists, portraits from the catalogue</sub></td>
    <td colspan="2" align="center"><sub>A playlist — cover built from its tracks</sub></td>
  </tr>
  <tr>
    <td colspan="3" width="50%">
      <img src="docs/screenshots/player.webp" alt="The expanded player: large artwork, transport beneath it, a 522-track queue alongside, and the cover blurred behind the whole surface">
    </td>
    <td colspan="3" width="50%">
      <img src="docs/screenshots/search.webp" alt="A catalogue search for the Beatles: artists, Apple Music playlists, albums and songs in one list, each opening a page of its own">
    </td>
  </tr>
  <tr>
    <td colspan="3" align="center"><sub>The player opened out, queue alongside</sub></td>
    <td colspan="3" align="center"><sub>Searching the whole catalogue</sub></td>
  </tr>
</table>

And the same library, the same queue and the same player, in a terminal:

```
CLImat
Broken Arrows  —  Avicii
Stories

         ▂▂▄▂▄▆▅█▁▂▁ ▁   ▂▂ ▁            ▃▁▃          ▁
    ▇▇▇▇▇███████▆▆▆████████▆▇▇█▃▄▄█▆█▂██▇█▄▆▅▇▇▆▇▇▅▇███▇▅▅▆▂▁

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━──────────────────────────
▶  Playing                                                1:38 / 3:53
shuffle on   repeat off   vol ████████████████░░░░

songs   albums   artists   playlists   apple music   QUEUE ───── [112]
    12  Aerodynamic                          Daft Punk           3:32
▸   13  Broken Arrows                        Avicii              3:52
    14  I Would Like                         Zara Larsson        3:44

[space] play/pause   [↑↓] move   [↵] play   [⇥] tabs   [Ctrl+C] hide
```

## Install

You need an **active Apple Music subscription**, an **x86_64** machine (Widevine
on Linux is x86_64 only), and **a network connection every time you play** —
that last one is permanent, not a limitation we intend to lift. See
[Limitations](#limitations).

Every route fetches castLabs Electron once — **about 200 MB of Chromium**. That
is the Widevine boundary and there is no smaller version of it.

### Arch and derivatives

```bash
yay -S slipmat        # the latest release
yay -S slipmat-git    # tracks main: the GNOME app
yay -S climat-git     # tracks main: the terminal player
```

The release and `-git` lines conflict with each other, as the convention
requires. `slipmat-git` rebuilds from whatever is on `main`, so it picks up
unreleased work — including anything half-finished.

Either `-git` front-end pulls in `slipmat-daemon-git`, which is the engine and
the sidecar. Install both and they share one copy of it, and one player.

### Flatpak — any distribution

Slipmat needs libadwaita ≥ 1.8 and GTK ≥ 4.20, which rules out Debian stable,
Ubuntu 24.04 and Fedora ≤ 42. **The Flatpak carries the GNOME 49 runtime with
it, so your system's libadwaita stops mattering** — verified from nothing on a
stock Ubuntu 25.10, which cannot build Slipmat at all.

Download `Slipmat.flatpak` from the [latest
release](https://github.com/SoftARV/Slipmat/releases/latest), then:

```bash
flatpak install ./Slipmat.flatpak
flatpak run dev.miguelrincon.Slipmat
```

A bundle carries the app and **not** the runtime it sits on, so the install
offers to fetch GNOME 49 from Flathub the first time. Answer yes, or it stops
with *"requires the runtime org.gnome.Platform/x86_64/49 which was not found"*.
If your machine has no Flathub remote at all:

```bash
flatpak remote-add --if-not-exists --user flathub \
  https://dl.flathub.org/repo/flathub.flatpakrepo
```

It is **not on Flathub** and is not intended to be.

The bundle carries the GNOME app, the engine and the sidecar. `climat` is not in
it — a terminal player inside a desktop sandbox is a strange place to put one,
and the AUR package or a build from source is the better route.

### From source

```bash
sudo pacman -S --needed base-devel pkgconf rust gtk4 libadwaita librsvg \
                        nodejs npm libpulse
make install     # build + install to ~/.local, no sudo
```

Also needs Rust ≥ 1.93 (relm4 0.11's MSRV) and Node — verified on Node 26. Then
launch **Slipmat** from the app grid, or run `slipmat`. If that command is not
found, `~/.local/bin` is not on your `PATH`.

`climat` installs alongside it. It needs a graphical session even though it
draws in a terminal — not for itself, but because the daemon behind it runs
Chromium, and Chromium wants a display server even with its window hidden. So it
will not work over a plain SSH connection to a headless machine. That is
Widevine, not a shortcut taken here.

### First run

Apple's own sign-in window opens once, and hides for good after you
authenticate. It is the same session whichever front-end you start. Notifications need the app to be installed — running from a build
tree, the shell has no `.desktop` entry to attach them to.

## Support

Slipmat is free, GPL-3, and stays that way. It is also a spare-time project for
one Linux laptop that other people happen to find useful.

If it earns a place on yours:

[![ko-fi](https://ko-fi.com/img/githubbutton_sm.svg)](https://ko-fi.com/miguelrincon)

## Limitations

These are properties of the platform, not a to-do list:

- **No offline or downloaded playback.** Linux's Widevine CDM does not support
  persistent licences (it reports `PLATFORM_UNVERIFIED`), so every track must be
  licensed live. Slipmat cannot work on a plane.
- **~200 MB on disk for the sidecar.** It is a full Chromium. That is the cost
  of the only CDM that exists.
- **x86_64 only.** No ARM Widevine on Linux.
- **`climat` needs a desktop session.** A terminal player that will not work
  over SSH is a surprise, and it is the same chain: the daemon runs Chromium,
  and Chromium wants a display server even with its window never mapped.
- **Apple can change `music.apple.com`.** The hook that drives MusicKit is small
  and defensive, but it is the one surface outside our control. If Apple moves
  something, Slipmat says so rather than silently spinning.
- **A few library tracks may be unplayable.** Apple delists tracks while leaving
  them in your library. Slipmat finds them on first attempt, remembers them
  across sessions, and dims them rather than letting one break a queue.

Known bugs live in [the issue tracker](https://github.com/SoftARV/Slipmat/issues).

## How it works, honestly

Apple Music full tracks are HLS + **Widevine** DRM. On Linux the only Widevine
CDM that exists is the one Google ships inside Chromium. There is no way around
that — WebKitGTK has no CDM, and GStreamer cannot decrypt the stream. **A 100%
native Apple Music player cannot be built.**

So Slipmat splits the problem three ways:

```
┌────────────────────────────┐   ┌────────────────────────────┐
│  Slipmat — GTK4/libadwaita │   │  climat — the terminal     │  ← what you see
│  library · search · queue  │   │  same, drawn in text       │
└─────────────┬──────────────┘   └─────────────┬──────────────┘
              │      newline-delimited JSON over a Unix socket
              └──────────────┬─────────────────┘
┌────────────────────────────▼─────────────────────────────────┐
│  slipmatd — the engine                        ← no window     │
│  owns the queue · MPRIS · artwork · the library cache         │
│  HTTPS ──────────────────────────────► api.music.apple.com    │
└────────────────────────────┬─────────────────────────────────┘
                             │  the same JSON, over stdio
┌────────────────────────────▼─────────────────────────────────┐
│  sidecar — castLabs Electron               ← invisible        │
│  hidden music.apple.com  +  MusicKit  +  Widevine             │
│  → audio straight to PipeWire, untouched                      │
└───────────────────────────────────────────────────────────────┘
```

**The daemon exists because the sidecar cannot be shared any other way.** One
Widevine CDM, one Chromium profile lock — two apps cannot each run one. So the
engine owns it and everything else is a client, which is also why closing a
window does not stop the music and why two of them show the same track.

Nothing enables it and nothing has to: the first client to start finds no daemon
and starts one. It puts its Chromium down after five idle minutes with nothing
playing, and picks it back up when asked.

All browsing, search and metadata is native code talking to Apple's REST API and
drawing native widgets. Only the **audio decode** happens in the sidecar — a
Chromium window with `show: false`, displayed exactly once for Apple's own
sign-in and then never again. It is never rendered, it does not appear in the
dash, and it does not publish an MPRIS player of its own.

Slipmat plays through Apple's own MusicKit player with Google's official CDM. It
is a native front-end and a remote control for a licensed session. It does not
strip DRM, does not use extracted CDMs, and does not download anything.

## Development

```bash
cargo run                                    # the GNOME app
cargo run -p climat                          # the terminal player
RUST_LOG=slipmatd=debug cargo run -p slipmatd   # the engine, with its protocol
make sidecar-run                             # sidecar alone, window VISIBLE
make gapless                                 # watch the audio stream across a boundary
cargo clippy --all-targets -- -D warnings    # the bar
make check                                   # fmt + clippy + test
```

Debug in layers, from the bottom — `make sidecar-run` first. If a track will not
play with the window visible, the problem is DRM or Apple, not Rust. If it plays
there but not in an app, the layer above is the daemon, and it is the one with
the logs.

Both front-ends start a daemon if none is listening, so a stale one from an
earlier build is easy to be fooled by. `pgrep -x slipmatd` before doubting a
change.

The module headers in `src/` say what each file is for,
the long comments are the traps and the measurements behind them,
and the merged PRs carry the reasoning for each change.

## Related

Prior art worth crediting: [Sidra](https://github.com/wimpysworld/sidra) and
[Cider](https://cider.sh) worked out the castLabs Electron path first, which is
what makes any of this possible on Linux. They take a different approach to the
interface; Slipmat would not exist without the groundwork.

## Licence

GPL-3.0-or-later. See [COPYING](COPYING).
