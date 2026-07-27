// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! What the content pane is showing, and where it came from.
//!
//! Three collections and one catalog search, all landing in the same place: a
//! list or a grid of our own types. Loading is per section and on first visit,
//! so launching does not wait on three round trips, and each section answers
//! for its own emptiness.
//!
//! The rebuild functions all follow the same discipline as Pitwall's row
//! reconcile, inverted: a full rebuild is honest here because a filter can
//! change membership arbitrarily on every keystroke, but it **resets the
//! scroll**, so nothing outside a load or a query change may call one.

use relm4::ComponentSender;

use super::{
    AppModel, CATALOG_BROWSE_ROWS, CATALOG_LIMIT, CatalogFilter, CommandMsg, LIBRARY_MAX,
    SearchScope, Tile,
};
use crate::components::grid_item::{ArtRegistry, GridItem};
use crate::components::track_row::{Entry, LibraryItem, apply_row_state};
use crate::music::client::{Client, SearchResults};
use crate::music::types::Track;

/// Fold one page of catalog results into rows, and report how many of the
/// **paging kind** came back.
///
/// That second number is the whole reason this is a function rather than a
/// match at the call site. Apple advances one `offset` across every type named
/// in `types=`, so exactly one kind can drive pagination — and which one
/// depends on the filter. Counting the wrong list makes the next page start in
/// the wrong place, silently.
///
/// Unfiltered, artists and albums are trimmed hard and put on top: they are the
/// way into browsing, and burying them under 25 songs makes them invisible. The
/// point is a door, not an exhaustive list — which is exactly what the filter
/// is for when you want the list.
pub(super) fn catalog_rows(
    filter: CatalogFilter,
    found: SearchResults,
    first_page: bool,
) -> (Vec<Entry>, usize) {
    let SearchResults {
        songs,
        albums,
        artists,
        playlists,
    } = found;

    // Taken before anything is consumed below.
    let (n_songs, n_albums, n_artists, n_playlists) =
        (songs.len(), albums.len(), artists.len(), playlists.len());

    match filter {
        CatalogFilter::All if first_page => {
            let rows = artists
                .into_iter()
                .take(CATALOG_BROWSE_ROWS)
                .map(Entry::Artist)
                .chain(
                    playlists
                        .into_iter()
                        .take(CATALOG_BROWSE_ROWS)
                        .map(Entry::Playlist),
                )
                .chain(
                    albums
                        .into_iter()
                        .take(CATALOG_BROWSE_ROWS)
                        .map(Entry::Album),
                )
                .chain(songs.into_iter().map(Entry::Song))
                .collect();
            (rows, n_songs)
        }
        // Later pages append songs only. Paging returns the browse kinds again,
        // and adding them would duplicate rows already on screen.
        CatalogFilter::All => (songs.into_iter().map(Entry::Song).collect(), n_songs),

        // Filtered: one kind, so every row counts towards the next offset and
        // "more" is finally a coherent request.
        CatalogFilter::Songs => (songs.into_iter().map(Entry::Song).collect(), n_songs),
        CatalogFilter::Albums => (albums.into_iter().map(Entry::Album).collect(), n_albums),
        CatalogFilter::Artists => (artists.into_iter().map(Entry::Artist).collect(), n_artists),
        CatalogFilter::Playlists => (
            playlists.into_iter().map(Entry::Playlist).collect(),
            n_playlists,
        ),
    }
}

/// Case-insensitive substring match across the fields a person would search by.
///
/// Deliberately not fuzzy: with the whole library in memory, plain substring is
/// instant and predictable, and "predictable" is what makes type-to-find work.
pub(super) fn matches(track: &Track, needle: &str) -> bool {
    track.title.to_lowercase().contains(needle)
        || track.artist.to_lowercase().contains(needle)
        || track.album.to_lowercase().contains(needle)
}

impl AppModel {
    pub(super) fn query(&self) -> &str {
        match self.scope() {
            SearchScope::Library => &self.library_query,
            SearchScope::Catalog => &self.catalog_query,
        }
    }

