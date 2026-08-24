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

  ▶   ██░░░░░░░░░░░░░░░░░░░░░░░   0:21 / 5:03
      shuffle off   repeat off   vol ██████████

  z prev   x play   c pause   b next   _ hide   q quit
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

## Keys

| Key | What it does |
| --- | --- |
| `z` | Previous track |
| `x` | Play |
| `c` | Pause |
| `b` | Next track |
| `space` | Play / pause |
| `←` `→` | Seek five seconds |
| `_` | Hide — leave, and let the music keep playing |
| `q` | Quit — stop the daemon and the music with it |

`q` and `_` are the whole difference between closing the window and closing the
player. `q` is refused while another Slipmat client is open, because quitting
would take the player from a window somebody else is looking at.

## Running it

```
cargo run -p climat
```

The daemon starts itself on the first connection, so there is nothing to enable
and no service to install.
