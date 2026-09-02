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

use super::{AppModel, CommandMsg, SearchScope, SortBy, Tile, View};
use crate::components::grid_item::{ArtRegistry, GridItem};
use crate::components::track_row::{Entry, LibraryItem, apply_row_state};
use slipmat_core::ipc::Request;
use slipmat_core::music::types::Track;

/// Fold one page of catalog results into rows, and report how many of the
/// **paging kind** came back.
///
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
    /// Show what the daemon found in the catalog.
    ///
    /// **Checked against the box, not against a generation counter.** The
    /// daemon answers with the query it searched for, so a slow reply for
    /// "aita" cannot land on the results for "aitana" — which is the same
    /// guarantee the counter gave, made by the side that knows.
    pub(super) fn fill_catalog(
        &mut self,
        query: &str,
        entries: Vec<Entry>,
        offset: usize,
        more: bool,
    ) {
        if query != self.query().trim() {
            tracing::debug!(%query, "discarding results for a query that moved on");
            return;
        }
        self.searching_catalog = false;
        self.catalog_exhausted = !more;
        if offset == 0 {
            self.catalog = entries;
            self.rebuild_rows();
        } else {
            self.append_rows(&entries);
            self.catalog.extend(entries);
        }
    }

    /// Re-read the library the daemon keeps on disk.
    ///
    /// **This client does not fetch any more.** One process asks Apple and
    /// writes the cache; this one reads it and does its own filtering, sorting
    /// and grid building, which is presentation rather than something to ask
    /// across a socket.
    pub(super) fn reload_from_cache(&mut self, sender: &ComponentSender<Self>) {
        let cached = slipmat_core::library_cache::load();
        tracing::info!(
            songs = cached.songs.len(),
            albums = cached.albums.len(),
            artists = cached.artists.len(),
            playlists = cached.playlists.len(),
            "read the library from cache"
        );
        self.all_tracks = cached.songs;
        self.albums = cached.albums;
        self.artists = cached.artists;
        self.playlists = cached.playlists;
        self.built_rows = None;
        self.built_albums = None;
        self.built_artists = None;
        self.built_playlists = None;
        self.rebuild_rows();
        self.rebuild_albums();
        self.rebuild_artists();
        self.rebuild_playlists();
        // A pin whose playlist is gone (#133). The cache read is when we know
        // what still exists, which is what the playlist fetch used to be.
        self.prune_stale_pins(sender);
        self.maybe_prune_artwork(sender);
    }

    /// The query for whichever scope is showing.
    pub(super) fn query(&self) -> &str {
        match self.scope() {
            SearchScope::Library => &self.library_query,
            SearchScope::Catalog => &self.catalog_query,
        }
    }

    /// What the results list shows, in order.
    ///
    /// Filtering reads `all_tracks`, never the factory, so clearing a search
    /// restores everything rather than whatever survived the last narrowing.
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
                let sort = self.sorts.get(self.view);
                tracks.sort_by(|a, b| sort.by.compare(a, b));
                // Reversed rather than sorted the other way, so ties keep the
                // stable order the comparator already gave them.
                if sort.by.descends_by_default() != sort.reversed {
                    tracks.reverse();
                }
                tracks.into_iter().map(Entry::Song).collect()
            }
            // Apple already ranked these; filtering them again locally would
            // only throw away results that matched for reasons we cannot see.
            SearchScope::Catalog => self.catalog.clone(),
        }
    }

    /// Ask the daemon to search the catalog.
    ///
    /// **The tokens for this live there** (rule 7), which is why a request
    /// crosses the socket rather than the credentials doing it. The answer
    /// carries the query it searched for, so this side can tell a result for
    /// the word in the box from one for two keystrokes ago.
    pub(super) fn run_catalog_search(
        &mut self,
        _sender: &ComponentSender<Self>,
        _generation: u64,
        offset: usize,
    ) {
        let term = self.query().trim().to_owned();
        if term.is_empty() {
            return;
        }
        self.searching_catalog = true;
        self.ask(Request::Search {
            query: term,
            filter: self.catalog_filter.into(),
            offset,
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
        let overrides = self.row_overrides.clone();
        self.library
            .extend_from_iter(new.iter().cloned().map(|entry| {
                LibraryItem::new(
                    entry,
                    registry.clone(),
                    current.clone(),
                    dead.clone(),
                    overrides.clone(),
                )
            }));
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
            self.sorts.get(self.view).by.id(),
            self.sorts.get(self.view).reversed,
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
        let overrides = self.row_overrides.clone();
        self.library.clear();
        self.library
            .extend_from_iter(visible.into_iter().map(|track| {
                LibraryItem::new(
                    track,
                    registry.clone(),
                    current.clone(),
                    dead.clone(),
                    overrides.clone(),
                )
            }));
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

    /// Rebuild the album grid from `albums` + the query.
    pub(super) fn rebuild_albums(&mut self) {
        // Already showing exactly this? Then the widgets are correct and
        // rebuilding them would only re-decode every cover — see `built_albums`.
        // The sort is part of the fingerprint: the widgets already on screen
        // may be the right ones in the wrong order.
        let sort = self.sorts.get(View::Albums);
        let fingerprint = format!(
            "{}\u{1}{}\u{1}{}",
            self.library_query.trim().to_lowercase(),
            sort.by.id(),
            sort.reversed
        );
        if self.built_albums.as_deref() == Some(fingerprint.as_str()) {
            return;
        }
        self.built_albums = Some(fingerprint);

        let started = std::time::Instant::now();
        let _timed = crate::app::Timed("albums", started);

        let needle = self.library_query.trim().to_lowercase();
        let mut albums: Vec<_> = self
            .albums
            .iter()
            .filter(|a| {
                needle.is_empty()
                    || a.name.to_lowercase().contains(&needle)
                    || a.artist.to_lowercase().contains(&needle)
            })
            .cloned()
            .collect();
        // Sorted on the domain objects rather than the tiles: a `Tile` is a
        // display shape and the keys live on the album. Reversed rather than
        // sorted the other way, so ties keep the comparator's stable order —
        // the same discipline the songs list follows.
        albums.sort_by(|a, b| sort.by.compare_album(a, b));
        if sort.by.descends_by_default() != sort.reversed {
            albums.reverse();
        }
        let tiles: Vec<Tile> = albums.into_iter().map(Tile::Album).collect();
        self.album_grid.clear();
        self.album_art_widgets.borrow_mut().clear();
        let items = self.grid_items(tiles, &self.album_art_widgets);
        self.album_grid.extend_from_iter(items);
    }

    pub(super) fn rebuild_playlists(&mut self) {
        // Already showing exactly this? Then the widgets are correct and
        // rebuilding them would only re-decode every cover — see `built_playlists`.
        // The sort is part of the fingerprint: the widgets already on screen
        // may be the right ones in the wrong order.
        let sort = self.sorts.get(View::Playlists);
        let fingerprint = format!(
            "{}\u{1}{}\u{1}{}",
            self.library_query.trim().to_lowercase(),
            sort.by.id(),
            sort.reversed
        );
        if self.built_playlists.as_deref() == Some(fingerprint.as_str()) {
            return;
        }
        self.built_playlists = Some(fingerprint);

        let started = std::time::Instant::now();
        let _timed = crate::app::Timed("playlists", started);

        let needle = self.library_query.trim().to_lowercase();
        let mut playlists: Vec<_> = self
            .playlists
            .iter()
            .filter(|p| {
                needle.is_empty()
                    || p.name.to_lowercase().contains(&needle)
                    || p.curator.to_lowercase().contains(&needle)
            })
            .cloned()
            .collect();
        playlists.sort_by(|a, b| sort.by.compare_playlist(a, b));
        if sort.by.descends_by_default() != sort.reversed {
            playlists.reverse();
        }
        let tiles: Vec<Tile> = playlists.into_iter().map(Tile::Playlist).collect();
        self.playlist_grid.clear();
        self.playlist_art_widgets.borrow_mut().clear();
        let items = self.grid_items(tiles, &self.playlist_art_widgets);
        self.playlist_grid.extend_from_iter(items);
    }

    pub(super) fn rebuild_artists(&mut self) {
        // Already showing exactly this? Then the widgets are correct and
        // rebuilding them would only re-decode every cover — see `built_artists`.
        // Only a direction to remember: a library artist carries nothing but a
        // name, so there is no key to choose between.
        let sort = self.sorts.get(View::Artists);
        let fingerprint = format!(
            "{}\u{1}{}",
            self.library_query.trim().to_lowercase(),
            sort.reversed
        );
        if self.built_artists.as_deref() == Some(fingerprint.as_str()) {
            return;
        }
        self.built_artists = Some(fingerprint);

        let started = std::time::Instant::now();
        let _timed = crate::app::Timed("artists", started);

        let needle = self.library_query.trim().to_lowercase();
        let mut artists: Vec<_> = self
            .artists
            .iter()
            .filter(|a| needle.is_empty() || a.name.to_lowercase().contains(&needle))
            .cloned()
            .collect();
        // No key to choose, so no `descends_by_default` either: the toggle is
        // the whole control, and it means A–Z or Z–A.
        artists.sort_by(SortBy::compare_artist);
        if sort.reversed {
            artists.reverse();
        }
        let tiles: Vec<Tile> = artists.into_iter().map(Tile::Artist).collect();
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
                // No artwork cache handed over any more: a tile never reads
                // one, because it no longer decides whether to load a cover
                // itself. It asks, and the answer arrives decoded (#27).
                GridItem::new(tile, registry.clone(), self.tile_art_request.clone())
            })
            .collect()
    }

    /// Fill the four collections from disk, before the sidecar is even up.
    ///
    /// Called once, from `init`. Nothing here decides *not* to fetch — the
    /// loaders' `!is_empty()` guards do that on their own, which is the same
    /// rule that makes revisiting a section instant.
    pub(super) fn seed_from_cache(&mut self) {
        let cached = slipmat_core::library_cache::load();
        if cached.is_empty() {
            return;
        }
        tracing::info!(
            songs = cached.songs.len(),
            albums = cached.albums.len(),
            artists = cached.artists.len(),
            playlists = cached.playlists.len(),
            "opened on the cached library"
        );
        self.all_tracks = cached.songs;
        self.albums = cached.albums;
        self.artists = cached.artists;
        self.playlists = cached.playlists;
        // Only the section being opened into: the other three cost ~500ms each
        // in cover decoding, and `SetView` builds them on the way in. Doing all
        // four here would spend that before the window is even mapped.
        match self.view {
            View::Albums => self.rebuild_albums(),
            View::Artists => self.rebuild_artists(),
            View::Playlists => self.rebuild_playlists(),
            View::Songs | View::Search => self.rebuild_rows(),
        }
    }

    /// Every artwork the library can account for, as cache keys.
    ///
    /// Tracks are in it as well as the three grids: a playlist's mosaic is
    /// drawn from its first four *tracks'* covers, so leaving them out would
    /// make every mosaic evictable the moment the cache went over its cap.
    fn artwork_keys(&self) -> std::collections::HashSet<String> {
        let mut keys = std::collections::HashSet::new();
        for art in self.all_tracks.iter().filter_map(|t| t.artwork.as_ref()) {
            keys.insert(art.cache_key());
        }
        for art in self.albums.iter().filter_map(|a| a.artwork.as_ref()) {
            keys.insert(art.cache_key());
        }
        for art in self.artists.iter().filter_map(|a| a.artwork.as_ref()) {
            keys.insert(art.cache_key());
        }
        for art in self.playlists.iter().filter_map(|p| p.artwork.as_ref()) {
            keys.insert(art.cache_key());
        }
        keys
    }

    /// Sweep the artwork cache, once a launch and only once every section has
    /// reported.
    ///
    /// **All four, or the keep-set is a lie.** Pruning after the songs alone
    /// would call every album and artist cover evictable, and the cache is what
    /// makes the grids fast — #27 measured 520ms against 75ms on exactly that.
    pub(super) fn maybe_prune_artwork(&mut self, sender: &ComponentSender<Self>) {
        let all_reported =
            self.tried_library && self.tried_albums && self.tried_artists && self.tried_playlists;
        if self.pruned || !all_reported {
            return;
        }
        self.pruned = true;
        let keep = self.artwork_keys();
        sender.oneshot_command(async move {
            CommandMsg::Pruned(
                relm4::spawn_blocking(move || crate::components::prune::run(&keep))
                    .await
                    .unwrap_or_default(),
            )
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slipmat_core::music::types::TrackId;

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

    #[test]
    fn search_matches_title_artist_and_album_case_insensitively() {
        let t = track("SUPERESTRELLA", Some("1"));
        assert!(matches(&t, "superestrella"), "title");
        assert!(matches(&t, "aitana"), "artist");
        assert!(!matches(&t, "superstrella"), "not fuzzy, by design");
        assert!(!matches(&t, "rosalia"));
    }
}