    /// What the results list shows, in order.
    pub(super) fn visible_entries(&self) -> Vec<Entry> {
        match self.scope() {
            SearchScope::Library => {
                let needle = self.query().trim().to_lowercase();
                let mut tracks: Vec<Track> = self
                    .all_tracks
                    .iter()
                    .filter(|t| needle.is_empty() || matches(t, &needle))
                    .cloned()
                    .collect();
                // Sorted here rather than in `all_tracks`, so changing the
                // order never has to re-fetch and Apple's own load order stays
                // available underneath.
                let sort = self.sort;
                tracks.sort_by(|a, b| sort.compare(a, b));
                // Reversed rather than sorted the other way, so ties keep the
                // stable order the comparator already gave them.
                if sort.descends_by_default() != self.sort_reversed {
                    tracks.reverse();
                }
                tracks.into_iter().map(Entry::Song).collect()
            }
            // Apple already ranked these; filtering them again locally would
            // only throw away results that matched for reasons we cannot see.
            SearchScope::Catalog => self.catalog.clone(),
        }
    }

    pub(super) fn run_catalog_search(
        &mut self,
        sender: &ComponentSender<Self>,
        generation: u64,
        offset: usize,
    ) {
        let Some(tokens) = &self.tokens else {
            return;
        };
        let client = Client::new(
            tokens.developer_token.clone(),
            tokens.music_user_token.clone(),
            tokens.storefront.clone(),
        );
        let term = self.catalog_query.trim().to_owned();
        if term.is_empty() {
            return;
        }
        self.searching_catalog = true;
        let types = self.catalog_filter.types();
        tracing::debug!(%term, types, "searching the catalog");
        sender.oneshot_command(async move {
            CommandMsg::Catalog {
                generation,
                offset,
                result: client
                    .search(&term, types, CATALOG_LIMIT, offset)
                    .await
                    .map_err(|err| format!("{err:#}")),
            }
        });
    }

    /// Add rows to the end, leaving the ones already there alone.
    ///
    /// Paging in more catalog results is the one change to this list that is
    /// purely additive: every existing row still stands and still means the
    /// same thing. `rebuild_rows` would clear the view and build it again,
    /// which discards the scroll position — so scrolling to the bottom to
    /// fetch more put the reader straight back at the top of a list they had
    /// just worked their way down. The rebuild is right when the *contents*
    /// change; it is wrong when they only grow.
    ///
    /// `built_rows` is deliberately left alone. It describes the query the
    /// widgets were built for, and appending does not change that — clearing
    /// it here would make the next section switch rebuild for no reason, at
    /// the ~2.5ms-per-cover cost that fingerprint exists to avoid.
    pub(super) fn append_rows(&mut self, new: &[Entry]) {
        if new.is_empty() {
            return;
        }
        let started = std::time::Instant::now();
        let _timed = crate::app::Timed("rows-append", started);
        tracing::debug!(added = new.len(), "library: appending rows");

        let registry = self.library_icons.clone();
        // Already current — the new rows read the marker at bind time, exactly
        // as the ones above them did.
        let current = self.current_track.clone();
        let dead = self.dead_rows.clone();
        self.library.extend_from_iter(
            new.iter().cloned().map(|entry| {
                LibraryItem::new(entry, registry.clone(), current.clone(), dead.clone())
            }),
        );
    }

