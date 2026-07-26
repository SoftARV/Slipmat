<!--
SPDX-FileCopyrightText: 2026 Miguel Rincon
SPDX-License-Identifier: GPL-3.0-or-later
-->

# AUR packaging

Two packages, both built from this tree:

| Directory | Package | Builds from |
| --- | --- | --- |
| `tonearm/` | `tonearm` | the `v0.1.0` release tarball |
| `tonearm-git/` | `tonearm-git` | the latest commit on `main` |

They conflict with each other, as the two conventions require.

## Why the PKGBUILDs live here

An AUR package **is** its own git repository, hosted on `aur.archlinux.org` and
holding exactly two tracked files — `PKGBUILD` and `.SRCINFO`. It is not a
repository you create on GitHub.

But the PKGBUILD has to change in lockstep with the code: a new dependency, a
moved file, a renamed make target all break it. A PKGBUILD living only in a
repository nobody touches while developing goes stale silently, and the first
sign is a stranger's build failing. So this tree holds the source of truth and
the AUR repository is a publishing target.

Nothing here has been pushed to the AUR yet.

## Publishing, when the time comes

```bash
git clone ssh://aur@aur.archlinux.org/tonearm.git aur-tonearm
cp packaging/aur/tonearm/PKGBUILD aur-tonearm/
cd aur-tonearm && makepkg --printsrcinfo > .SRCINFO
git commit -am "…" && git push
```

`.SRCINFO` is generated, never hand-written, and the AUR rejects a push without
one.

## Things that are true of these builds

**The Widevine CDM is not in the package.** `wvcus` fetches it through
Chromium's own component updater at first run, into `~/.config/Tonearm/`. What
ships is Electron (MIT) and Tonearm (GPL-3), so there is no proprietary
redistribution — which is what makes packaging this possible at all.

**The sidecar goes to `/usr/share/tonearm/sidecar`,** which
`player::sidecar::locate` finds through `XDG_DATA_DIRS`. That search path was
added *for* packaging: before it, only `~/.local/share` and the dev tree were
checked, and a system install would have been invisible to the binary that
needed it.

**`prepare()` uses the network,** for both the crate registry and the ~200 MB
Electron download. Unavoidable: castLabs ships only as a GitHub release, and
`sidecar/.npmrc` carries the `allow-git=root` that npm 12 requires to accept it.

**`tonearm` warns about `$srcdir` until 0.2.0.** The v0.1.0 binary baked
`CARGO_MANIFEST_DIR` into itself through two dev-only fallbacks. Both are gated
behind `debug_assertions` now, so the warning clears with the next tag —
`tonearm-git` is already clean.

**Foreign-architecture binaries are pruned.** One npm dependency ships 7 MB of
prebuilt `.node` files for arm64, ia32, darwin and win32. `package()` deletes
everything that is not `linux-x64-gnu`; they were noticed because `strip`
complained about them.

## Building one locally

```bash
cd packaging/aur/tonearm && makepkg -f
```

Needs the `rust` package rather than a rustup toolchain — makepkg resolves
`cargo` through pacman. With rustup, `--nodeps` builds fine but skips the check.
