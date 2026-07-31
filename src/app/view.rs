// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Where the user is: which sidebar section is showing, and what that implies.
//!
//! Small enough to inline, kept apart because it is the one piece of app state
//! with a *persisted* representation ([`Section`]) and a *widget*
//! representation (the sidebar row index). Three encodings of the same idea is
//! exactly where they drift, so the conversions and the tests that pin them
//! live together.

use super::AppModel;
use crate::music::types::{Album, Artist, Playlist, Track};
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

/// One sidebar row: which section, its icon, and what it is called.
pub(super) struct Row {
    pub view: View,
    pub icon: &'static str,
    pub label: &'static str,
}

/// What a sidebar row means.
///
/// The sidebar used to be an array of sections indexed by position, and
/// `View::from_row` turned a position back into a section. Pinning playlists
/// into it (#133) ended that: a pin is not a section — it pushes a page rather
/// than changing what the pane shows — so a row's position can no longer say
/// what the row does.
///
/// Rows carry their meaning instead, which deletes the index contract rather
/// than working around it. The alternative was a second `ListBox` holding the
/// pins, and this app shipped that once and removed it in 285b542: two boxes
/// each keep their own selection, whichever takes focus first selects its own
/// first row, and it overrode the other after the fact. One list means one
/// selection, with nothing to keep in sync and no order dependency between
/// "select this" and "clear that".
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SidebarRow {
    /// One of the five fixed sections.
    Section(View),
    /// A pinned playlist, by **library** id. The name is looked up when the row
    /// is built, so a pin costs nothing to store and cannot go out of date.
    Pinned(String),
}

/// Every row the sidebar shows, in order: the sections, then the pins.
///
/// The one place that order is decided, and what `SidebarRowChosen` indexes
/// into — so a row's meaning and its position are established together rather
/// than a hundred lines apart.
pub(super) fn sidebar_rows(pins: &[String]) -> Vec<SidebarRow> {
    View::SIDEBAR
        .iter()
        .map(|row| SidebarRow::Section(row.view))
        .chain(pins.iter().cloned().map(SidebarRow::Pinned))
        .collect()
}

/// Where a section sits, for selecting the persisted one at startup.
pub(super) fn section_index(rows: &[SidebarRow], view: View) -> Option<i32> {
    rows.iter()
        .position(|row| row == &SidebarRow::Section(view))
        .and_then(|i| i32::try_from(i).ok())
}

impl View {
    /// The sidebar, in order.
    ///
    /// The sections, and only the sections — [`sidebar_rows`] is what the
    /// sidebar actually shows, because pins live below these.
    ///
    /// The label is not [`View::title`]: the row says "Search" under a heading
    /// that says "Apple Music", and the narrow header has no heading to lean on.
    pub(super) const SIDEBAR: [Row; 5] = [
        Row {
            view: Self::Search,
            icon: "system-search-symbolic",
            label: "Search",
        },
        Row {
            view: Self::Songs,
            icon: "folder-music-symbolic",
            label: "Songs",
        },
        Row {
            view: Self::Albums,
            icon: "media-optical-symbolic",
            label: "Albums",
        },
        Row {
            view: Self::Artists,
            icon: "avatar-default-symbolic",
            label: "Artists",
        },
        Row {
            view: Self::Playlists,
            // A grid, because that is what the row opens — and it sets the row
            // apart from the pins below it, which are lists.
            //
            // Not `grid-filled-symbolic`: adwaita-icon-theme 50 ships no
            // `*filled*` symbolic icons, and a missing name renders as the
            // fallback rather than as itself.
            icon: "view-grid-symbolic",
            // "All", because the heading above it says Playlists and the pins
            // below it are the rest of that group. On its own the row would
            // read as a section; in place it reads as "all of them".
            label: "All",
        },
    ];

    pub(super) fn scope(self) -> SearchScope {
        match self {
            Self::Search => SearchScope::Catalog,
            _ => SearchScope::Library,
        }
    }