    /// Rebuild the visible rows from `all_tracks` + query.
    ///
    /// A full rebuild is honest here, unlike Pitwall's in-place reconcile: the
    /// filter can change membership arbitrarily on every keystroke, and these
    /// rows hold no state worth preserving (no popovers, no expanders).
    ///
    /// It does **reset the scroll**, though, so it is the wrong tool for a
    /// change that only adds — see [`Self::append_rows`].
    pub(super) fn rebuild_rows(&mut self) {
        // Sort and direction as well as the query: all three change the order,
        // and the widgets already on screen may satisfy the new request.
        let fingerprint = format!(
            "{}\u{1}{}\u{1}{}\u{1}{:?}",
            self.query(),
            self.sort.id(),
            self.sort_reversed,
            self.scope()
        );
        if self.built_rows.as_deref() == Some(fingerprint.as_str()) {
            return;
        }
        self.built_rows = Some(fingerprint);

        let started = std::time::Instant::now();
        let _timed = crate::app::Timed("rows", started);

        // Rebuilding resets the scroll. It is legitimate on load and on a
        // search change; anywhere else it is a bug, so say when it happens.
        tracing::debug!(query = %self.query(), "library: rebuilding rows");
        let visible = self.visible_entries();
        let playing = self.playing_catalog_id();
        // The rows are built with the marker already set, so record that here
        // or `mark_now_playing` will think it still needs applying.
        self.marked_playing = playing.clone();
        let registry = self.library_icons.clone();
        // Rows are about to be discarded; none of their widgets are ours now.
        registry.borrow_mut().clear();
        // Rows read the marker from here at bind time, so it just has to be
        // current before they are built.
        let current = self.current_track.clone();
        *current.borrow_mut() = playing.clone();
        let dead = self.dead_rows.clone();
        self.library.clear();
        self.library.extend_from_iter(
            visible.into_iter().map(|track| {
                LibraryItem::new(track, registry.clone(), current.clone(), dead.clone())
            }),
        );
    }

    /// Move the play marker in the library list.
    ///
    /// Only the two affected rows are touched — the one losing the marker and
    /// the one gaining it — by replacing them in the store, which is what makes
    /// `ListView` re-bind those rows. Mutating the item in place does nothing:
    /// the store emits no change, so the widget is never told to update. That
    /// is why the marker did not appear at all in the first virtualised
    /// version.
    pub(super) fn mark_now_playing(&mut self) {
        let current = self.playing_catalog_id();
        if current == self.marked_playing {
            return;
        }
        // The shared cell first, so any row bound from here on is correct...
        *self.current_track.borrow_mut() = current.clone();
        // ...then the two rows that are on screen right now, if they are.
        if let Some(old) = self.marked_playing.take() {
            self.set_row_playing(&old, false);
        }
        if let Some(new) = &current {
            self.set_row_playing(new, true);
        }
        self.marked_playing = current;
    }

    /// Move the marker on one row **without touching the model**.
    ///
    /// Editing the store — even replacing a single item — makes `ListView`
    /// re-measure, and the scroll jumps to the top. Intolerable for something
    /// that fires on every track change. So: update the item's data silently,
    /// so a later re-bind is correct, and update the widget directly if this
    /// row happens to be on screen right now.
    /// Repaint one row's marker. Touches a widget, never the model.
    ///
    /// Every list gets asked, not just the results one. The same song can be on
    /// an album page and in the search results underneath it, and a marker that
    /// only lands on whichever was built last is a marker you cannot trust.
    pub(super) fn set_row_playing(&self, catalog_id: &str, playing: bool) {
        let playable = !self.dead_rows.borrow().contains(catalog_id);
        let lists =
            std::iter::once(&self.library_icons).chain(self.pages.iter().map(|p| p.registry()));
        for registry in lists {
            if let Some(w) = registry.borrow().get(catalog_id) {
                apply_row_state(&w.icon, &w.root, playing, playable);
            }
        }
    }

    /// Load the library's albums, once. Revisiting the section is instant.
    pub(super) fn load_albums(&mut self, sender: &ComponentSender<Self>) {
        // `tried` as well as "is it empty": a load that *failed* leaves the
        // collection empty, and without this it was re-attempted on every
        // sidecar event forever. Cleared by the section's reload button, which
        // is how a failure is meant to be retried.
        if self.loading_albums || self.tried_albums || !self.albums.is_empty() {
            return;
        }
        self.tried_albums = true;
        let Some(client) = self.client() else { return };
        self.loading_albums = true;
        tracing::info!("loading library albums");
        sender.oneshot_command(async move {
            CommandMsg::LibraryAlbums(
                client
                    .all_library_albums(LIBRARY_MAX)
                    .await
                    .map_err(|err| format!("{err:#}")),
            )
        });
    }

