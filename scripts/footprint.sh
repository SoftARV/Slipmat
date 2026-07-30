#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Miguel Rincon
# SPDX-License-Identifier: GPL-3.0-or-later
#
# What Slipmat costs the machine it runs on: memory, CPU, and disk.
#
# There is no single number, and the obvious one is wrong. Slipmat is a Rust
# process supervising a Chromium, and Chromium is six or seven processes that
# share most of their pages. Adding up RSS counts that shared memory once per
# process and roughly doubles the answer, which is how a browser engine gets
# reported as costing a gigabyte it is not using.
#
# So this reports **PSS** — proportional set size, where a page shared by four
# processes counts a quarter in each. It is the number that answers "how much
# would I get back if this quit", which is the question being asked.
#
# CPU matters here for a specific reason rather than a general one: two bugs in
# this app's history pinned a core (#37's runaway reducer, and the `#[watch]`
# animation that had to be diagnosed from a core dump). Both would have shown up
# here instantly.
#
# Idle is **not** near zero today, and that is the first thing this script found:
# the backdrop's drift is an `infinite` CSS animation, so the frame clock never
# stops and a fifth of a core goes into repainting a still window. See #126 for
# the measurement and the bisect. Until that is settled, read the CPU column as
# "this plus whatever I changed".
#
#   scripts/footprint.sh          # measure a running Slipmat
#   scripts/footprint.sh --disk   # disk only, no instance needed
#
# Start Slipmat first. Play something before measuring if you want the playing
# figure — a decoder that has never decoded is not the working set.

set -uo pipefail
cd "$(dirname "$0")/.."

BUS=dev.miguelrincon.Slipmat
SAMPLE=${SAMPLE:-3} # seconds of CPU sampling

human() { # KB -> human
	awk -v k="$1" 'BEGIN { if (k > 1048576) printf "%.1f GB", k/1048576;
	                       else if (k > 1024) printf "%.0f MB", k/1024;
	                       else printf "%d KB", k }'
}

# --- disk ---------------------------------------------------------------------

disk() {
	echo "DISK"
	# The sidecar dominates everything else by an order of magnitude, and it is
	# not ours: it is a whole Chromium, which is the price of the only Widevine
	# CDM that exists on Linux. Reported separately for exactly that reason —
	# nothing we write will ever move that number.
	local rows=(
		"the app itself|target/release/slipmat"
		"the sidecar (Chromium)|sidecar/node_modules"
		"Widevine CDM + session|$HOME/.config/Slipmat"
		"artwork + library cache|$HOME/.cache/slipmat"
		"last session|${XDG_STATE_HOME:-$HOME/.local/state}/slipmat"
	)
	local label path size
	for row in "${rows[@]}"; do
		label=${row%%|*}
		path=${row##*|}
		if [ -e "$path" ]; then
			size=$(du -sk "$path" 2>/dev/null | cut -f1)
			printf '  %-24s %10s  %s\n' "$label" "$(human "$size")" "$path"
		else
			printf '  %-24s %10s  %s\n' "$label" "—" "$path (absent)"
		fi
	done
	echo
	echo "  The cache is the only one that grows with use, and it is swept:"
	echo "  see components/prune.rs. The sidecar never changes."
}

if [ "${1:-}" = "--disk" ]; then
	disk
	exit 0
fi

# --- find the app -------------------------------------------------------------

# **Ask the bus, never scan /proc.** An instance started as ./target/debug/slipmat
# whose binary has since been rebuilt no longer matches by name, and a second
# instance hands off to the first and exits silently — so the process you find by
# scanning may not be the one holding the session. The bus is never wrong about
# who owns the name.
PID=$(busctl --user call org.freedesktop.DBus /org/freedesktop/DBus \
	org.freedesktop.DBus GetConnectionUnixProcessID s "$BUS" 2>/dev/null |
	awk '{print $2}')

if [ -z "$PID" ] || [ ! -d "/proc/$PID" ]; then
	echo "Slipmat is not running — start it first, or use --disk." >&2
	exit 1
fi

# Every descendant, depth first. The tree is not two levels: GTK's image loaders
# (glycin) run in bwrap sandboxes that hang off both the app and the sidecar, and
# they are real memory that a naive parent-and-children walk misses.
descendants() {
	local p=$1 c
	echo "$p"
	for c in $(pgrep -P "$p" 2>/dev/null); do descendants "$c"; done
}
mapfile -t PIDS < <(descendants "$PID")

# What each process is for. Chromium tells you in its own argv, and seven rows
# reading "electron" is not a report.
role() {
	local cmd=$1
	case "$cmd" in
	*--type=renderer*) echo "sidecar: renderer (MusicKit)" ;;
	*--type=gpu-process*) echo "sidecar: gpu" ;;
	*--type=zygote*) echo "sidecar: zygote" ;;
	*--type=utility*)
		case "$cmd" in
		*NetworkService*) echo "sidecar: network" ;;
		*AudioService*) echo "sidecar: audio" ;;
		*) echo "sidecar: utility" ;;
		esac
		;;
	*--type=broker*) echo "sidecar: broker" ;;
	*glycin*) echo "image decoder (sandboxed)" ;;
	*bwrap*) echo "sandbox wrapper" ;;
	*electron*) echo "sidecar: main" ;;
	*) echo "the app (Rust + GTK)" ;;
	esac
}

