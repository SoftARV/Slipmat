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
#
# NOTE ON CLOCKS: the timestamps below are local time; `tracing` logs UTC.
# The header prints the offset so the two logs can be lined up — the first
# run of this script cost a few minutes to working out that a "gap" two
# hours adrift was in fact a screenshot shutter.

set -uo pipefail

if ! command -v pactl >/dev/null; then
	echo "pactl not found — install libpulse (PipeWire's pulse shim)." >&2
	exit 1
fi

# Which sink-input belongs to Tonearm. Matched on any property containing the
# name, because Chromium spreads it across application.name,
# application.process.binary and media.name depending on the version — and
# `app.setName('Tonearm')` in sidecar/main.js is what puts it there at all.
#
# This filter is the point: a notification sound or a screenshot shutter opens
# and closes its own short-lived stream, and reporting those as gaps makes the
# whole instrument untrustworthy.
tonearm_indices() {
	pactl list sink-inputs 2>/dev/null | awk '
		/^Sink Input #/ { idx = substr($3, 2); hit = 0 }
		tolower($0) ~ /tonearm/ { hit = 1 }
		/^[[:space:]]*$/ { if (hit && idx != "") print idx; idx = ""; hit = 0 }
		END { if (hit && idx != "") print idx }
	'
}

echo "Watching Tonearm's audio stream. Play across a track boundary."
echo "Expect: nothing at all. A 'remove' of Tonearm's own stream is a gap."
echo
printf 'Timestamps below are local time; the app logs UTC (offset %s).\n' \
	"$(date '+%:z')"
echo

known=$(tonearm_indices | tr '\n' ' ')
if [ -n "${known// /}" ]; then
	echo "Tonearm is already playing on sink-input(s): $known"
else
	echo "Tonearm is not playing yet — its stream will appear when you hit play."
fi
echo

pactl subscribe 2>/dev/null | while read -r line; do
	case "$line" in
	*"on sink-input"*) ;;
	*) continue ;;
	esac

	index=${line##*#}
	stamp=$(date '+%H:%M:%S.%3N')

	case "$line" in
	*remove*)
		# Already gone from `pactl list`, so this leans on what we saw when
		# it appeared.
		case " $known " in
		*" $index "*)
			echo "$stamp  Tonearm's stream #$index went away"
			echo "    ^^ the decoder stopped — that is an audible gap."
			known=${known// $index / }
			;;
		esac
		;;
	*)
		current=$(tonearm_indices | tr '\n' ' ')
		case " $current " in
		*" $index "*)
			[ -n "${known// /}" ] || echo "$stamp  Tonearm's stream #$index appeared"
			known=$current
			;;
		esac
		;;
	esac
done