    /// What the header says it is showing.
    ///
    /// Only ever read on a narrow window, where it stands in for the search
    /// entry — and where the sidebar has already collapsed to an overlay, so
    /// the selected row that normally answers "where am I" is off screen.
    pub(super) fn title(self) -> &'static str {
        match self {
            Self::Search => "Apple Music",
            Self::Songs => "Songs",
            Self::Albums => "Albums",
            Self::Artists => "Artists",
            Self::Playlists => "Playlists",
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
    /// When it was saved to the library.
    ///
    /// **Only offered where Apple actually sends it.** Measured against a real
    /// library: 420 of 420 albums and 8 of 8 playlists carry `dateAdded`, and
    /// **0 of 541 songs** do — the same attribute, documented for all three,
    /// present for two. A sort that silently orders by nothing is worse than
    /// one that is not offered, so `SortBy::for_view` is what decides.
    Added,
    /// When a playlist was last edited. Playlists only; 8 of 8 carry it.
    Updated,
}

impl SortBy {
    /// What each section can honestly be sorted by.
    ///
    /// Not one list: the keys differ because the *data* differs. An album has a
    /// year and a date added; a playlist has neither an artist nor a year; a
    /// library artist carries **only a name**, so its menu would be a single
    /// radio button and it gets the direction toggle instead.
    pub(super) fn for_view(view: View) -> &'static [Self] {
        match view {
            // Search results are songs, so they sort like songs.
            View::Songs | View::Search => &[Self::Title, Self::Artist, Self::Album, Self::Year],
            View::Albums => &[Self::Title, Self::Artist, Self::Year, Self::Added],
            View::Playlists => &[Self::Title, Self::Added, Self::Updated],
            View::Artists => &[Self::Title],
        }
    }

    /// The first key a section offers, used when a restored one does not apply
    /// to it — a playlist cannot sort by artist however the ini file reads.
    pub(super) fn valid_for(self, view: View) -> Self {
        let allowed = Self::for_view(view);
        if allowed.contains(&self) {
            self
        } else {
            allowed[0]
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Title => "Title",
            Self::Artist => "Artist",
            Self::Album => "Album",
            Self::Year => "Year",
            Self::Added => "Recently Added",
            Self::Updated => "Recently Updated",
        }
    }

    /// The action target, and what lands in the ini file.
    pub(super) fn id(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Artist => "artist",
            Self::Album => "album",
            Self::Year => "year",
            Self::Added => "added",
            Self::Updated => "updated",
        }
    }

    pub(super) fn parse(s: &str) -> Self {
        match s {
            "artist" => Self::Artist,
            "album" => Self::Album,
            "year" => Self::Year,
            "added" => Self::Added,
            "updated" => Self::Updated,
            _ => Self::Title,
        }
    }

    /// Which way round this sort reads *naturally*, before the user flips it.
    ///
    /// Alphabetical wants A–Z; dates want newest first. Folding that in here
    /// means the direction toggle always means the same thing on screen — the
    /// arrow points the way the list actually runs.
    pub(super) fn descends_by_default(self) -> bool {
        matches!(self, Self::Year | Self::Added | Self::Updated)
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
            // Songs do not carry these — `for_view` never offers them here —
            // but a restored setting could still name one, so they order by
            // title rather than pretending.
            Self::Added | Self::Updated => by_title(),
        }
    }

    /// Order two albums. Same discipline as [`SortBy::compare`]: every arm
    /// falls back to title so ties cannot swap places between rebuilds.
    pub(super) fn compare_album(self, a: &Album, b: &Album) -> std::cmp::Ordering {
        let fold = |s: &str| s.to_lowercase();
        let by_title = || fold(&a.name).cmp(&fold(&b.name));
        match self {
            Self::Artist => fold(&a.artist).cmp(&fold(&b.artist)).then_with(by_title),
            Self::Year => a.year.cmp(&b.year).then_with(by_title),
            Self::Added => a.date_added.cmp(&b.date_added).then_with(by_title),
            _ => by_title(),
        }
    }

    /// Order two playlists.
    pub(super) fn compare_playlist(self, a: &Playlist, b: &Playlist) -> std::cmp::Ordering {
        let fold = |s: &str| s.to_lowercase();
        let by_title = || fold(&a.name).cmp(&fold(&b.name));
        match self {
            Self::Added => a.date_added.cmp(&b.date_added).then_with(by_title),
            Self::Updated => a.last_modified.cmp(&b.last_modified).then_with(by_title),
            _ => by_title(),
        }
    }

    /// Order two artists. Only ever by name, because that is all a library
    /// artist has — the direction toggle is the whole control here.
    pub(super) fn compare_artist(a: &Artist, b: &Artist) -> std::cmp::Ordering {
        a.name.to_lowercase().cmp(&b.name.to_lowercase())
    }
}