    pub(super) fn load_artists(&mut self, sender: &ComponentSender<Self>) {
        // `tried` as well as "is it empty": a load that *failed* leaves the
        // collection empty, and without this it was re-attempted on every
        // sidecar event forever. Cleared by the section's reload button, which
        // is how a failure is meant to be retried.
        if self.loading_artists || self.tried_artists || !self.artists.is_empty() {
            return;
        }
        self.tried_artists = true;
        let Some(client) = self.client() else { return };
        self.loading_artists = true;
        tracing::info!("loading library artists");
        sender.oneshot_command(async move {
            CommandMsg::LibraryArtists(
                client
                    .all_library_artists(LIBRARY_MAX)
                    .await
                    .map_err(|err| format!("{err:#}")),
            )
        });
    }

    /// Rebuild the album grid from `albums` + the query.
    pub(super) fn rebuild_albums(&mut self) {
        // Already showing exactly this? Then the widgets are correct and
        // rebuilding them would only re-decode every cover — see `built_albums`.
        let fingerprint = self.library_query.trim().to_lowercase();
        if self.built_albums.as_deref() == Some(fingerprint.as_str()) {
            return;
        }
        self.built_albums = Some(fingerprint);

        let started = std::time::Instant::now();
        let _timed = crate::app::Timed("albums", started);

        let needle = self.library_query.trim().to_lowercase();
        let tiles: Vec<Tile> = self
            .albums
            .iter()
            .filter(|a| {
                needle.is_empty()
                    || a.name.to_lowercase().contains(&needle)
                    || a.artist.to_lowercase().contains(&needle)
            })
            .cloned()
            .map(Tile::Album)
            .collect();
        self.album_grid.clear();
        self.album_art_widgets.borrow_mut().clear();
        let items = self.grid_items(tiles, &self.album_art_widgets);
        self.album_grid.extend_from_iter(items);
    }

    /// Load the library's playlists, once.
    pub(super) fn load_playlists(&mut self, sender: &ComponentSender<Self>) {
        // `tried` as well as "is it empty": a load that *failed* leaves the
        // collection empty, and without this it was re-attempted on every
        // sidecar event forever. Cleared by the section's reload button, which
        // is how a failure is meant to be retried.
        if self.loading_playlists || self.tried_playlists || !self.playlists.is_empty() {
            return;
        }
        self.tried_playlists = true;
        let Some(client) = self.client() else { return };
        self.loading_playlists = true;
        tracing::info!("loading library playlists");
        sender.oneshot_command(async move {
            CommandMsg::LibraryPlaylists(
                client
                    .all_library_playlists(LIBRARY_MAX)
                    .await
                    .map_err(|err| format!("{err:#}")),
            )
        });
    }

    pub(super) fn rebuild_playlists(&mut self) {
        // Already showing exactly this? Then the widgets are correct and
        // rebuilding them would only re-decode every cover — see `built_playlists`.
        let fingerprint = self.library_query.trim().to_lowercase();
        if self.built_playlists.as_deref() == Some(fingerprint.as_str()) {
            return;
        }
        self.built_playlists = Some(fingerprint);

        let started = std::time::Instant::now();
        let _timed = crate::app::Timed("playlists", started);

        let needle = self.library_query.trim().to_lowercase();
        let tiles: Vec<Tile> = self
            .playlists
            .iter()
            .filter(|p| {
                needle.is_empty()
                    || p.name.to_lowercase().contains(&needle)
                    || p.curator.to_lowercase().contains(&needle)
            })
            .cloned()
            .map(Tile::Playlist)
            .collect();
        self.playlist_grid.clear();
        self.playlist_art_widgets.borrow_mut().clear();
        let items = self.grid_items(tiles, &self.playlist_art_widgets);
        self.playlist_grid.extend_from_iter(items);
    }

