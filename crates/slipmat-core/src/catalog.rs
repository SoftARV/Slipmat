// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Searching all of Apple Music, and the order the answers go in.
//!
//! Here rather than in a client because **this is the part that needs the
//! tokens**, and rule 7 keeps those in one process. The ordering came with it:
//! two clients deciding separately what sits above the songs is two answers to
//! one question.

use crate::entry::Entry;
use crate::ipc::CatalogFilter;
use crate::music::client::SearchResults;

/// Apple caps search at 25 results per request, so this is its ceiling rather
/// than a choice. More than that means paging with an offset.
pub const CATALOG_LIMIT: u32 = 25;

/// How many artists and albums to show above the songs. Enough to be a way in,
/// few enough that the songs are still visible without scrolling.
pub const CATALOG_BROWSE_ROWS: usize = 3;

/// is for when you want the list.
pub fn catalog_rows(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::music::types::{Track, TrackId};

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
                    date_added: String::new(),
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
                    date_added: String::new(),
                    last_modified: String::new(),
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
}
