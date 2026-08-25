#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Miguel Rincon
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Publish a package to the AUR from packaging/aur/, which stays the source of
# truth. An AUR package *is* a git repository holding a handful of files; this
# clones it, copies ours in, regenerates .SRCINFO, and pushes.
#
# Dry run by default. Nothing leaves this machine without --push, because the
# push is what makes a package public and there is no undo.
#
# Deliberately **not** a CI job. Doing this from Actions would mean keeping a
# key that can publish arbitrary code under your name in a repository secret,
# and the AUR guidelines are pointed about automated updates being at the
# maintainer's own risk.
set -euo pipefail

pkg=${1:-}
push=${2:-}
repo=$(git rev-parse --show-toplevel)
src="$repo/packaging/aur/$pkg"

if [[ ! -d $src ]]; then
    # The argument is a **package base**, which is the directory here and the
    # repository on the AUR. `slipmat-git` is a split base: one push publishes
    # slipmat-daemon-git, slipmat-git and climat-git together.
    echo "usage: $0 <slipmat|slipmat-git> [--push]" >&2
    exit 2
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

echo "==> cloning $pkg"
# `master` explicitly: this repository's default branch is `main`, and the AUR
# accepts pushes to master alone. Getting this wrong fails at the push, after
# everything else has already been done.
git -c init.defaultBranch=master clone --quiet \
    "ssh://aur@aur.archlinux.org/$pkg.git" "$work/$pkg"
cd "$work/$pkg"

cp "$src/PKGBUILD" "$src/LICENSE" .

# The guidelines' own tip: exclude everything, force-add what belongs. It is
# what stops a stray source tarball or pkg/ directory being committed — both of
# which makepkg leaves in the working directory.
printf '%s\n' '*' '!PKGBUILD' '!.SRCINFO' '!LICENSE' '!.gitignore' > .gitignore

# **A VCS package publishes the version it would build to, not the placeholder.**
# `pkgver=…r0.g0000000` in the tree is a stand-in that `pkgver()` replaces at
# build time, and pushing it would replace a real version on the AUR with
# something that reads as broken. Helpers cope either way — they run `pkgver()`
# themselves — but the package page is what a person looks at.
#
# `--noprepare` is what makes this affordable: `prepare()` runs `npm install`
# and fetches ~200 MB of Electron, and none of it is needed to answer "what
# would this build call itself". What is left is a source clone and
# `git describe`. `--nodeps` because nothing is being compiled.
#
# It rewrites the working copy, never the tree's own PKGBUILD — makepkg runs
# here, on the copy.
if [[ $pkg == *-git ]]; then
    echo "==> computing the version this would build to"
    makepkg --noprepare --nobuild --nodeps --noconfirm >/dev/null
fi

# Regenerated every time, never hand-written. A push without it, or with one
# that disagrees with the PKGBUILD, is rejected by the AUR's own hook.
makepkg --printsrcinfo > .SRCINFO
git add -f PKGBUILD .SRCINFO LICENSE .gitignore

if git diff --cached --quiet; then
    echo "==> nothing to publish; $pkg is already up to date"
    exit 0
fi

# **A VCS package must not get a bare pkgver bump.** It is not out of date when
# upstream gains commits — pkgver() computes the version at build time — so a
# commit that changes nothing else is churn the guidelines ask maintainers not
# to create. Note makepkg *rewrites* pkgver in place after a build, so this is
# easy to do by accident rather than by intent.
if [[ $pkg == *-git ]]; then
    substantive=$(git diff --cached -U0 -- PKGBUILD |
        grep -E '^[-+]' | grep -vE '^[-+]{3}' | grep -vcE '^[-+]pkgver=' || true)
    if [[ $substantive -eq 0 ]]; then
        echo "==> only pkgver changed in a -git package — nothing worth a commit"
        exit 0
    fi
fi

# **The base has to match the repository name**, because the AUR keys a
# repository on `pkgbase` and rejects a push where they disagree — at the push,
# after everything else is already done, which is the same late failure the
# `master` rule above exists to avoid.
base=$(sed -n 's/^pkgbase = //p' .SRCINFO)
if [[ $base != "$pkg" ]]; then
    echo "==> pkgbase is '$base' but this is the '$pkg' repository; the AUR would reject it" >&2
    exit 1
fi

echo "==> what would be published"
git diff --cached --stat | sed 's/^/    /'
echo
# **`pkgname` sits at column 0 and the rest are tab-indented**, so a single
# leading-whitespace pattern silently drops exactly the lines that matter most:
# a split base publishes several packages, and which ones is the thing to read
# before pushing. Two patterns rather than one clever one.
grep -E '^pkgname = ' .SRCINFO | sed 's/^/    /'
grep -E '^\s+(pkgver|pkgrel|source|sha256sums) ' .SRCINFO | sed 's/^/    /'

version=$(sed -n 's/^\tpkgver = //p' .SRCINFO)
release=$(sed -n 's/^\tpkgrel = //p' .SRCINFO)

if [[ $push != "--push" ]]; then
    echo
    echo "==> dry run. Re-run with --push to publish $pkg $version-$release"
    exit 0
fi

git commit --quiet -m "$pkg $version-$release"
git push --quiet origin HEAD:master
echo "==> published https://aur.archlinux.org/packages/$pkg"
