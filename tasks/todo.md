# Issue #196 AUR sidecar payload task checklist

- Plan: [`tasks/plan.md`](plan.md)
- Issue: [#196 — AUR packages omit `queue-identity.js`](https://github.com/SoftARV/Slipmat/issues/196)
- Branch: `fix/196-aur-queue-identity`

Status: Completed and approved on 2026-09-02. Awaiting merge; AUR publication
remains a separate explicit action.

## Task 1: Correct the AUR sidecar payload definitions

**Description:** Install `queue-identity.js` beside `preload.js` in the current
`slipmat-daemon-git` payload. Update the stable definition so a release that
contains the module installs it while the current `v0.10.0` source, which does
not contain or require it, remains buildable.

**Acceptance criteria:**

- [x] `package_slipmat-daemon-git()` copies `sidecar/queue-identity.js` into
  `/usr/share/slipmat/sidecar/` with the other preload runtime files.
- [x] The stable `package()` copies the module when it exists in the selected
  release source and does not fail when building `v0.10.0`.
- [x] No package names, ownership, dependencies, sidecar behavior, or unrelated
  install paths change.

**Verification:**

- [x] Run `bash -n packaging/aur/slipmat/PKGBUILD packaging/aur/slipmat-git/PKGBUILD`.
- [x] Run `git diff --check`.
- [x] Review the focused diff against `Makefile`'s existing sidecar install
  list and the direct imports in `sidecar/preload.js`.

**Dependencies:** None.

**Files likely touched:**

- `packaging/aur/slipmat/PKGBUILD`
- `packaging/aur/slipmat-git/PKGBUILD`

**Estimated scope:** Small, 2 files.

## Task 2: Build, inspect, and exercise both package variants

**Description:** Build the stable and split `-git` package bases from their
declared sources, inspect the daemon payloads, then install the rebuilt
`slipmat-daemon-git` package and repeat issue #196's Climat startup check with
Electron logging enabled.

**Acceptance criteria:**

- [x] Both package bases build successfully, and the `slipmat-daemon-git`
  archive contains `usr/share/slipmat/sidecar/queue-identity.js`.
- [x] The current stable archive matches the runtime imports in its `v0.10.0`
  preload; the PKGBUILD is ready to include the module when a release contains
  it.
- [x] After installing and restarting the rebuilt daemon, Electron reports no
  `Cannot find module ./queue-identity` error and Climat reaches ready or
  signed-out instead of timing out.

**Verification:**

- [x] Build `packaging/aur/slipmat/PKGBUILD` with `makepkg -f` in a standalone
  temporary directory.
- [x] Build `packaging/aur/slipmat-git/PKGBUILD` with `makepkg -f` in a
  standalone temporary directory.
- [x] Inspect the daemon archives with `bsdtar -tf`; confirm the `-git` module
  path and compare each archive with its source preload imports.
- [x] Install the rebuilt daemon package, restart `slipmatd`, and repeat issue
  #196's logging and Climat startup sequence.
- [x] Run `make check` and review the final diff for unrelated changes.

**Dependencies:** Task 1.

**Files likely touched:**

- `tasks/plan.md`
- `tasks/todo.md`

**Estimated scope:** Small, verification plus 2 checklist files.

## Checkpoint: Issue #196 ready for review

- [x] Tasks 1 and 2 meet their acceptance criteria.
- [x] Both PKGBUILDs are syntax-clean and build from their declared sources.
- [x] Package inspection proves the affected daemon payload contains the
  preload dependency without breaking the current stable source.
- [x] Runtime evidence proves Climat no longer reaches the masked MusicKit
  timeout caused by the missing preload module.
- [x] `make check` passes.
- [x] No external AUR publication occurred.
- [x] The human has reviewed and approved the fix before merge or publication.