    pub(super) fn rebuild_artists(&mut self) {
        // Already showing exactly this? Then the widgets are correct and
        // rebuilding them would only re-decode every cover — see `built_artists`.
        let fingerprint = self.library_query.trim().to_lowercase();
        if self.built_artists.as_deref() == Some(fingerprint.as_str()) {
            return;
        }
        self.built_artists = Some(fingerprint);

        let started = std::time::Instant::now();
        let _timed = crate::app::Timed("artists", started);

        let needle = self.library_query.trim().to_lowercase();
        let tiles: Vec<Tile> = self
            .artists
            .iter()
            .filter(|a| needle.is_empty() || a.name.to_lowercase().contains(&needle))
            .cloned()
            .map(Tile::Artist)
            .collect();
        self.artist_grid.clear();
        self.artist_art_widgets.borrow_mut().clear();
        let items = self.grid_items(tiles, &self.artist_art_widgets);
        self.artist_grid.extend_from_iter(items);
    }

    /// Wrap tiles with the shared artwork cache and the "fetch this" callback.
    pub(super) fn grid_items(&self, tiles: Vec<Tile>, registry: &ArtRegistry) -> Vec<GridItem> {
        tiles
            .into_iter()
            .map(|tile| {
                GridItem::new(
                    tile,
                    self.tile_art.clone(),
                    registry.clone(),
                    self.tile_art_request.clone(),
                )
            })
            .collect()
    }

    /// An API client for the current tokens, or `None` if we have none yet.
    ///
    /// Built per request rather than cached: the developer token is re-harvested
    /// and can be replaced mid-session (rule 7).
    pub(super) fn client(&self) -> Option<Client> {
        let tokens = self.tokens.as_ref()?;
        Some(Client::new(
            tokens.developer_token.clone(),
            tokens.music_user_token.clone(),
            tokens.storefront.clone(),
        ))
    }

