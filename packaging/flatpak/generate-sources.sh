#!/usr/bin/env bash
# Regenerate the offline source list for the Flatpak build.
#
# `flatpak-builder` forbids network access during a build, so every crate has
# to be declared up front with a hash. This turns Cargo.lock into that list.
#
# Run it whenever Cargo.lock changes. There is no npm equivalent to generate:
# the sidecar's only dependency is Electron itself, pinned as an archive in the
# manifest, and nothing in the npm tree is needed at runtime.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../.." && pwd)"

gen="$here/.flatpak-cargo-generator.py"
if [ ! -f "$gen" ]; then
  echo "fetching flatpak-cargo-generator…"
  curl -sSfL -o "$gen" \
    https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/master/cargo/flatpak-cargo-generator.py
fi

python3 "$gen" "$repo/Cargo.lock" -o "$here/cargo-sources.json"
echo "wrote $here/cargo-sources.json ($(wc -c < "$here/cargo-sources.json") bytes)"
