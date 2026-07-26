#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Miguel Rincon
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Watch the sidecar's audio stream across a track boundary.
#
# Gapless has two halves and this measures the one your ears cannot.
#
#   1. Rust must not drive the queue. `RUST_LOG=tonearm=info` prints a
#      "track transition" line at every boundary saying what prompted it.
#      A natural boundary must read `prompted_by="nothing — MusicKit
#      advanced itself"`. Anything else means Rust is feeding tracks one at
#      a time and rule 3 is broken.
#
#   2. The decoder must not stop. That is this script. If the PipeWire
#      stream is torn down and rebuilt at the boundary, there is a gap no
#      matter what the log says — you will see `removed` then `new`.
#      A gapless transition keeps one stream alive throughout.
#
# Neither replaces listening. Both tell you *where* a gap came from.

set -uo pipefail

if ! command -v pactl >/dev/null; then
	echo "pactl not found — install libpulse (PipeWire's pulse shim)." >&2
	exit 1
fi

# The sidecar's stream, not the whole system's. Chromium names it after the
# app; `app.setName('Tonearm')` in sidecar/main.js is what makes this work.
stream_of_interest() {
	pactl list sink-inputs 2>/dev/null |
		grep -iE "index:|application.name|media.name" |
		sed 's/^[[:space:]]*//'
}

echo "Watching PipeWire sink-inputs. Play across a track boundary."
echo "Expect: nothing at all. A 'remove' followed by a 'new' is a gap."
echo
echo "--- streams right now ---"
stream_of_interest
echo "-------------------------"
echo

# `pactl subscribe` emits a line per event; timestamp them so the boundary can
# be lined up against the app's own "track transition" log.
pactl subscribe 2>/dev/null | while read -r line; do
	case "$line" in
	*"on sink-input"*)
		printf '%s  %s\n' "$(date '+%H:%M:%S.%3N')" "$line"
		case "$line" in
		*remove*)
			echo "    ^^ the stream went away — that is an audible gap."
			;;
		esac
		;;
	esac
done
