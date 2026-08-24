// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

mod app;
mod components;
mod library_cache;
mod mpris;
mod notify;
mod session;
mod settings;
mod style;
mod unplayable;

use relm4::RelmApp;
use relm4::gtk;
use tracing_subscriber::EnvFilter;

pub(crate) use slipmat_core::APP_ID;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("slipmat=info")),
        )
        .init();

    // `RelmApp::new` calls `gtk::init()` and — because we enable relm4's
    // `libadwaita` feature — `adw::init()` too. So there's deliberately no adw
    // init here.
    let app = RelmApp::new(APP_ID);
    setup_icon();

    // Load preferences and apply the colour scheme before the window is shown,
    // so there is no flash of the wrong theme. The model owns them from here.
    let settings = settings::Settings::load();
    settings.apply_theme();
    // Before the window exists, so nothing is ever drawn in the wrong accent.
    style::init(
        style::Accent::parse(&settings.accent),
        settings.player_backdrop,
    );
    app.run::<app::AppModel>(settings);
}

/// Point GTK at our icon and name it as the default.
///
/// On Wayland this does **not** put an icon on the window — a client can't set
/// its own toplevel icon there; GNOME Shell takes it from the installed
/// `.desktop`, so only the installed app shows one. Kept because it's the
/// standard idiom, works on X11, and lets a dev build resolve the icon
/// pre-install. Must run after `RelmApp::new`, which initialised GTK.
fn setup_icon() {
    // Only in a dev build. `CARGO_MANIFEST_DIR` is the directory the binary was
    // *compiled* in, which in a package is a build root that will not exist on
    // the machine that runs it — and baking it into a release binary is what
    // makes `makepkg` warn that the package references `$srcdir`.
    #[cfg(debug_assertions)]
    if let Some(display) = relm4::gtk::gdk::Display::default() {
        let theme = gtk::IconTheme::for_display(&display);
        theme.add_search_path(concat!(env!("CARGO_MANIFEST_DIR"), "/data/icons"));
    }
    gtk::Window::set_default_icon_name(APP_ID);
}
