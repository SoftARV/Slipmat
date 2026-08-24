// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! What the daemon knows about the library, and how a client asks for a slice
//! of it.

use slipmat_core::entry::Entry;
use slipmat_core::ipc::View;
use slipmat_core::library_cache;
use slipmat_core::music::types::{Album, Artist, Playlist, Track};

#[derive(Default)]
pub struct Library {
    pub tracks: Vec<Track>,
    pub albums: Vec<Album>,
    pub artists: Vec<Artist>,
    pub playlists: Vec<Playlist>,
}

impl Library {
    /// What was on disk last time. An empty library is not an error — it means
    /// the first browse waits on Apple instead of answering instantly.
    pub fn from_cache() -> Self {
        let cached = library_cache::load();
        Self {
            tracks: cached.songs,
            albums: cached.albums,
            artists: cached.artists,
            playlists: cached.playlists,
        }
    }

    /// One window of one section.
    ///
    /// Returns the total *before* the window, so a client can size a scrollbar
    /// without asking for 535 rows it will not draw.
    pub fn browse(
        &self,
        view: View,
        query: &str,
        offset: usize,
        limit: usize,
    ) -> (Vec<Entry>, usize) {
        let needle = query.trim().to_lowercase();
        let all: Vec<Entry> = match view {
            View::Songs => self
                .tracks
                .iter()
                .filter(|t| matches_track(t, &needle))
                .cloned()
                .map(Entry::Song)
                .collect(),
            View::Albums => self
                .albums
                .iter()
                .filter(|a| matches(&needle, &[&a.name, &a.artist]))
                .cloned()
                .map(Entry::Album)
                .collect(),
            View::Artists => self
                .artists
                .iter()
                .filter(|a| matches(&needle, &[&a.name]))
                .cloned()
                .map(Entry::Artist)
                .collect(),
            View::Playlists => self
                .playlists
                .iter()
                .filter(|p| matches(&needle, &[&p.name]))
                .cloned()
                .map(Entry::Playlist)
                .collect(),
        };

        let total = all.len();
        let window = all
            .into_iter()
            .skip(offset)
            // Zero means "the rest": a client that wants everything should not
            // have to guess a number bigger than the library.
            .take(if limit == 0 { usize::MAX } else { limit })
            .collect();
        (window, total)
    }
}

fn matches(needle: &str, fields: &[&str]) -> bool {
    needle.is_empty() || fields.iter().any(|f| f.to_lowercase().contains(needle))
}

fn matches_track(track: &Track, needle: &str) -> bool {
    matches(needle, &[&track.title, &track.artist, &track.album])
}

#[cfg(test)]
mod tests {
    use super::*;
    use slipmat_core::music::types::TrackId;

    fn track(title: &str, artist: &str) -> Track {
        Track {
            date_added: String::new(),
            year: String::new(),
            favorite: false,
            in_library: true,
            library_id: None,
            id: TrackId(title.into()),
            catalog_id: Some(title.into()),
            title: title.into(),
            artist: artist.into(),
            album: String::new(),
            duration_ms: 0,
            track_number: 0,
            artwork: None,
        }
    }

    fn library() -> Library {
        Library {
            tracks: vec![
                track("Wind Shear", "Pierce Fulton"),
                track("A Moment Apart", "ODESZA"),
                track("Colors", "Beck"),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn a_window_reports_the_total_before_it_was_windowed() {
        // A client draws a screenful and a scrollbar. The scrollbar needs the
        // number it did not receive.
        let (rows, total) = library().browse(View::Songs, "", 1, 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(total, 3);
    }

    #[test]
    fn a_zero_limit_means_the_rest_rather_than_nothing() {
        // The alternative is every client guessing a number larger than the
        // library, which is the kind of thing that works until someone has
        // 20,000 songs.
        let (rows, _) = library().browse(View::Songs, "", 0, 0);
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn the_query_reaches_past_the_title() {
        // Searching an artist you can spell but whose track titles you cannot
        // is the ordinary case, not the clever one.
        let (rows, total) = library().browse(View::Songs, "odesza", 0, 0);
        assert_eq!(total, 1);
        assert!(matches!(&rows[0], Entry::Song(t) if t.title == "A Moment Apart"));
    }

    #[test]
    fn an_offset_past_the_end_is_empty_rather_than_a_panic() {
        let (rows, total) = library().browse(View::Songs, "", 99, 10);
        assert!(rows.is_empty());
        assert_eq!(total, 3, "the total still describes the library");
    }
}
