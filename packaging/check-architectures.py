#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Miguel Rincon
# SPDX-License-Identifier: GPL-3.0-or-later
"""Check the public packaging targets for each supported architecture."""

import subprocess
import sys
from pathlib import Path

root = Path(__file__).resolve().parents[1]
failures = []

for arch in ("x86_64", "aarch64"):
    result = subprocess.run(
        ["make", "--dry-run", "flatpak-bundle", f"FLATPAK_ARCH={arch}"],
        cwd=root,
        capture_output=True,
        text=True,
    )
    output = result.stdout + result.stderr
    expected = (f"--arch={arch}", f"Slipmat-{arch}.flatpak")
    missing = [value for value in expected if value not in output]
    if result.returncode:
        failures.append(f"{arch}: Flatpak target failed")
    elif missing:
        failures.append(f"{arch}: missing {', '.join(missing)}")

result = subprocess.run(
    ["makepkg", "--printsrcinfo"],
    cwd=root / "packaging" / "aur" / "slipmat-git",
    capture_output=True,
    text=True,
)
if result.returncode or "\tarch = aarch64" not in result.stdout:
    failures.append("slipmat-git: generated metadata does not support aarch64")

if failures:
    print("Packaging architecture contract failed:")
    for failure in failures:
        print(f"  {failure}")
    sys.exit(1)
