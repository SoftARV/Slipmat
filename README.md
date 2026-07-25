<!--
SPDX-FileCopyrightText: 2026 Miguel Rincon
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Tonearm

A native GNOME client for Apple Music.

Every other Apple Music option on Linux is `music.apple.com` in a costume —
Electron wrapped around the website, with the website's scroll behaviour and the
website's search field. Tonearm is the first one where the interface is actually
native: GTK4 and libadwaita, written in Rust, with real `adw::ActionRow` lists,
real GNOME search, and proper MPRIS integration.

The web engine is still there. You just never see it.

## How it works, honestly

Apple Music full tracks are HLS + **Widevine** DRM. On Linux the only Widevine
CDM that exists is the one Google ships inside Chromium. There is no way around
that — WebKitGTK has no CDM, and GStreamer cannot decrypt the stream. **A 100%
native Apple Music player cannot be built.**

So Tonearm splits the problem:

```
┌──────────────────────────────────────────────────┐
│  Tonearm — Rust · relm4 · libadwaita             │  ← everything you see
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
Chromium window with `show: false` that is displayed exactly once, for Apple's
own sign-in, and then never again.

Tonearm plays through Apple's own MusicKit player with Google's official CDM. It
is a native front-end and a remote control for a licensed session. It does not
strip DRM, does not use extracted CDMs, and does not download anything.

## Requirements

- An **active Apple Music subscription**
- GTK ≥ 4.20, libadwaita ≥ 1.8, Rust ≥ 1.93, Node ≥ 20
- x86_64 (Widevine on Linux is x86_64 only)
- A running keyring provider (`gnome-keyring` on GNOME)
- **A network connection, always** — see Limitations

```bash
# CachyOS / Arch
sudo pacman -S --needed base-devel pkgconf rust gtk4 libadwaita librsvg nodejs npm
```

## Install

```bash
make sidecar     # fetch castLabs Electron (~200 MB, once)
make install     # build + install to ~/.local, no sudo
```

Then launch **Tonearm** from the app grid. On first run, Apple's sign-in window
opens once; after you authenticate it hides for good.

## Limitations

These are properties of the platform, not a to-do list:

- **No offline or downloaded playback.** Linux's Widevine CDM does not support
  persistent licences (it reports `PLATFORM_UNVERIFIED`), so every track must be
  licensed live. Tonearm cannot work on a plane.
- **~200 MB on disk for the sidecar.** It is a full Chromium. That is the cost
  of the only CDM that exists.
- **x86_64 only.** No ARM Widevine on Linux.
- **Apple can change `music.apple.com`.** The hook that drives MusicKit is small
  and defensive, but it is the one surface outside our control. If Apple moves
  something, Tonearm says so rather than silently spinning.

## Development

```bash
cargo run                                    # dev
RUST_LOG=tonearm=debug cargo run             # trace the sidecar protocol
make sidecar-run                             # sidecar alone, window VISIBLE
cargo clippy --all-targets -- -D warnings    # the bar
make check                                   # fmt + clippy + test
```

Debug in layers — `make sidecar-run` first. If a track won't play with the
window visible, the problem is DRM or Apple, not Rust.

See [CLAUDE.md](CLAUDE.md) for the full architecture and the rules the code
follows.

## Related

Tonearm is the third in a series of native GNOME apps: **Dockyard** (Docker) and
**Pitwall** (GitHub Actions).

Prior art worth crediting — both take the wrapper approach Tonearm is reacting
to, and both worked out the castLabs Electron path first:
[Sidra](https://github.com/wimpysworld/sidra) and
[Cider](https://cider.sh).

## Licence

GPL-3.0-or-later. See [COPYING](COPYING).
