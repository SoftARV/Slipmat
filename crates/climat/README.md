<!--
SPDX-FileCopyrightText: 2026 Miguel Rincon
SPDX-License-Identifier: GPL-3.0-or-later
-->

# climat

Apple Music in a terminal, in the shape of Winamp.

`climat` is a client of `slipmatd` — the same daemon the GTK app talks to. It
draws what the daemon says and sends what the keyboard asks for, so a terminal
and a window open at once are two views of one player rather than two players.

```
  Addicted To a Memory (feat. Bahari)  —  Zedd

    ▂▂▄▂▄▆▅█▁▂▁ ▁   ▂▂ ▁
  ▅▅█████████████▆▇▇████▃▃▃▄▁
  ▶   ██░░░░░░░░░░░░░░░░░░░░░░░   0:21 / 5:03
      shuffle off   repeat off   vol ██████████

  QUEUE   30 tracks
  ▸   1  Addicted To a Memory (feat. Bahari)     Zedd              5:03
      2  Aftershock (feat. Jacquie Lee)          Cash Cash         3:26

  SONGS   albums   artists   playlists   /muse
  ▸ Absolution                            Muse · 2003
  ▸ Drones                                Muse · 2015
  ▸ The Resistance                        Muse · 2009

  space play/pause   ↑↓ move   ↵ play/open   / filter   1-4 section   ⇥ pane   _ hide   q quit
```

## It needs a graphical session

**This is the honest caveat, and it is not going away.** Apple Music's full
tracks are Widevine-protected, and the only Widevine module on Linux ships
inside Chromium. `slipmatd` therefore runs a hidden Chromium to decode audio,
and Chromium wants a display server even with no window on screen.

So `climat` runs in a terminal, but it needs that terminal to be on a machine
with a desktop session — a terminal emulator under Wayland or X11, or `tmux`
inside one. It will not work over a plain SSH connection to a headless box.
That is a limit imposed by the DRM, not a shortcut taken here.

## The bars

climat never touches the audio — the sidecar owns the stream — so the
visualiser *listens*, reading the sink's monitor source, the loopback every
output exposes. That is how `cava` and every other visualiser works, and it
means the bars follow whatever is playing, including a track the GNOME window
started.

It talks PulseAudio rather than PipeWire, because PipeWire ships
`pipewire-pulse` and answers to it — one backend covers both servers. If there
is no audio server to listen to, the bars simply never appear and nothing else
changes.

The windows overlap, so the bars move about every 12ms while each transform
still looks at 46ms of audio — the frame rate and the frequency resolution are
separate things. Two rows rather than one, because a block character gives
eight heights and a bar has to cross an eighth of its range before anything
changes on screen. Measured on a release build at 1.1% of a core playing, 0.1%
paused.

## Keys

| Key | What it does |
| --- | --- |
| `space` | Play / pause |
| `z` `b` | Previous / next track |
| `←` `→` | Seek five seconds |
| `s` `r` | Shuffle / repeat |
| `-` `+` | Volume |
| `⇥` | Switch between the library and the queue |
| `1` – `4` | Songs · albums · artists · playlists |
| `5` | All of Apple Music |
| `/` | Filter the library, or search Apple Music |
| `esc` | Out of a page, then out of a filter |
| `↑` `↓`, `k` `j` | Move the cursor |
| `Home` | Put the cursor back on the playing track |
| `↵` | Play the selected track, or open the album it leads to |
| `d` | Remove it from the queue |
| `K` `J` | Move it up / down |
| `_` | Hide — leave, and let the music keep playing |
| `q` | Quit — stop the daemon and the music with it |

A `▸` marks a row that opens a page rather than playing.

**Who answers decides what a keystroke costs.** Over the library the list
narrows as you type, which is affordable because it never reaches Apple — the
daemon replies from the library it already holds, a round trip to a local
socket. Over Apple Music every query is a real request to somebody else's API,
so typing only edits the text and `↵` is what sends it. Same box, same key, two
rules, and the bottom row says which one is in force.

The bottom row changes with which pane has focus — only the queue reorders,
only the library filters — and shows the keys that fit, dropping the least essential first —
so a narrow window loses the reorder hints rather than losing the row. Leaving
and quitting are always on it.

Nothing here edits the queue directly. A key sends a request and the rows move
when the daemon echoes, which is what keeps a terminal and a GTK window from
disagreeing about what is playing. The cursor is the exception: it is where
*this* terminal is looking, so it moves the instant you press a key.

`q` and `_` are the whole difference between closing the window and closing the
player. `q` is refused while another Slipmat client is open, because quitting
would take the player from a window somebody else is looking at.

## Running it

```
cargo run -p climat
```

The daemon starts itself on the first connection, so there is nothing to enable
and no service to install.
