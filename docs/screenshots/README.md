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

| File | Shows |
| --- | --- |
| `library.webp` | Songs — the playing track marked, bar active |
| `albums.webp` | The Albums grid |
| `search.webp` | A catalogue search: artists and albums above songs |
| `queue.webp` | Playlists, with the queue sidebar open |

## Taking them

- **Dark theme**, since that is what the app looks like on a default GNOME
  install and what every shot here uses.
- Capture the **window, not the screen** — `Alt`+`PrtSc`. GNOME's shadow and
  rounded corners come along with it, which is what makes a GTK4 app look like
  one.
- Play something first, and keep the *same* track across the set. A Now Playing
  bar reading "Nothing playing" undersells every screenshot it appears in, and a
  different track in each one reads as four unrelated apps.
- Avoid a half-loaded state: no spinners, no placeholder covers.

## Processing

Resize to 1600px wide, then pick a format **by what is in the picture**:

```bash
# UI and text — lossless, so glyphs stay crisp
magick shot.png -resize 1600x -strip \
  -define webp:lossless=true -define webp:method=6 docs/screenshots/library.webp

# Mostly album art — lossy, since lossless gains nothing and costs 4x
magick shot.png -resize 1600x -strip \
  -quality 92 -define webp:method=6 docs/screenshots/albums.webp
```

WebP rather than PNG throughout: the four shots here are **724 KB** together,
where the same images as PNG were **2.2 MB**. A repository carries its blobs
forever, so this is worth two minutes.

Keep each one under ~500 KB. If one is stubborn, `-resize 1400x` first — nobody
reads a README at full resolution.
