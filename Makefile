# Tonearm — build and install to a personal (per-user) prefix.
#
# No sudo: this is a one-user, one-machine app (see CLAUDE.md), so everything
# lands under ~/.local, which is already on PATH and XDG_DATA_DIRS. Override
# PREFIX for a system install (make PREFIX=/usr/local install, with sudo).
#
# Unlike its siblings, Tonearm installs a *second* artefact: the Electron
# sidecar that owns DRM playback. It is ~200 MB of Chromium and is fetched by
# npm, never committed. The binary finds it via TONEARM_SIDECAR, else
# $(DATADIR)/tonearm/sidecar, else ./sidecar for a dev tree.

PREFIX  ?= $(HOME)/.local
BINDIR   = $(PREFIX)/bin
DATADIR  = $(PREFIX)/share
APPID    = dev.miguelrincon.Tonearm
SIDECAR  = $(DATADIR)/tonearm/sidecar

ICON_SIZES = 16 32 48 64 128 256 512

.PHONY: all build run test check sizes sidecar sidecar-run gapless install install-sidecar \
        dev-install uninstall clean flatpak flatpak-bundle
all: build

build:
	cargo build --release

run:
	cargo run

test:
	cargo test

# The bar from CLAUDE.md. --all-targets so tests are linted too.
check: sizes
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo test

# A size budget, enforced as a ratchet. First, because it is instant and the
# thing it catches is drift you would otherwise only notice months later.
sizes:
	@./scripts/check-sizes.sh

# Fetch castLabs Electron. Two steps, both required: `npm install` brings down
# the ~14 MB wrapper, and install.js fetches the ~200 MB Chromium itself.
# castLabs ships no postinstall hook, so skipping the second step leaves you
# with a package that has no binary in it.
# (The Widevine CDM itself arrives later, at first run, via Chromium's
# component updater — that needs network too.)
sidecar:
	cd sidecar && npm install && node node_modules/electron/install.js

# Run the sidecar standalone with its window visible — the isolation step from
# CLAUDE.md. If a track plays here, DRM is fine and the bug is in the Rust side.
sidecar-run: sidecar
	cd sidecar && npm run debug

# Watch the audio stream across a track boundary. Run it in one terminal and
# `RUST_LOG=tonearm=info cargo run` in another — the log says whether Rust
# drove the transition, this says whether the decoder stopped.
gapless:
	./scripts/gapless-check.sh

flatpak:
	flatpak run org.flatpak.Builder --force-clean --user --install \
		--repo=flatpak-repo build-dir packaging/flatpak/dev.miguelrincon.Tonearm.yml

flatpak-bundle: flatpak
	flatpak build-bundle flatpak-repo Tonearm.flatpak dev.miguelrincon.Tonearm master
	@echo "Tonearm.flatpak — copy it anywhere and: flatpak install ./Tonearm.flatpak"

install: build install-sidecar dev-install
	install -Dm755 target/release/tonearm $(BINDIR)/tonearm
	@echo "Installed to $(PREFIX). Launch 'Tonearm' from the app grid, or run 'tonearm'."

install-sidecar: sidecar
	install -d $(SIDECAR)
	cp -r sidecar/package.json sidecar/main.js sidecar/preload.js \
		sidecar/node_modules $(SIDECAR)/

# Everything except the binaries: the .desktop entry and the icons.
# Not a way to get a dev-mode icon — on Wayland only the fully installed app
# shows one.
dev-install:
	install -Dm644 data/$(APPID).desktop $(DATADIR)/applications/$(APPID).desktop
	install -Dm644 data/icons/hicolor/scalable/apps/$(APPID).svg \
		$(DATADIR)/icons/hicolor/scalable/apps/$(APPID).svg
	install -Dm644 data/icons/hicolor/symbolic/apps/$(APPID)-symbolic.svg \
		$(DATADIR)/icons/hicolor/symbolic/apps/$(APPID)-symbolic.svg
	@# Raster sizes, rendered from the same SVG the app installs so the two
	@# can never drift. GTK resolves the SVG on its own, but the shell, the
	@# notification daemon and anything reading the icon theme without an SVG
	@# loader all want PNGs — and this loop used to look for files that were
	@# never in the tree, so it silently installed none.
	@if command -v rsvg-convert >/dev/null 2>&1; then \
		for sz in $(ICON_SIZES); do \
			install -d $(DATADIR)/icons/hicolor/$${sz}x$${sz}/apps; \
			rsvg-convert -w $${sz} -h $${sz} \
				data/icons/hicolor/scalable/apps/$(APPID).svg \
				-o $(DATADIR)/icons/hicolor/$${sz}x$${sz}/apps/$(APPID).png; \
		done; \
		echo "Rendered PNG icons: $(ICON_SIZES)"; \
	else \
		echo "rsvg-convert not found — installing the SVG only."; \
	fi
	@if [ -f $(DATADIR)/icons/hicolor/index.theme ]; then \
		touch $(DATADIR)/icons/hicolor; \
		gtk-update-icon-cache -q -t -f $(DATADIR)/icons/hicolor; \
	fi
	-update-desktop-database -q $(DATADIR)/applications

uninstall:
	rm -f $(BINDIR)/tonearm
	rm -rf $(DATADIR)/tonearm
	rm -f $(DATADIR)/applications/$(APPID).desktop
	rm -f $(DATADIR)/icons/hicolor/scalable/apps/$(APPID).svg
	rm -f $(DATADIR)/icons/hicolor/symbolic/apps/$(APPID)-symbolic.svg
	@for sz in $(ICON_SIZES); do \
		rm -f $(DATADIR)/icons/hicolor/$${sz}x$${sz}/apps/$(APPID).png; \
	done
	@if [ -f $(DATADIR)/icons/hicolor/index.theme ]; then \
		gtk-update-icon-cache -q -t -f $(DATADIR)/icons/hicolor; \
	fi
	-update-desktop-database -q $(DATADIR)/applications
	@echo "Uninstalled from $(PREFIX)."

clean:
	cargo clean
	rm -rf sidecar/node_modules
