// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

mod app;
mod components;
mod mpris;
mod music;
mod notify;
mod player;
mod settings;
mod unplayable;

use relm4::RelmApp;
use relm4::gtk;
use relm4::gtk::gdk;
use tracing_subscriber::EnvFilter;

pub(crate) const APP_ID: &str = "dev.miguelrincon.Tonearm";

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("tonearm=info")),
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
    if let Some(display) = gdk::Display::default() {
        let theme = gtk::IconTheme::for_display(&display);
        theme.add_search_path(concat!(env!("CARGO_MANIFEST_DIR"), "/data/icons"));
    }
    gtk::Window::set_default_icon_name(APP_ID);
}