impl AppModel {
    /// Write every section's sort to the ini file.
    ///
    /// All four together rather than only the one that changed: they live in
    /// one file, `save` writes the whole thing anyway, and picking out the
    /// changed one is a chance to forget one.
    pub(super) fn persist_sorts(&mut self) {
        let s = &mut self.settings;
        let sorts = self.sorts;
        s.sort = sorts.songs.by.id().into();
        s.sort_reversed = sorts.songs.reversed;
        s.album_sort = sorts.albums.by.id().into();
        s.album_sort_reversed = sorts.albums.reversed;
        s.artist_sort = sorts.artists.by.id().into();
        s.artist_sort_reversed = sorts.artists.reversed;
        s.playlist_sort = sorts.playlists.by.id().into();
        s.playlist_sort_reversed = sorts.playlists.reversed;
        s.save();
    }

    /// Rebuild whichever section is showing, in its new order.
    ///
    /// The fingerprint is cleared first: it exists to skip a rebuild that would
    /// change nothing, and a new sort changes everything.
    pub(super) fn resort(&mut self) {
        match self.view {
            View::Albums => {
                self.built_albums = None;
                self.rebuild_albums();
            }
            View::Artists => {
                self.built_artists = None;
                self.rebuild_artists();
            }
            View::Playlists => {
                self.built_playlists = None;
                self.rebuild_playlists();
            }
            View::Songs | View::Search => {
                self.built_rows = None;
                self.rebuild_rows();
            }
        }
    }
}

/// What one section is sorted by, and whether the user flipped it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sort {
    pub by: SortBy,
    pub reversed: bool,
}

/// Every section's sort, kept apart on purpose.
///
/// One shared setting would mean choosing "Recently Added" for albums and
/// finding songs claiming to be sorted by a date they do not have. The keys
/// differ because the data does, so the choices do too.
#[derive(Debug, Clone, Copy, Default)]
pub struct Sorts {
    pub songs: Sort,
    pub albums: Sort,
    pub artists: Sort,
    pub playlists: Sort,
}

impl Default for Sort {
    fn default() -> Self {
        Self {
            by: SortBy::Title,
            reversed: false,
        }
    }
}

impl Sorts {
    /// The sort in force for `view`. Search shares the songs list's, because
    /// it *is* the songs list showing other results.
    pub(super) fn get(&self, view: View) -> Sort {
        match view {
            View::Albums => self.albums,
            View::Artists => self.artists,
            View::Playlists => self.playlists,
            View::Songs | View::Search => self.songs,
        }
    }

