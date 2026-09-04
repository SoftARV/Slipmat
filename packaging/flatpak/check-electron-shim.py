#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Miguel Rincon
# SPDX-License-Identifier: GPL-3.0-or-later
"""Check that the Flatpak Electron shim selects an available display."""

import os
import socket
import subprocess
import sys
import tempfile
from pathlib import Path

shim = Path(__file__).with_name("electron-shim")


def run(env: dict[str, str]) -> subprocess.CompletedProcess[str]:
    with tempfile.TemporaryDirectory() as directory:
        fake = Path(directory) / "zypak-wrapper"
        fake.write_text("#!/bin/sh\nprintf '%s\\n' \"$@\"\n")
        fake.chmod(0o755)
        return subprocess.run(
            [shim, "."],
            env={"PATH": f"{directory}:{os.environ['PATH']}", **env},
            capture_output=True,
            text=True,
        )


with tempfile.TemporaryDirectory() as runtime:
    wayland = socket.socket(socket.AF_UNIX)
    wayland.bind(f"{runtime}/wayland-test")
    result = run({"XDG_RUNTIME_DIR": runtime, "WAYLAND_DISPLAY": "wayland-test"})
    wayland.close()

if result.returncode or "--ozone-platform=wayland" not in result.stdout:
    print("Electron shim did not select Wayland", file=sys.stderr)
    sys.exit(1)

result = run({"DISPLAY": ":0"})
if result.returncode or "--ozone-platform=x11" not in result.stdout:
    print("Electron shim did not select X11", file=sys.stderr)
    sys.exit(1)

result = run({})
if result.returncode == 0 or "no usable Wayland or X11 display" not in result.stderr:
    print("Electron shim did not reject a missing display", file=sys.stderr)
    sys.exit(1)
