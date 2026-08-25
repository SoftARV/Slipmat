#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Miguel Rincon
# SPDX-License-Identifier: GPL-3.0-or-later
"""Is every locked crate vendored for the offline Flatpak build?

`flatpak-builder` forbids network access, so a crate missing from
`cargo-sources.json` is not a slow build — it is `no matching package named X`
and a failed release. Adding a dependency and forgetting to regenerate is the
only way that happens, and it is easy to do.

**This runs in `make check` because the Flatpak job does not run on pull
requests.** It only builds on pushes to `main`, so without this the first sign
of a stale source list is a red X on an already-merged commit — which is
exactly how it was found. Parsing two files catches it in seconds instead.

No network and no dependencies: it compares names, not hashes. A wrong hash is
a different failure and one the real build will catch.
"""

import re
import sys
from pathlib import Path

root = Path(__file__).resolve().parents[2]
lock = (root / "Cargo.lock").read_text()
sources = root / "packaging" / "flatpak" / "cargo-sources.json"
blob = sources.read_text()

missing = []
for block in lock.split("[[package]]")[1:]:
    name = re.search(r'^name = "(.+)"$', block, re.M)
    version = re.search(r'^version = "(.+)"$', block, re.M)
    # No `source` means a path dependency — one of our own crates, built from
    # the tree rather than fetched, so it is never in the vendor list.
    if not (name and version and re.search(r"^source = ", block, re.M)):
        continue
    name, version = name.group(1), version.group(1)
    if f"/{name}/{name}-{version}.crate" not in blob and f'"{name}-{version}"' not in blob:
        missing.append(f"{name} {version}")

if missing:
    print(f"{sources.relative_to(root)} is stale — {len(missing)} crate(s) missing:")
    for one in missing:
        print(f"  {one}")
    print("\nRegenerate it:  packaging/flatpak/generate-sources.sh")
    sys.exit(1)
