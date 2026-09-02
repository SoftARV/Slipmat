# Issue #196 AUR sidecar payload task checklist

- Plan: [`tasks/plan.md`](plan.md)
- Issue: [#196 — AUR packages omit `queue-identity.js`](https://github.com/SoftARV/Slipmat/issues/196)
- Branch: `fix/196-aur-queue-identity`

Status: Approved for implementation on 2026-09-02.

## Task 1: Correct the AUR sidecar payload definitions

**Description:** Install `queue-identity.js` beside `preload.js` in the current
`slipmat-daemon-git` payload. Update the stable definition so a release that
contains the module installs it while the current `v0.10.0` source, which does
not contain or require it, remains buildable.

**Acceptance criteria:**

- [ ] `package_slipmat-daemon-git()` copies `sidecar/queue-identity.js` into
  `/usr/share/slipmat/sidecar/` with the other preload runtime files.
- [ ] The stable `package()` copies the module when it exists in the selected
  release source and does not fail when building `v0.10.0`.
- [ ] No package names, ownership, dependencies, sidecar behavior, or unrelated
  install paths change.

**Verification:**

- [ ] Run `bash -n packaging/aur/slipmat/PKGBUILD packaging/aur/slipmat-git/PKGBUILD`.
- [ ] Run `git diff --check`.
- [ ] Review the focused diff against `Makefile`'s existing sidecar install
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

- [ ] Both package bases build successfully, and the `slipmat-daemon-git`
  archive contains `usr/share/slipmat/sidecar/queue-identity.js`.
- [ ] The current stable archive matches the runtime imports in its `v0.10.0`
  preload; the PKGBUILD is ready to include the module when a release contains
  it.
- [ ] After installing and restarting the rebuilt daemon, Electron reports no
  `Cannot find module ./queue-identity` error and Climat reaches ready or
  signed-out instead of timing out.

**Verification:**

- [ ] Run `(cd packaging/aur/slipmat && makepkg -f)`.
- [ ] Run `(cd packaging/aur/slipmat-git && makepkg -f)`.
- [ ] Inspect the daemon archives with `bsdtar -tf`; confirm the `-git` module
  path and compare each archive with its source preload imports.
- [ ] Install the rebuilt daemon package, restart `slipmatd`, and repeat issue
  #196's logging and Climat startup sequence.
- [ ] Run `make check` and review the final diff for unrelated changes.

**Dependencies:** Task 1.

**Files likely touched:**

- `tasks/plan.md`
- `tasks/todo.md`

**Estimated scope:** Small, verification plus 2 checklist files.

## Checkpoint: Issue #196 ready for review

- [ ] Tasks 1 and 2 meet their acceptance criteria.
- [ ] Both PKGBUILDs are syntax-clean and build from their declared sources.
- [ ] Package inspection proves the affected daemon payload contains the
  preload dependency without breaking the current stable source.
- [ ] Runtime evidence proves Climat no longer reaches the masked MusicKit
  timeout caused by the missing preload module.
- [ ] `make check` passes.
- [ ] No external AUR publication occurred.
- [ ] The human has reviewed and approved the fix before merge or publication.