# --- CPU sample ---------------------------------------------------------------

jiffies() { # total user+system jiffies for a pid, 0 if it went away
	awk '{print $14 + $15}' "/proc/$1/stat" 2>/dev/null || echo 0
}

declare -A before
for p in "${PIDS[@]}"; do before[$p]=$(jiffies "$p"); done
sleep "$SAMPLE"

HZ=$(getconf CLK_TCK)

# --- report -------------------------------------------------------------------

echo "MEMORY AND CPU   (pid $PID, ${SAMPLE}s sample)"
printf '  %-28s %9s %9s %7s\n' "" RSS PSS CPU
rss_total=0
pss_total=0
cpu_total=0
for p in "${PIDS[@]}"; do
	[ -r "/proc/$p/smaps_rollup" ] || continue
	rss=$(awk '/^Rss:/  {print $2; exit}' "/proc/$p/smaps_rollup")
	pss=$(awk '/^Pss:/  {print $2; exit}' "/proc/$p/smaps_rollup")
	[ -n "$rss" ] || continue
	cmd=$(tr '\0' ' ' <"/proc/$p/cmdline")
	used=$(($(jiffies "$p") - ${before[$p]:-0}))
	cpu=$(awk -v u="$used" -v hz="$HZ" -v s="$SAMPLE" 'BEGIN { printf "%.1f", 100*u/hz/s }')

	printf '  %-28s %9s %9s %6s%%\n' "$(role "$cmd")" \
		"$(human "$rss")" "$(human "$pss")" "$cpu"
	rss_total=$((rss_total + rss))
	pss_total=$((pss_total + pss))
	cpu_total=$(awk -v a="$cpu_total" -v b="$cpu" 'BEGIN { print a + b }')
done

echo "  ────────────────────────────────────────────────────────────"
printf '  %-28s %9s %9s %6s%%\n' "$(printf '%d processes' "${#PIDS[@]}")" \
	"$(human "$rss_total")" "$(human "$pss_total")" "$cpu_total"
echo
echo "  PSS is the honest total. The RSS column sums to a much larger number"
echo "  because Chromium's processes share most of their pages, and adding RSS"
echo "  counts that shared memory once per process."
echo
echo "  CPU is % of one core. Anything near 100 is the class of bug that froze a"
echo "  desktop twice (#37, and the edge-versus-level rule in CLAUDE.md)."
echo
echo "  An idle window currently costs ~20% on a 120Hz display and is not a"
echo "  regression: the backdrop drifts on an infinite CSS animation, so the"
echo "  frame clock never stops (#126). GDK_DEBUG=frames counts them."
echo
disk
