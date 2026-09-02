# Issue #196 AUR sidecar payload implementation plan

## Status

Completed and approved on 2026-09-02. Awaiting merge; AUR publication remains
a separate explicit action.

## Source of truth

- Issue: [#196 — AUR packages omit `queue-identity.js` and prevent the
  sidecar preload from loading](https://github.com/SoftARV/Slipmat/issues/196)
- Branch: `fix/196-aur-queue-identity`
- Task checklist: [`tasks/todo.md`](todo.md)

Issue #196 supplies the reproduction, diagnosis, requested behavior, and
runtime success criteria. No separate specification is needed for this focused
packaging fix.

## Overview

Restore the preload dependency in AUR-built daemon payloads. The affected
`slipmat-daemon-git` package must install `queue-identity.js` beside
`preload.js`; the stable package definition must also carry the file when its
selected release contains it without breaking the current `v0.10.0` source,
which predates that module. Then build and inspect both package variants and
exercise Climat against the rebuilt daemon.

## Architecture decisions

1. Change only the two AUR package payload definitions. The sidecar source,
   Rust daemon, IPC protocol, dependencies, and package ownership stay
   unchanged.
2. Add `sidecar/queue-identity.js` directly to the `slipmat-daemon-git` copy
   list because current `main` requires it from `preload.js`.
3. Keep the stable PKGBUILD compatible with `pkgver=0.10.0`. That tag has
   neither the module nor its preload import, so the stable package will copy
   the module only when the selected release source contains it. An
   unconditional copy would make the current stable package fail to build.
4. Use package-archive inspection as the regression check. This is a packaging
   manifest correction, so a new parser, helper, or dependency would add more
   machinery than the fix.
5. Keep AUR publication outside this plan. Local build and runtime verification
   do not authorize `scripts/aur-publish.sh --push` or any other external
   release action.

## Dependency graph

```text
Task 1: Correct both AUR sidecar payload definitions
    |
    v
Task 2: Build, inspect, and exercise the packages
    |
    v
Checkpoint: Issue #196 verified and ready for review
```

The work is sequential: package evidence is meaningful only after both
definitions are corrected.

## Task list

### Phase 1: Package definitions

- [x] Task 1: Correct the AUR sidecar payload definitions.

### Phase 2: Package and runtime proof

- [x] Task 2: Build, inspect, and exercise both AUR package variants.

### Checkpoint: Complete

- [x] Both PKGBUILDs are syntax-clean and build from their declared sources.
- [x] The `slipmat-daemon-git` archive contains
  `/usr/share/slipmat/sidecar/queue-identity.js`.
- [x] The stable `v0.10.0` archive remains buildable without a file its source
  does not contain, and the definition includes the module for the next source
  release that carries it.
- [x] The rebuilt daemon loads the preload without a missing-module error and
  Climat reaches ready or signed-out instead of the MusicKit timeout.
- [x] `make check` passes and the final diff contains no unrelated change.
- [x] The human approves the result before merge or AUR publication.

Detailed acceptance criteria and verification commands live in
[`tasks/todo.md`](todo.md).

## Risks and controls

| Risk | Impact | Control |
|---|---|---|
| The stable PKGBUILD requests a file absent from `v0.10.0` | High | Make the stable install conditional on the selected release source containing the module; build the current stable package. |
| Only the front-end package is inspected in the split build | High | Inspect `slipmat-daemon-git`, which owns `/usr/share/slipmat/sidecar`, rather than `climat-git` or `slipmat-git`. |
| The archive is correct but the installed daemon still uses an old payload | High | Install the rebuilt daemon package, confirm the installed path, and restart the daemon before launching Climat. |
| The existing 60-second MusicKit timeout hides another preload failure | Medium | Run the daemon with Electron logging and check the first preload error as well as Climat's final stage. |
| Local verification accidentally publishes an AUR update | High | Use `makepkg` and archive inspection only; never pass `--push` to the publication script. |

## Verification strategy

Task 1 uses shell syntax and diff checks. Task 2 builds each PKGBUILD from its
declared source, inspects the generated package archives, then verifies the
installed `-git` daemon with the issue's logging environment and Climat flow.

Required checks:

```bash
bash -n packaging/aur/slipmat/PKGBUILD \
  packaging/aur/slipmat-git/PKGBUILD
git diff --check

(cd packaging/aur/slipmat && makepkg -f)
(cd packaging/aur/slipmat-git && makepkg -f)

bsdtar -tf packaging/aur/slipmat-git/slipmat-daemon-git-*.pkg.tar.zst \
  | grep -Fx usr/share/slipmat/sidecar/queue-identity.js

make check
```

The stable archive is also inspected against the direct relative imports in
its own `preload.js`. For `v0.10.0`, absence of `queue-identity.js` is correct
because that source does not import it. Runtime verification follows issue
#196 after installing the rebuilt `slipmat-daemon-git` package.

## Verification evidence

- The stable `0.10.0-1` package built from its declared tag; all 235 release
  tests passed, and its archive contains the three sidecar files imported by
  that release.
- The split `0.10.0.r136.g50088c5-1` package base built from `main`; all 331
  release tests passed, and the daemon archive contains
  `usr/share/slipmat/sidecar/queue-identity.js`.
- `make check` passed on the feature branch.
- Pacman upgraded `slipmat-daemon-git` and `climat-git` together to `r136`.
  The installed module is owned by the daemon package.
- With the issue's Electron logging enabled, the preload harvested the signed-in
  authorization and refreshed 535 songs, 420 albums, 266 artists, and 8
  playlists. Climat connected and rendered the song library without the
  60-second timeout.
- No AUR package was published.

## Definition of done

- Both tasks meet their acceptance criteria.
- Package syntax, builds, archive contents, repository checks, and the Climat
  runtime path are verified.
- No missing-module preload error occurs with the rebuilt `-git` daemon.
- The current stable package still builds, and its definition is ready to copy
  the module once a release contains it.
- No package is published without a separate explicit instruction.
- The human reviews and approves the result before merge.

## Planning evidence and limits

The code graph generation `2026-09-02T16:48:10Z` on branch
`fix/196-aur-queue-identity` was checked at Verify tier. It reported no
recorded coverage gaps for the inspected paths, but PKGBUILD freshness is not
tracked, so both files were read directly. Direct source inspection confirmed
that current `sidecar/preload.js` imports `./queue-identity`, both AUR payload
lists omit the module, and the Makefile install path includes it. Git object
inspection also confirmed that tag `v0.10.0` contains neither
`sidecar/queue-identity.js` nor its preload import. Coverage remains a
best-effort signal; package archives and runtime behavior are the final proof.

## Open questions

None. The plan resolves the stable-tag mismatch while preserving issue #196's
required behavior for affected `-git` installations.
