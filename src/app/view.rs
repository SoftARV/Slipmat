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

/// Which kinds of result a catalog search asks Apple for.
///
/// **A second axis, not a third `SearchScope`.** Scope is *where* the search
/// runs; this is *what it looks for*. Collapsing them would need a variant per
/// combination.
///
/// Narrowing is not only a convenience. Apple pages a single `offset` across
/// every type named in `types=`, so "show me more albums" is not a question
/// that can even be asked until albums are the only thing being requested —
/// scrolling a mixed result set drags in more of all three.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CatalogFilter {
    #[default]
    All,
    Songs,
    Albums,
    Artists,
    Playlists,
}

impl CatalogFilter {
    pub(super) const ALL: [Self; 5] = [
        Self::All,
        Self::Songs,
        Self::Albums,
        Self::Artists,
        Self::Playlists,
    ];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::All => "Everything",
            Self::Songs => "Songs",
            Self::Albums => "Albums",
            Self::Artists => "Artists",
            Self::Playlists => "Playlists",
        }
    }

    /// The action target. Not persisted — a filter is what you want of *this*
    /// search, unlike the library sort, which is how you like your library.
    pub(super) fn id(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Songs => "songs",
            Self::Albums => "albums",
            Self::Artists => "artists",
            Self::Playlists => "playlists",
        }
    }

    pub(super) fn parse(s: &str) -> Self {
        match s {
            "songs" => Self::Songs,
            "albums" => Self::Albums,
            "artists" => Self::Artists,
            "playlists" => Self::Playlists,
            _ => Self::All,
        }
    }

    /// The `types=` value. Apple wants a comma-separated list, and answers only
    /// for the kinds named — a key is absent rather than empty when a kind
    /// matched nothing.
    pub(super) fn types(self) -> &'static str {
        match self {
            Self::All => "songs,albums,artists,playlists",
            Self::Songs => "songs",
            Self::Albums => "albums",
            Self::Artists => "artists",
            Self::Playlists => "playlists",
        }
    }
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
    Year,
}

impl SortBy {
    pub(super) const ALL: [Self; 4] = [Self::Title, Self::Artist, Self::Album, Self::Year];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Title => "Title",
            Self::Artist => "Artist",
            Self::Album => "Album",
            Self::Year => "Year",
        }
    }

    /// The action target, and what lands in the ini file.
    pub(super) fn id(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Artist => "artist",
            Self::Album => "album",
            Self::Year => "year",
        }
    }

    pub(super) fn parse(s: &str) -> Self {
        match s {
            "artist" => Self::Artist,
            "album" => Self::Album,
            "year" => Self::Year,
            _ => Self::Title,
        }
    }

    /// Which way round this sort reads *naturally*, before the user flips it.
    ///
    /// Alphabetical wants A–Z; dates want newest first. Folding that in here
    /// means the direction toggle always means the same thing on screen — the
    /// arrow points the way the list actually runs.
    pub(super) fn descends_by_default(self) -> bool {
        matches!(self, Self::Year)
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
            // Ascending here; `descends_by_default` flips it for display, so
            // Year reads newest-first without this arm knowing about direction.
            Self::Year => a.year.cmp(&b.year).then_with(by_title),
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

    #[test]
    fn every_catalog_filter_round_trips_through_its_id() {
        for filter in CatalogFilter::ALL {
            assert_eq!(CatalogFilter::parse(filter.id()), filter);
        }
        // A future version's id, or a typo, must widen rather than show nothing.
        assert_eq!(CatalogFilter::parse("music-videos"), CatalogFilter::All);
        assert_eq!(CatalogFilter::parse(""), CatalogFilter::All);
    }

    #[test]
    fn unfiltered_asks_for_every_kind_the_app_can_show() {
        // If a kind is missing here it is unreachable from search entirely —
        // which is exactly how catalog playlists went missing for four
        // milestones.
        let all = CatalogFilter::All.types();
        for filter in CatalogFilter::ALL {
            if filter == CatalogFilter::All {
                continue;
            }
            assert!(
                all.split(',').any(|kind| kind == filter.types()),
                "{:?} is offered as a filter but absent from the unfiltered search",
                filter
            );
        }
    }

    #[test]
    fn a_narrowed_search_asks_for_exactly_one_kind() {
        // Apple pages one offset across every kind named, so more than one
        // here would make "load more" incoherent again.
        for filter in CatalogFilter::ALL {
            if filter == CatalogFilter::All {
                continue;
            }
            assert!(!filter.types().contains(','), "{filter:?}");
        }
    }

    fn track(title: &str, album: &str, n: u32, added: &str, year: &str) -> Track {
        Track {
            id: crate::music::types::TrackId(title.into()),
            catalog_id: Some(title.into()),
            title: title.into(),
            artist: String::new(),
            album: album.into(),
            favorite: false,
            in_library: false,
            library_id: None,
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

    /// What the list actually shows: the comparator, then the natural
    /// direction. Mirrors `visible_entries`.
    fn displayed(sort: SortBy, reversed: bool, v: &mut [Track]) -> Vec<String> {
        v.sort_by(|a, b| sort.compare(a, b));
        if sort.descends_by_default() != reversed {
            v.reverse();
        }
        v.iter().map(|t| t.title.clone()).collect()
    }

    #[test]
    fn year_reads_newest_first_and_flips_on_request() {
        let mut v = vec![
            track("old", "", 0, "", "1999"),
            track("undated", "", 0, "", ""),
            track("new", "", 0, "", "2026"),
        ];
        // Years read newest-first without anyone asking, and something Apple
        // gave no year for is not the newest thing you own.
        assert_eq!(
            displayed(SortBy::Year, false, &mut v),
            ["new", "old", "undated"]
        );
        assert_eq!(
            displayed(SortBy::Year, true, &mut v),
            ["undated", "old", "new"]
        );
    }

    #[test]
    fn alphabetical_reads_a_to_z_without_asking() {
        // The opposite default from dates, which is the whole point of
        // `descends_by_default`: Reverse Order always means "the other way
        // from how this list naturally reads".
        let mut v = vec![track("Zebra", "", 0, "", ""), track("Apple", "", 0, "", "")];
        assert_eq!(displayed(SortBy::Title, false, &mut v), ["Apple", "Zebra"]);
        assert_eq!(displayed(SortBy::Title, true, &mut v), ["Zebra", "Apple"]);
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
