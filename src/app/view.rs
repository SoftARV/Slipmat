// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Where the user is: which sidebar section is showing, and what that implies.
//!
//! Small enough to inline, kept apart because it is the one piece of app state
//! with a *persisted* representation ([`Section`]) and a *widget*
//! representation (the sidebar row index). Three encodings of the same idea is
//! exactly where they drift, so the conversions and the tests that pin them
//! live together.

use crate::music::types::Track;
use crate::settings::Section;

/// Which sidebar section is showing.
///
/// This is the single source of truth for the content pane. `SearchScope` is
/// derived from it rather than stored alongside — two overlapping states for
/// "where am I" is how the sidebar came to show Search selected while the
/// library list was on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum View {
    Search,
    #[default]
    Songs,
    Albums,
    Artists,
    Playlists,
}

impl View {
    /// Sidebar row order — the contract `connect_row_selected` reads, and the
    /// only place the mapping lives.
    pub(super) fn from_row(index: i32) -> Self {
        match index {
            0 => Self::Search,
            2 => Self::Albums,
            3 => Self::Artists,
            4 => Self::Playlists,
            _ => Self::Songs,
        }
    }

    pub(super) fn row(self) -> i32 {
        match self {
            Self::Search => 0,
            Self::Songs => 1,
            Self::Albums => 2,
            Self::Artists => 3,
            Self::Playlists => 4,
        }
    }

    pub(super) fn scope(self) -> SearchScope {
        match self {
            Self::Search => SearchScope::Catalog,
            _ => SearchScope::Library,
        }
    }
}

impl From<Section> for View {
    fn from(section: Section) -> Self {
        match section {
            Section::Library => Self::Songs,
            Section::Albums => Self::Albums,
            Section::Artists => Self::Artists,
            Section::Playlists => Self::Playlists,
            Section::Catalog => Self::Search,
        }
    }
}

impl From<View> for Section {
    fn from(view: View) -> Self {
        match view {
            View::Songs => Self::Library,
            View::Albums => Self::Albums,
            View::Artists => Self::Artists,
            View::Playlists => Self::Playlists,
            View::Search => Self::Catalog,
        }
    }
}

/// Which set of music the search box is searching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchScope {
    /// Filter the tracks already loaded from the user's library, locally.
    #[default]
    Library,
    /// Search Apple Music's whole catalog, over the network.
    Catalog,
}

/// How the Songs list is ordered.
///
/// Applied to *our* `Track`s rather than asked of Apple: the whole library is
/// already in memory, sorting 500 of them is instant, and `/me/library/songs`
/// offers no sort parameter worth a round trip anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortBy {
    /// Apple's own order, which is alphabetical by title.
    #[default]
    Title,
    Artist,
    Album,
    /// Newest first — the only ones that descend, because "recently added,
    /// oldest first" is not a thing anybody wants.
    RecentlyAdded,
    Year,
}

