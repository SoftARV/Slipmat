// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Persistent **preferences**, in `~/.config/slipmat/settings.ini`.
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

const GROUP: &str = "Slipmat";

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
    Playlists,
    Catalog,
}

impl Section {
    fn as_str(self) -> &'static str {
        match self {
            Self::Library => "library",
            Self::Albums => "albums",
            Self::Artists => "artists",
            Self::Playlists => "playlists",
            Self::Catalog => "catalog",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "catalog" => Self::Catalog,
            "albums" => Self::Albums,
            "artists" => Self::Artists,
            "playlists" => Self::Playlists,
            _ => Self::Library,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub theme: Theme,
    /// Accent colour id; see `style::Accent`.
    pub accent: String,
    /// How the Songs list is ordered. Stored as the id string, so an unknown
    /// value from a hand-edited or future ini falls back rather than breaking
    /// startup.
    pub sort: String,
    /// Whether the user flipped the sort's natural direction.
    pub sort_reversed: bool,
    /// The grids sort separately, because their keys differ from the songs
    /// list's — an album has a date added, a playlist has no artist, an artist
    /// has only a name.
    pub album_sort: String,
    pub album_sort_reversed: bool,
    pub artist_sort: String,
    pub artist_sort_reversed: bool,
    pub playlist_sort: String,
    pub playlist_sort_reversed: bool,
    pub section: Section,
    pub show_sidebar: bool,
    /// Whether the current cover is painted behind the player, blurred. On by
    /// default.
    pub player_backdrop: bool,
    /// Notify when the track changes. Off by default (`bool`'s default).
    pub notify_track_change: bool,
    /// Playlists pinned to the sidebar, in the order they were put there.
    ///
    /// **Library ids only** (`p.…`, as `/me/library/playlists` returns them).
    /// A pinned row is a playlist you own, and the two id spaces are not
    /// interchangeable — a catalog id 404s against `/me/library`.
    pub pinned_playlists: Vec<String>,
}

/// Separates pins in the ini. KeyFile's own list separator, so a hand-edited
/// file reads the way anyone would expect.
const PIN_SEP: char = ';';

/// Split the stored pin list, dropping anything that cannot be a pin.
///
/// **Both directions are ours, deliberately.** glib 0.22 binds `string_list`
/// for reading but no `set_string_list` to pair with it, and writing through
/// `set_string` while reading through `string_list` would put an unescaped
/// write against an escaping read — which works right up until an id needs
/// escaping. Owning both sides costs a few lines and cannot drift.
///
/// Duplicates are dropped because two pins of one playlist are two identical
/// rows, and no click could tell them apart.
fn parse_pins(stored: &str) -> Vec<String> {
    let mut pins: Vec<String> = Vec::new();
    for id in stored.split(PIN_SEP) {
        let id = id.trim();
        if !id.is_empty() && !pins.iter().any(|seen| seen == id) {
            pins.push(id.to_owned());
        }
    }
    pins
}

/// Join pins for storage, dropping any that would corrupt the format.
///
/// No real Apple library playlist id contains the separator — measured against
/// a real library — but an id that did would silently become two broken pins,
/// and losing one pin beats resurrecting two that point nowhere.
fn join_pins(pins: &[String]) -> String {
    pins.iter()
        .filter(|id| !id.is_empty() && !id.contains(PIN_SEP))
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(&PIN_SEP.to_string())
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: Theme::default(),
            // Apple Music red: the app's own subject says more than GNOME blue.
            accent: "apple-red".into(),
            // Apple's own order.
            sort: "title".into(),
            sort_reversed: false,
            album_sort: "title".into(),
            album_sort_reversed: false,
            artist_sort: "title".into(),
            artist_sort_reversed: false,
            playlist_sort: "title".into(),
            playlist_sort_reversed: false,
            section: Section::default(),
            // Visible on a first run: a sidebar nobody has hidden yet should
            // be there to be found.
            show_sidebar: true,
            // Not `bool`'s default: the backdrop is the app's own look, and
            // #145 explicitly asked for it to stay on for everyone else.
            player_backdrop: true,
            notify_track_change: false,
            // Nothing pinned until somebody pins something. An app that
            // guesses which playlists matter to you gets it wrong.
            pinned_playlists: Vec::new(),
        }
    }
}