    pub(super) fn set(&mut self, view: View, sort: Sort) {
        let slot = match view {
            View::Albums => &mut self.albums,
            View::Artists => &mut self.artists,
            View::Playlists => &mut self.playlists,
            View::Songs | View::Search => &mut self.songs,
        };
        *slot = sort;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_section_can_be_found_again_by_the_row_it_built() {
        // The replacement for the old index contract. `section_index` is what
        // selects the persisted section at startup, and `sidebar_rows` is what
        // built the row it has to find — if those disagree the app opens with
        // one section selected and another one showing.
        let rows = sidebar_rows(&[]);
        for row in &View::SIDEBAR {
            let index = section_index(&rows, row.view)
                .unwrap_or_else(|| panic!("{} has no row", row.label));
            assert_eq!(
                rows[index as usize],
                SidebarRow::Section(row.view),
                "{} was found at somebody else's row",
                row.label
            );
        }
    }

    #[test]
    fn the_sections_come_first_and_keep_their_order() {
        // Pins are appended, so a section's position does not move when one is
        // added — but nothing enforces that except this. A pin landing among
        // the sections would put a page-pusher where a section switch belongs.
        let rows = sidebar_rows(&["p.one".to_owned(), "p.two".to_owned()]);
        let sections: Vec<_> = View::SIDEBAR
            .iter()
            .map(|row| SidebarRow::Section(row.view))
            .collect();
        assert_eq!(&rows[..sections.len()], &sections[..]);
        assert_eq!(
            &rows[sections.len()..],
            &[
                SidebarRow::Pinned("p.one".to_owned()),
                SidebarRow::Pinned("p.two".to_owned()),
            ]
        );
    }

    #[test]
    fn a_pin_is_never_mistaken_for_a_section() {
        // The whole reason rows carry their meaning. Clicking a pin must push a
        // page; clicking a section must change the pane. Reading either from a
        // position is what this design removed.
        let rows = sidebar_rows(&["p.one".to_owned()]);
        assert!(section_index(&rows, View::Playlists).is_some());
        assert!(matches!(rows.last(), Some(SidebarRow::Pinned(_))));
    }

    #[test]
    fn a_section_only_offers_keys_its_data_has() {
        // Measured, not assumed: 420/420 albums and 8/8 playlists carry
        // `dateAdded`, and 0/541 songs do. Offering "Recently Added" on the
        // songs list would sort by an empty string and look like it worked.
        assert!(!SortBy::for_view(View::Songs).contains(&SortBy::Added));
        assert!(SortBy::for_view(View::Albums).contains(&SortBy::Added));
        assert!(SortBy::for_view(View::Playlists).contains(&SortBy::Added));
        // Only playlists are ever edited after the fact.
        assert!(SortBy::for_view(View::Playlists).contains(&SortBy::Updated));
        assert!(!SortBy::for_view(View::Albums).contains(&SortBy::Updated));
        // A library artist carries a name and nothing else, so there is
        // nothing to choose between — the direction toggle is the control.
        assert_eq!(SortBy::for_view(View::Artists), &[SortBy::Title]);
    }

    #[test]
    fn a_restored_sort_a_section_cannot_honour_falls_back() {
        // The ini file outlives any one version of this list.
        assert_eq!(SortBy::Updated.valid_for(View::Albums), SortBy::Title);
        assert_eq!(SortBy::Artist.valid_for(View::Playlists), SortBy::Title);
        assert_eq!(SortBy::Added.valid_for(View::Albums), SortBy::Added);
    }

    #[test]
    fn each_section_keeps_its_own_sort() {
        let mut sorts = Sorts::default();
        sorts.set(
            View::Albums,
            Sort {
                by: SortBy::Added,
                reversed: true,
            },
        );
        assert_eq!(sorts.get(View::Albums).by, SortBy::Added);
        // Untouched by the album choice, which is the whole point.
        assert_eq!(sorts.get(View::Songs).by, SortBy::Title);
        // Search is the songs list showing other results, so it shares.
        sorts.set(
            View::Songs,
            Sort {
                by: SortBy::Artist,
                reversed: false,
            },
        );
        assert_eq!(sorts.get(View::Search).by, SortBy::Artist);
    }

    #[test]
    fn a_position_outside_the_list_names_no_row() {
        // `ListBox` reports a selection while rows are being rebuilt under it,
        // so an out-of-range position is ordinary rather than exceptional. It
        // used to fall back to Songs, which meant a rebuild could silently
        // change section.
        let rows = sidebar_rows(&[]);
        assert!(rows.get(99).is_none());
        assert!(section_index(&[], View::Songs).is_none());
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
        for sort in [
            SortBy::Title,
            SortBy::Artist,
            SortBy::Album,
            SortBy::Year,
            SortBy::Added,
            SortBy::Updated,
        ] {
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
