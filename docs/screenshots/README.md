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
  data/icons/hicolor/scalable/apps/dev.miguelrincon.Slipmat.svg \
  -o docs/screenshots/icon.png
```

## The shots

| File | Shows |
| --- | --- |
| `library.webp` | Songs — the play marker, unplayable tracks, row menus |
| `search.webp` | A catalogue search: artists, playlists, albums and songs mixed |
| `albums.webp` | The Albums grid, four to a row |
| `artists.webp` | An artist page — the round portrait from the catalogue twin, and their albums |
| `playlist.webp` | A playlist page: the composed mosaic, Play and Shuffle, tracks |
| `player.webp` | The expanded player: artwork, transport, queue |

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
- Show what is *new*. A shot that could have been taken three versions ago is a
  wasted one.

  This matters more than it sounds, and it has now happened twice. The 0.1 set
  went stale in one release in a way that was actively misleading:
  `albums.webp` was captioned "with the queue sidebar open", and by 0.2 the
  queue had moved into the player and no sidebar existed. Then the 0.2 set did
  it again — `playlist.webp` was captioned "the four-up cover **Apple** builds
  from its tracks", and by 0.3 that was doubly wrong: Apple sends no artwork at
  all for a playlist you made, and the mosaic in the picture is one Slipmat
  composes itself.

  A screenshot outlives the sentence next to it, so when the UI moves, **check
  the captions as well as the images** — a wrong caption is worse than an old
  picture, because a reader trusts it and it explains what they are looking
  at.

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