fn path() -> Option<std::path::PathBuf> {
    let dir = glib::user_config_dir().join("slipmat");
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
        if let Ok(on) = file.boolean(GROUP, "player-backdrop") {
            settings.player_backdrop = on;
        }
        if let Ok(accent) = file.string(GROUP, "accent") {
            settings.accent = accent.into();
        }
        if let Ok(sort) = file.string(GROUP, "sort") {
            settings.sort = sort.into();
        }
        if let Ok(rev) = file.boolean(GROUP, "sort-reversed") {
            settings.sort_reversed = rev;
        }
        for (key, into) in [
            ("album-sort", &mut settings.album_sort),
            ("artist-sort", &mut settings.artist_sort),
            ("playlist-sort", &mut settings.playlist_sort),
        ] {
            if let Ok(value) = file.string(GROUP, key) {
                *into = value.into();
            }
        }
        for (key, into) in [
            ("album-sort-reversed", &mut settings.album_sort_reversed),
            ("artist-sort-reversed", &mut settings.artist_sort_reversed),
            (
                "playlist-sort-reversed",
                &mut settings.playlist_sort_reversed,
            ),
        ] {
            if let Ok(value) = file.boolean(GROUP, key) {
                *into = value;
            }
        }
        if let Ok(pinned) = file.string(GROUP, "pinned-playlists") {
            settings.pinned_playlists = parse_pins(&pinned);
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
        file.set_boolean(GROUP, "player-backdrop", self.player_backdrop);
        file.set_string(GROUP, "accent", &self.accent);
        file.set_string(GROUP, "sort", &self.sort);
        file.set_boolean(GROUP, "sort-reversed", self.sort_reversed);
        file.set_string(GROUP, "album-sort", &self.album_sort);
        file.set_boolean(GROUP, "album-sort-reversed", self.album_sort_reversed);
        file.set_string(GROUP, "artist-sort", &self.artist_sort);
        file.set_boolean(GROUP, "artist-sort-reversed", self.artist_sort_reversed);
        file.set_string(GROUP, "playlist-sort", &self.playlist_sort);
        file.set_boolean(GROUP, "playlist-sort-reversed", self.playlist_sort_reversed);
        file.set_string(
            GROUP,
            "pinned-playlists",
            &join_pins(&self.pinned_playlists),
        );

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
            Section::Playlists,
            Section::Catalog,
        ] {
            assert_eq!(Section::parse(section.as_str()), section);
        }
        // A hand-edited or future-version ini must not break startup. This
        // once used "playlists" as the unknown value, which stopped being one.
        assert_eq!(Section::parse("radio"), Section::Library);
        assert_eq!(Section::parse(""), Section::Library);
    }

    #[test]
    fn the_sidebar_starts_visible() {
        // Not bool's default: a sidebar nobody has hidden should be findable.
        assert!(Settings::default().show_sidebar);
    }

    #[test]
    fn the_backdrop_starts_on() {
        // Not `bool`'s default. #145 asked for a way *out* of the backdrop and
        // said to leave it on for everyone else, so a forgotten `..default()`
        // that silently turned it off would be the opposite of the request.
        assert!(Settings::default().player_backdrop);
    }

    #[test]
    fn notifications_are_off_by_default() {
        // One notification per song is noise; opting in is the user's choice.
        assert!(!Settings::default().notify_track_change);
    }

    #[test]
    fn pins_round_trip_in_the_order_they_were_put_there() {
        // Order is the feature: pin order is what the sidebar draws, so a round
        // trip that sorted or reversed them would silently rearrange somebody's
        // sidebar between launches.
        let pins = vec![
            "p.EYWrg13SzrKxYBb".to_owned(),
            "p.e5Ukqg18xa".to_owned(),
            "p.rXAJKDruDkOY0Eg".to_owned(),
        ];
        assert_eq!(parse_pins(&join_pins(&pins)), pins);
    }

    #[test]
    fn nothing_pinned_stays_nothing() {
        assert_eq!(join_pins(&[]), "");
        assert!(parse_pins("").is_empty());
        // A key left behind by hand-editing is the same as no key.
        assert!(parse_pins(";;  ;").is_empty());
    }

    #[test]
    fn a_playlist_cannot_be_pinned_twice() {
        // Two pins of one playlist are two identical rows, and no click could
        // tell them apart.
        let stored = "p.one;p.two;p.one";
        assert_eq!(parse_pins(stored), vec!["p.one", "p.two"]);
    }

    #[test]
    fn an_id_that_would_corrupt_the_format_is_dropped_not_split() {
        // No real library id contains the separator, but one that did would
        // come back as two pins pointing nowhere. Losing it beats that.
        let pins = vec!["p.fine".to_owned(), "p.b;roken".to_owned()];
        assert_eq!(join_pins(&pins), "p.fine");
    }

    #[test]
    fn surrounding_space_from_a_hand_edited_file_is_forgiven() {
        assert_eq!(parse_pins(" p.one ; p.two "), vec!["p.one", "p.two"]);
    }
}
