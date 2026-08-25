<!--
SPDX-FileCopyrightText: 2026 Miguel Rincon
SPDX-License-Identifier: GPL-3.0-or-later
-->

# AUR packaging

Two package *bases*, both built from this tree — and the `-git` one is a
**split package** that produces three:

| Directory | Builds | Builds from |
| --- | --- | --- |
| `slipmat/` | `slipmat` | the `v0.10.0` release tarball |
| `slipmat-git/` | `slipmat-daemon-git`, `slipmat-git`, `climat-git` | the latest commit on `main` |

The release and `-git` lines conflict with each other, as the two conventions
require.

## Why the `-git` base is split

**The engine is not the interface.** The daemon owns the Chromium sidecar, and
the Chromium profile lock means exactly one process may — so a machine has one
daemon and as many front-ends as it likes. Shipping the daemon inside each
front-end would put the same ~220 MB of Electron at the same paths in two
packages, which pacman refuses and which would be wrong even if it did not:
they are one installation.

| Package | Holds | Depends on |
| --- | --- | --- |
| `slipmat-daemon-git` | `slipmatd`, the sidecar, the optional systemd unit | — |
| `slipmat-git` | the GNOME app, desktop file, icons | the daemon |
| `climat-git` | the terminal player | the daemon |

Installing either front-end pulls the daemon in on its own, so each behaves as
though it were self-contained, and both can be installed together sharing one
copy of it.

**`slipmat-git` shipped without `slipmatd` for a while and nobody noticed**,
because the package predating the daemon still worked: the app spawned the
sidecar itself. After the switchover the app is a *client*, and a build from
`main` installed a window that sits on "Connecting" forever, redialling a
daemon that was never packaged. The Flatpak manifest had the identical hole. A
front-end package that does not pull in an engine is the thing to check first
whenever the architecture moves.

**The release package still needs this treatment.** `v0.10.0` predates the
daemon, so `slipmat/PKGBUILD` is correct for the tag it builds — it stops being
correct the moment a release contains `slipmatd`.

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

Both are published:

- <https://aur.archlinux.org/packages/slipmat>
- <https://aur.archlinux.org/packages/slipmat-git>

## Publishing

```bash
make aur           # dry run: what would change, for both packages
make aur-publish   # the same with --push
```

`scripts/aur-publish.sh` clones the AUR repo, copies `PKGBUILD` and `LICENSE`
in, regenerates `.SRCINFO`, and pushes to `master`. Dry run by default, because
a push is public and there is no undo.

It encodes the four rules that fail in ways worth not rediscovering:

- **`master` only.** This repository's default branch is `main`, so the clone
  pins `master` explicitly. Otherwise it fails at the push, after everything
  else has already been done.
- **`.SRCINFO` is regenerated every time.** The AUR's own hook rejects a push
  without one, or with one that disagrees with the `PKGBUILD`.
- **No bare `pkgver` bumps to `slipmat-git`.** A VCS package is not out of date
  when upstream gains commits. The script refuses a commit where nothing but
  `pkgver` moved — which matters because **`makepkg` rewrites `pkgver` in place
  after a build**, so it happens by accident rather than intent.
- **A `.gitignore` that excludes everything**, force-adding the four files. It
  is what stops a stray source tarball or `pkg/` directory being committed.

**Not a CI job, deliberately.** Publishing from Actions would mean keeping a key
that can push arbitrary code under your name in a repository secret, and the
guidelines are pointed about automated updates being at the maintainer's own
risk and malfunctioning accounts being removed without notice.

After tagging a release, update `slipmat/PKGBUILD` with the new `pkgver` and the
`sha256sum` **of the tarball GitHub serves** — download it rather than computing
one locally, because a GitHub archive names its root directory after the
repository and the bytes differ.

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
