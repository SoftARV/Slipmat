<!--
SPDX-FileCopyrightText: 2026 Miguel Rincon
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Screenshots

What the top-level README expects to find here, and what each shot should show.

`icon.png` is **generated, not captured** — it is the app's own SVG rendered at
128px, so it can never drift from what the app actually installs:

```bash
rsvg-convert -w 128 -h 128 \
  data/icons/hicolor/scalable/apps/dev.miguelrincon.Tonearm.svg \
  -o docs/screenshots/icon.png
```

## The shots

| File | Shows | Why it earns its place |
| --- | --- | --- |
| `library.png` | Songs, a full library list | The thing a wrapper cannot do: a real, virtualised, native list |
| `album.png` | An album or playlist page, playing | Cover, track list, and the Now Playing bar in one frame |
| `grid.png` | Albums or Artists | Round artist portraits are the most obviously *native* detail |
| `search.png` | Apple Music results | Artists and albums above songs — the way in to the catalogue |
| `gapless.png` | The run beside its log | The evidence for the one claim that matters |

Only the first two are load-bearing. The rest are nice.

## Taking them

- **Dark theme**, since that is what the app looks like on a default GNOME
  install and what every other shot here uses.
- Capture the **window, not the screen** — `Alt`+`PrtSc`, or the area tool.
  GNOME's shadow and rounded corners come along with it, which is what makes a
  GTK4 app look like one.
- Play something first. A Now Playing bar reading "Nothing playing" undersells
  every screenshot it appears in.
- Avoid a half-loaded state: no spinners, no placeholder covers.

## Processing

Resize to 1600px wide and strip metadata, so the repo does not accumulate
multi-megabyte PNGs:

```bash
magick shot.png -resize 1600x -strip docs/screenshots/album.png
```

Keep each one **under ~500 KB**. If one is stubborn, `-resize 1400x` first —
nobody reads a README at full resolution.