    pub(super) fn load_library(&mut self, sender: &ComponentSender<Self>) {
        let Some(tokens) = &self.tokens else {
            self.toast("Not connected yet");
            return;
        };
        // Songs never got the `tried` guard the three grids have, and the
        // consequence is the same one their comment describes: a load that
        // *failed* leaves `all_tracks` empty, so the auto-load fires again on
        // the very next sidecar event. With a token flapping once a second that
        // is a 403 per second against Apple, forever. Cleared by the section's
        // reload button, which is how a failure is meant to be retried.
        if self.tried_library {
            return;
        }
        // A library request without a user token cannot succeed — `/me/library`
        // answers 403 "Authentication required". Better to skip it than to
        // spend a round trip proving what the token already says.
        if tokens.music_user_token.is_none() {
            tracing::debug!("skipping library load: no user token yet");
            return;
        }
        self.tried_library = true;
        // Built per request rather than cached: the developer token is
        // re-harvested and can be replaced mid-session (rule 7), and a stale
        // client 401s in a way that looks like a sign-in problem.
        let client = Client::new(
            tokens.developer_token.clone(),
            tokens.music_user_token.clone(),
            tokens.storefront.clone(),
        );
        self.loading_library = true;
        tracing::info!("loading library");
        sender.oneshot_command(async move {
            CommandMsg::Library(
                client
                    .all_library_songs(LIBRARY_MAX)
                    .await
                    .map_err(|err| format!("{err:#}")),
            )
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::music::types::TrackId;

    fn track(title: &str, catalog: Option<&str>) -> Track {
        Track {
            id: TrackId(format!("i.{title}")),
            catalog_id: catalog.map(str::to_owned),
            title: title.into(),
            favorite: false,
            in_library: false,
            library_id: None,
            date_added: String::new(),
            year: String::new(),
            artist: "Aitana".into(),
            album: "Superestrella".into(),
            duration_ms: 200_000,
            track_number: 1,
            artwork: None,
        }
    }

    /// A page of results with a distinct count per kind, so a test that reads
    /// the wrong one fails loudly rather than coincidentally passing.
    fn page(songs: usize, albums: usize, artists: usize, playlists: usize) -> SearchResults {
        use crate::music::types::{Album, Artist, Playlist};
        SearchResults {
            songs: (0..songs)
                .map(|i| track(&format!("s{i}"), Some("1")))
                .collect(),
            albums: (0..albums)
                .map(|i| Album {
                    id: format!("al{i}"),
                    name: format!("al{i}"),
                    artist: String::new(),
                    artwork: None,
                    year: String::new(),
                    library: false,
                    track_count: 1,
                })
                .collect(),
            artists: (0..artists)
                .map(|i| Artist {
                    id: format!("ar{i}"),
                    name: format!("ar{i}"),
                    artwork: None,
                    genres: String::new(),
                    library: false,
                })
                .collect(),
            playlists: (0..playlists)
                .map(|i| Playlist {
                    id: format!("pl{i}"),
                    name: format!("pl{i}"),
                    curator: String::new(),
                    description: String::new(),
                    artwork: None,
                    library: false,
                })
                .collect(),
        }
    }

    fn kinds(rows: &[Entry]) -> Vec<&'static str> {
        rows.iter()
            .map(|e| match e {
                Entry::Song(_) => "song",
                Entry::Album(_) => "album",
                Entry::Artist(_) => "artist",
                Entry::Playlist(_) => "playlist",
            })
            .collect()
    }

    #[test]
    fn unfiltered_results_lead_with_trimmed_browse_rows() {
        // Artists, playlists and albums are doors: a few of each on top, then
        // the songs. Trimmed, or 25 songs bury them.
        let (rows, paged) = catalog_rows(CatalogFilter::All, page(5, 9, 9, 9), true);
        let k = kinds(&rows);
        assert_eq!(
            k.iter().filter(|k| **k == "artist").count(),
            CATALOG_BROWSE_ROWS
        );
        assert_eq!(
            k.iter().filter(|k| **k == "album").count(),
            CATALOG_BROWSE_ROWS
        );
        assert_eq!(
            k.iter().filter(|k| **k == "playlist").count(),
            CATALOG_BROWSE_ROWS
        );
        assert_eq!(k.iter().filter(|k| **k == "song").count(), 5);
        // Songs last, browse rows first — never interleaved.
        assert_eq!(k[k.len() - 5..], ["song"; 5]);
        // Songs are what pages when nothing is filtered.
        assert_eq!(paged, 5);
    }

    #[test]
    fn later_pages_carry_songs_only() {
        // Apple returns the browse kinds again on every page. Appending them
        // would duplicate rows already on screen.
        let (rows, paged) = catalog_rows(CatalogFilter::All, page(25, 9, 9, 9), false);
        assert_eq!(kinds(&rows), ["song"; 25]);
        assert_eq!(paged, 25);
    }

    #[test]
    fn a_filtered_page_counts_its_own_kind_not_songs() {
        // The bug this function exists to prevent: paging by `songs.len()`
        // while showing albums walks the offset with the wrong number, so the
        // next page starts in the wrong place. Every count here is distinct.
        for (filter, kind, expected) in [
            (CatalogFilter::Songs, "song", 5),
            (CatalogFilter::Albums, "album", 6),
            (CatalogFilter::Artists, "artist", 7),
            (CatalogFilter::Playlists, "playlist", 8),
        ] {
            let (rows, paged) = catalog_rows(filter, page(5, 6, 7, 8), true);
            assert_eq!(kinds(&rows), vec![kind; expected], "{filter:?} rows");
            assert_eq!(paged, expected, "{filter:?} offset");
        }
    }

    #[test]
    fn a_filtered_page_never_trims() {
        // The trim exists so browse rows do not bury the songs. Asking *for*
        // albums and getting three of them would be absurd.
        let (rows, paged) = catalog_rows(CatalogFilter::Albums, page(0, 25, 0, 0), true);
        assert_eq!(rows.len(), 25);
        assert_eq!(paged, 25);
    }

    #[test]
    fn search_matches_title_artist_and_album_case_insensitively() {
        let t = track("SUPERESTRELLA", Some("1"));
        assert!(matches(&t, "superestrella"), "title");
        assert!(matches(&t, "aitana"), "artist");
        assert!(!matches(&t, "superstrella"), "not fuzzy, by design");
        assert!(!matches(&t, "rosalia"));
    }
}
