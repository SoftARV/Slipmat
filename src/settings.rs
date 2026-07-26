// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Persistent **preferences**, in `~/.config/tonearm/settings.ini`.
//!
//! Preferences only. Tokens live in the keyring and are re-harvested every
//! launch (CLAUDE.md rule 7) — nothing secret goes in this file, ever. If you
//! find yourself adding a field whose value would be embarrassing in a
//! plain-text file under `~/.config`, it belongs somewhere else.
//!
//! A missing or corrupt file is not an error: it means defaults. This is a
//! single-user app on one machine, and refusing to start because an ini file
//! got mangled would be absurd.

use relm4::gtk::glib::{self, KeyFile, KeyFileFlags};

const GROUP: &str = "Tonearm";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

impl Theme {
    fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "light" => Self::Light,
            "dark" => Self::Dark,
            _ => Self::System,
        }
    }

    /// Index in the Preferences combo row, and back.
    pub fn from_index(i: u32) -> Self {
        match i {
            1 => Self::Light,
            2 => Self::Dark,
            _ => Self::System,
        }
    }

    pub fn index(self) -> u32 {
        match self {
            Self::System => 0,
            Self::Light => 1,
            Self::Dark => 2,
        }
    }
}

/// Which sidebar section the app opens on. Persisted, so it reopens where you
/// left it rather than always on the same one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Section {
    #[default]
    Library,
    Albums,
    Artists,
    Catalog,
}

impl Section {
    fn as_str(self) -> &'static str {
        match self {
            Self::Library => "library",
            Self::Albums => "albums",
            Self::Artists => "artists",
            Self::Catalog => "catalog",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "catalog" => Self::Catalog,
            "albums" => Self::Albums,
            "artists" => Self::Artists,
            _ => Self::Library,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub theme: Theme,
    pub section: Section,
    pub show_sidebar: bool,
    /// Notify when the track changes. Off by default (`bool`'s default): a
    /// notification for every song is a lot of noise, and the person who wants
    /// it will go and turn it on.
    pub notify_track_change: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: Theme::default(),
            section: Section::default(),
            // Visible on a first run: a sidebar nobody has hidden yet should
            // be there to be found.
            show_sidebar: true,
            notify_track_change: false,
        }
    }
}

fn path() -> Option<std::path::PathBuf> {
    let dir = glib::user_config_dir().join("tonearm");
    Some(dir.join("settings.ini"))
}

impl Settings {
    /// Read preferences, falling back to defaults for anything missing.
    pub fn load() -> Self {
        let mut settings = Self::default();
        let Some(path) = path() else {
            return settings;
        };

        let file = KeyFile::new();
        if file.load_from_file(&path, KeyFileFlags::NONE).is_err() {
            // No file yet, or unreadable. Defaults, quietly — this is the
            // normal first-run path, not a failure.
            return settings;
        }

        if let Ok(theme) = file.string(GROUP, "theme") {
            settings.theme = Theme::parse(&theme);
        }
        if let Ok(notify) = file.boolean(GROUP, "notify-track-change") {
            settings.notify_track_change = notify;
        }
        if let Ok(section) = file.string(GROUP, "section") {
            settings.section = Section::parse(&section);
        }
        if let Ok(show) = file.boolean(GROUP, "show-sidebar") {
            settings.show_sidebar = show;
        }
        tracing::debug!(?settings, "loaded settings");
        settings
    }

    /// Write preferences. Best-effort: failing to save a preference must never
    /// interrupt playback.
    pub fn save(&self) {
        let Some(path) = path() else {
            return;
        };
        let Some(dir) = path.parent() else {
            return;
        };

        let file = KeyFile::new();
        file.set_string(GROUP, "theme", self.theme.as_str());
        file.set_boolean(GROUP, "notify-track-change", self.notify_track_change);
        file.set_string(GROUP, "section", self.section.as_str());
        file.set_boolean(GROUP, "show-sidebar", self.show_sidebar);

        if let Err(err) = std::fs::create_dir_all(dir) {
            tracing::warn!(?err, "could not create config directory");
            return;
        }
        if let Err(err) = file.save_to_file(&path) {
            tracing::warn!(?err, "could not save settings");
        }
    }

    /// Apply the colour scheme. Called at startup before the window is shown,
    /// so there is no flash of the wrong theme, and again whenever it changes.
    pub fn apply_theme(&self) {
        let manager = relm4::adw::StyleManager::default();
        manager.set_color_scheme(match self.theme {
            Theme::System => relm4::adw::ColorScheme::Default,
            Theme::Light => relm4::adw::ColorScheme::ForceLight,
            Theme::Dark => relm4::adw::ColorScheme::ForceDark,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_round_trips_through_its_string_form() {
        for theme in [Theme::System, Theme::Light, Theme::Dark] {
            assert_eq!(Theme::parse(theme.as_str()), theme);
        }
    }

    #[test]
    fn an_unknown_theme_falls_back_to_system() {
        // A hand-edited or future-version ini must not break startup.
        assert_eq!(Theme::parse("solarized"), Theme::System);
        assert_eq!(Theme::parse(""), Theme::System);
    }

    #[test]
    fn theme_round_trips_through_its_combo_index() {
        for theme in [Theme::System, Theme::Light, Theme::Dark] {
            assert_eq!(Theme::from_index(theme.index()), theme);
        }
    }

    #[test]
    fn an_out_of_range_index_falls_back_to_system() {
        assert_eq!(Theme::from_index(99), Theme::System);
    }

    #[test]
    fn section_round_trips_and_falls_back() {
        for section in [
            Section::Library,
            Section::Albums,
            Section::Artists,
            Section::Catalog,
        ] {
            assert_eq!(Section::parse(section.as_str()), section);
        }
        // A hand-edited or future-version ini must not break startup.
        assert_eq!(Section::parse("playlists"), Section::Library);
        assert_eq!(Section::parse(""), Section::Library);
    }

    #[test]
    fn the_sidebar_starts_visible() {
        // Not bool's default: a sidebar nobody has hidden should be findable.
        assert!(Settings::default().show_sidebar);
    }

    #[test]
    fn notifications_are_off_by_default() {
        // One notification per song is noise; opting in is the user's choice.
        assert!(!Settings::default().notify_track_change);
    }
}
