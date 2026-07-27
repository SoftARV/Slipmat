#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Miguel Rincon
# SPDX-License-Identifier: GPL-3.0-or-later
#
# A size budget for source files, enforced as a ratchet.
#
# Long files are not a bug in themselves. The reason this exists is that the
# rule against them was already written down — "a new `impl AppModel` method
# goes in the sibling that owns its concern" — and `app/mod.rs` still grew from
# a post-split 1500 lines back past 2800 before anyone looked. A rule nobody can
# see is not a rule, which is the same argument behind `clippy -D warnings`
# being the bar rather than a preference.
#
# How it works:
#
#   * anything over BUDGET must be listed in the exceptions file, with the size
#     it is allowed to reach;
#   * a listed file that grows past its recorded size fails.
#
# So a new file cannot quietly become the next `mod.rs`, and the ones already
# over can only shrink. Raising an entry is allowed — it just has to be a
# deliberate line in a diff rather than an accident nobody noticed.

set -uo pipefail
cd "$(dirname "$0")/.."

BUDGET=600
EXCEPTIONS=scripts/size-exceptions.txt

fail=0
listed() { grep -E "^$1[[:space:]]" "$EXCEPTIONS" 2>/dev/null | awk '{print $2}'; }

while read -r count path; do
	[ "$path" = total ] && continue
	allowed=$(listed "$path")

	if [ -z "$allowed" ]; then
		if [ "$count" -gt "$BUDGET" ]; then
			printf '  %5d  %-34s over the %d-line budget and not recorded\n' \
				"$count" "$path" "$BUDGET"
			fail=1
		fi
		continue
	fi

	if [ "$count" -gt "$allowed" ]; then
		printf '  %5d  %-34s grew past its recorded %d\n' "$count" "$path" "$allowed"
		fail=1
	fi
done < <(find src sidecar -name '*.rs' -o -name '*.js' \
	| grep -v node_modules | xargs wc -l | sort -rn)

if [ "$fail" -eq 0 ]; then
	echo "  sizes: within budget"
	exit 0
fi

cat <<'MSG'

  Split it, or record the new size in scripts/size-exceptions.txt with a reason.
  Recording is a legitimate answer — `view!` and a reducer are long by nature.
  Doing it silently is not, which is the whole point of the file.
MSG
exit 1