impl SortBy {
    pub(super) const ALL: [Self; 5] = [
        Self::Title,
        Self::Artist,
        Self::Album,
        Self::RecentlyAdded,
        Self::Year,
    ];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Title => "Title",
            Self::Artist => "Artist",
            Self::Album => "Album",
            Self::RecentlyAdded => "Recently Added",
            Self::Year => "Year",
        }
    }

    /// The action target, and what lands in the ini file.
    pub(super) fn id(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Artist => "artist",
            Self::Album => "album",
            Self::RecentlyAdded => "recent",
            Self::Year => "year",
        }
    }

    pub(super) fn parse(s: &str) -> Self {
        match s {
            "artist" => Self::Artist,
            "album" => Self::Album,
            "recent" => Self::RecentlyAdded,
            "year" => Self::Year,
            _ => Self::Title,
        }
    }

    /// Order two tracks. Every arm falls back to title, so the list is stable —
    /// two tracks that tie must not swap places between rebuilds.
    pub(super) fn compare(self, a: &Track, b: &Track) -> std::cmp::Ordering {
        let fold = |s: &str| s.to_lowercase();
        let by_title = || fold(&a.title).cmp(&fold(&b.title));
        match self {
            Self::Title => by_title(),
            Self::Artist => fold(&a.artist).cmp(&fold(&b.artist)).then_with(by_title),
            Self::Album => fold(&a.album)
                .cmp(&fold(&b.album))
                // Within an album, track order beats alphabetical. An album
                // sorted by title is not an album.
                .then_with(|| a.track_number.cmp(&b.track_number))
                .then_with(by_title),
            // Descending. Empty sorts last rather than first: something Apple
            // gave no date for is not the newest thing you own.
            Self::RecentlyAdded => b.date_added.cmp(&a.date_added).then_with(by_title),
            Self::Year => b.year.cmp(&a.year).then_with(by_title),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sidebar_row_order_round_trips() {
        // `connect_row_selected` reads a row index and `row()` writes one back
        // when restoring the last section. If those two ever disagree the app
        // opens with one section selected and another one showing — which is
        // exactly the bug this pins down.
        for view in [
            View::Search,
            View::Songs,
            View::Albums,
            View::Artists,
            View::Playlists,
        ] {
            assert_eq!(View::from_row(view.row()), view);
        }
    }

    #[test]
    fn an_unknown_sidebar_row_falls_back_to_songs() {
        assert_eq!(View::from_row(99), View::Songs);
        assert_eq!(View::from_row(-1), View::Songs);
    }

    #[test]
    fn the_view_round_trips_through_the_persisted_section() {
        for view in [
            View::Search,
            View::Songs,
            View::Albums,
            View::Artists,
            View::Playlists,
        ] {
            assert_eq!(View::from(Section::from(view)), view);
        }
    }

    #[test]
    fn only_search_looks_at_the_catalog() {
        assert_eq!(View::Search.scope(), SearchScope::Catalog);
        for view in [View::Songs, View::Albums, View::Artists, View::Playlists] {
            assert_eq!(view.scope(), SearchScope::Library);
        }
    }

    fn track(title: &str, album: &str, n: u32, added: &str, year: &str) -> Track {
        Track {
            id: crate::music::types::TrackId(title.into()),
            catalog_id: Some(title.into()),
            title: title.into(),
            artist: String::new(),
            album: album.into(),
            date_added: added.into(),
            year: year.into(),
            duration_ms: 0,
            track_number: n,
            artwork: None,
        }
    }

    #[test]
    fn every_sort_round_trips_through_its_persisted_id() {
        for sort in SortBy::ALL {
            assert_eq!(SortBy::parse(sort.id()), sort);
        }
        // A hand-edited or future-version ini must not break startup.
        assert_eq!(SortBy::parse("bpm"), SortBy::Title);
        assert_eq!(SortBy::parse(""), SortBy::Title);
    }

    #[test]
    fn recently_added_puts_the_newest_first_and_the_undated_last() {
        let mut v = [
            track("old", "", 0, "2020-01-01T00:00:00Z", ""),
            track("undated", "", 0, "", ""),
            track("new", "", 0, "2026-07-01T00:00:00Z", ""),
        ];
        v.sort_by(|a, b| SortBy::RecentlyAdded.compare(a, b));
        let order: Vec<&str> = v.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(order, ["new", "old", "undated"]);
    }

    #[test]
    fn sorting_by_album_keeps_album_order_within_an_album() {
        let mut v = [
            track("Zebra", "Fragile", 1, "", ""),
            track("Apple", "Fragile", 2, "", ""),
            track("Mango", "Aqualung", 1, "", ""),
        ];
        v.sort_by(|a, b| SortBy::Album.compare(a, b));
        let order: Vec<&str> = v.iter().map(|t| t.title.as_str()).collect();
        // An album sorted by title is not an album.
        assert_eq!(order, ["Mango", "Zebra", "Apple"]);
    }
}
