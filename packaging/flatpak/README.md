# Flatpak

Slipmat needs libadwaita ≥ 1.8 and GTK ≥ 4.20 — relm4's `gnome_49` feature —
which almost nothing ships yet. Debian stable, Ubuntu 24.04 and Fedora ≤ 42
cannot build it. A Flatpak carries the GNOME 49 runtime with it, so those same
systems can run it.

**Not for Flathub**, and not submitted there. This builds a bundle you install
yourself.

```bash
make flatpak          # build and install locally
make flatpak-bundle   # produces Slipmat-$(uname -m).flatpak
```

The first build needs the toolchain, all of it from Flathub and none of it
needing root:

**Those are build dependencies.** Installing the finished bundle needs only
`org.gnome.Platform//49`, and `make flatpak-bundle` records Flathub inside the
bundle (`--runtime-repo`) so flatpak offers to fetch it. Without that flag the
install stops dead on a machine with no Flathub remote — which is every clean
machine, and none of the ones this was developed on.

```bash
flatpak install --user flathub \
  org.flatpak.Builder \
  org.gnome.Sdk//49 \
  org.freedesktop.Sdk.Extension.rust-stable//25.08 \
  org.electronjs.Electron2.BaseApp//25.08
```

## Files

| | |
| --- | --- |
| `dev.miguelrincon.Slipmat.yml` | the manifest |
| `cargo-sources.json` | 489 crates by hash — generated, committed |
| `generate-sources.sh` | regenerates the above from `Cargo.lock` |
| `electron-shim` | puts zypak where it has to be |

Run `generate-sources.sh` whenever `Cargo.lock` changes. There is no npm
equivalent to regenerate — see below.

## Three things that are not obvious

**zypak wraps `electron`, not the app.** It has to be the *direct* parent of
the Chromium process, and Chromium is the app's grandchild: the launcher starts
the Rust binary, which spawns Electron. Wrapping the launcher leaves the sidecar
aborting on `chrome-sandbox … mode 4755` exactly as if zypak were absent, and
the supervisor restarts it for ever. So `electron-shim` stands where
`electron_binary()` looks and wraps the real binary beside it.

**No npm tree.** The sidecar's only dependency is Electron itself, and the app
runs `node_modules/electron/dist/electron` directly — the other thirteen
packages exist only to *download* that binary. So the castLabs release is a
single pinned archive rather than a generated node-sources list. Bumping
Electron means changing the URL and the `sha256` in the manifest, and nothing
else.

**The build is offline**, because `flatpak-builder` forbids network access
during a build. Every crate is declared with a hash up front.

## Permissions, and why

- `--share=network` — Widevine has no persistent licences on Linux, so playback
  needs a connection every time, and the CDM is fetched on first run.
- `--device=dri` — without it GTK renders in software and the grids scroll badly
  enough to read as a bug in the app.
- `--own-name=…` — Flatpak's bus proxy only lets an app own names matching its
  ID; without this `GtkApplication` cannot register and exits 0 with no window.
- **No `--filesystem`.** Settings, artwork cache, session and the CDM all live
  under the app's own directories.

## The CDM

It is fetched, not bundled — by Chromium's own component updater, into
`~/.var/app/dev.miguelrincon.Slipmat/config/Slipmat/WidevineCdm/`. Nothing
proprietary is redistributed here: Electron is MIT, Slipmat is GPL-3, and the
CDM arrives on the user's machine through their own updater.

This was measured rather than assumed: Slipmat was run inside a real
`org.gnome.Platform//49` sandbox with `--nofilesystem=home` and a private `HOME`
carrying no CDM, and a track played. The original plan deferred Flatpak on the
grounds that a sandboxed component updater was "genuinely hard"; it is not.

## ARM64 acceptance

Use a clean ARM64 Linux VM on Apple Silicon. Confirm `uname -m` prints
`aarch64`; an x86_64 guest under emulation does not test this build.

1. Start from a snapshot with no Chrome installation, Widevine files, or
   Slipmat configuration.
2. Install `Slipmat-aarch64.flatpak`. Confirm both commands name `aarch64`:

   ```bash
   flatpak info --show-ref dev.miguelrincon.Slipmat
   flatpak info --show-runtime dev.miguelrincon.Slipmat
   ```

   The runtime command must report `org.gnome.Platform/aarch64/49`; no x86_64
   runtime should appear in the installation transaction.
3. Inspect both installed executables and confirm `file` reports AArch64:

   ```bash
   deployment=$(flatpak info --show-location dev.miguelrincon.Slipmat)
   file "$deployment/files/bin/slipmat.real" \
     "$deployment/files/share/slipmat/sidecar/node_modules/electron/dist/electron.bin"
   ```

4. Run `flatpak run --env=RUST_LOG=slipmat=debug
   dev.miguelrincon.Slipmat`. Confirm the log reports `widevine ready` and the
   CDM appears below
   `~/.var/app/dev.miguelrincon.Slipmat/config/Slipmat/WidevineCdm/`.
5. Sign in, load the library, and play a protected full-length track with
   audible output. Test play, pause, seek, previous, next, volume, and desktop
   media controls.
6. Open the queue drawer, collapse history, and let a gapless album cross a
   natural boundary. Check the transition log and PipeWire stream, then listen
   for a gap.
7. Restart Slipmat and play another protected track. Confirm the Apple session
   and CDM survived the restart.

Before publishing the ARM64 bundle, inspect its contents and confirm it ships
castLabs Electron without a Widevine binary. Repeat the sign-in and playback
smoke test with the x86_64 bundle after the Electron upgrade.
