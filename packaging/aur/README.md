<!--
SPDX-FileCopyrightText: 2026 Miguel Rincon
SPDX-License-Identifier: GPL-3.0-or-later
-->

# AUR packaging

Two packages, both built from this tree:

| Directory | Package | Builds from |
| --- | --- | --- |
| `slipmat/` | `slipmat` | the `v0.3.0` release tarball |
| `slipmat-git/` | `slipmat-git` | the latest commit on `main` |

They conflict with each other, as the two conventions require.

**`v0.3.0` is the first tag under this name, and the release package needed
it.** Between the rename and that tag `slipmat/PKGBUILD` was pinned to
`v0.2.0`, whose tree still builds a binary called `tonearm` — `package()` would
have failed looking for `target/release/slipmat`. The recorded `sha256sum` was
wrong for a second reason worth remembering: **a GitHub archive names its root
directory after the repository as it stands now**, so renaming the repository
changed the tarball's bytes even though the tag itself never moved, and
`cd "Slipmat-$pkgver"` only started matching once both had happened.

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
git clone ssh://aur@aur.archlinux.org/slipmat.git aur-slipmat
cp packaging/aur/slipmat/PKGBUILD aur-slipmat/
cd aur-slipmat && makepkg --printsrcinfo > .SRCINFO
git commit -am "…" && git push
```

`.SRCINFO` is generated, never hand-written, and the AUR rejects a push without
one.

## Things that are true of these builds

**The Widevine CDM is not in the package.** `wvcus` fetches it through
Chromium's own component updater at first run, into `~/.config/Slipmat/`. What
ships is Electron (MIT) and Slipmat (GPL-3), so there is no proprietary
redistribution — which is what makes packaging this possible at all.

**The sidecar goes to `/usr/share/slipmat/sidecar`,** which
`player::sidecar::locate` finds through `XDG_DATA_DIRS`. That search path was
added *for* packaging: before it, only `~/.local/share` and the dev tree were
checked, and a system install would have been invisible to the binary that
needed it.

**`prepare()` uses the network,** for both the crate registry and the ~200 MB
Electron download. Unavoidable: castLabs ships only as a GitHub release, and
`sidecar/.npmrc` carries the `allow-git=root` that npm 12 requires to accept it.

**Both packages build clean as of v0.1.1.** v0.1.0 could not be packaged at
all: its sidecar search never looked at `XDG_DATA_DIRS`, so the binary could not
find the sidecar installed beside it, and it baked its own build directory in —
which is what `makepkg` was reporting as a reference to `$srcdir`. Both were
fixed in 0.1.1, and that release exists because of this packaging work.

**Foreign-architecture binaries are pruned.** One npm dependency ships 7 MB of
prebuilt `.node` files for arm64, ia32, darwin and win32. `package()` deletes
everything that is not `linux-x64-gnu`; they were noticed because `strip`
complained about them.

## Building one locally

```bash
cd packaging/aur/slipmat && makepkg -f
```

Needs the `rust` package rather than a rustup toolchain — makepkg resolves
`cargo` through pacman. With rustup, `--nodeps` builds fine but skips the check.
