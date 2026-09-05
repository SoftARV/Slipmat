// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The library, kept between runs so the app opens on it.
//!
//! Measured before this existed: ~1.5s of sidecar boot, then ~1.5s of fetching
//! 534 songs that had not changed since the last launch — about 3.2s of spinner
//! every time. The songs, albums, artists and playlists are all rederivable
//! (Apple will tell us again), so this is a **cache**, beside `unplayable.json`
//! and the artwork, and never `$XDG_STATE_HOME`.
//!
//! Read tolerantly and written best-effort: losing it costs one slow launch.
//! Catalog ids and album titles are not secrets, so rule 7 is not engaged —
//! there is nothing here that would matter if someone read the file.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::music::types::{Album, Artist, Playlist, Track};

/// Bumped when a field changes *meaning*. Additions are handled by
/// `#[serde(default)]` and need no bump; a rename or a reinterpretation would
/// otherwise be read as valid data that quietly says the wrong thing.
const VERSION: u32 = 1;

/// Everything the four sidebar sections are built from.
#[derive(Debug, Default, Deserialize)]
pub struct Library {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    pub songs: Vec<Track>,
    #[serde(default)]
    pub albums: Vec<Album>,
    #[serde(default)]
    pub artists: Vec<Artist>,
    #[serde(default)]
    pub playlists: Vec<Playlist>,
}

impl Library {
    pub fn is_empty(&self) -> bool {
        self.songs.is_empty()
            && self.albums.is_empty()
            && self.artists.is_empty()
            && self.playlists.is_empty()
    }
}

/// Written by borrowing the model's own vectors, so saving never clones the
/// whole library to hand it to serde.
#[derive(Serialize)]
struct Writing<'a> {
    version: u32,
    songs: &'a [Track],
    albums: &'a [Album],
    artists: &'a [Artist],
    playlists: &'a [Playlist],
}

fn cache_file() -> Option<PathBuf> {
    Some(crate::paths::cache_dir()?.join("library.json"))
}

/// What we had last time. Any problem yields an empty library, which means the
/// app starts on a spinner exactly as it used to — never an error.
pub fn load() -> Library {
    let Some(path) = cache_file() else {
        return Library::default();
    };
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Library::default();
    };
    parse(&raw)
}

/// The half of `load` that is not I/O, so tests never touch a home directory.
fn parse(raw: &str) -> Library {
    match serde_json::from_str::<Library>(raw) {
        Ok(cache) if cache.version == VERSION => cache,
        Ok(cache) => {
            tracing::debug!(
                found = cache.version,
                want = VERSION,
                "library cache is old"
            );
            Library::default()
        }
        Err(err) => {
            tracing::debug!(?err, "library cache unreadable; ignoring");
            Library::default()
        }
    }
}

/// Persist what the model holds. Best-effort: a failure costs one slow launch
/// and must never interrupt anything.
///
/// An empty collection is written as empty, and read back as "not cached" — so
/// saving after only the songs have arrived does not tell the next launch that
/// there are no albums.
pub fn save(songs: &[Track], albums: &[Album], artists: &[Artist], playlists: &[Playlist]) {
    let Some(path) = cache_file() else { return };
    let started = std::time::Instant::now();
    let writing = Writing {
        version: VERSION,
        songs,
        albums,
        artists,
        playlists,
    };
    let Ok(json) = serde_json::to_string(&writing) else {
        return;
    };
    let bytes = json.len();
    if let Err(err) = crate::artwork::write_atomically(&path, json.as_bytes()) {
        tracing::debug!(?err, "could not save the library cache");
        return;
    }
    // Serialising and writing happen on the GTK thread, so the cost is worth
    // knowing rather than assuming. It runs at most four times a launch, each
    // right after a network load the user has already waited for.
    tracing::debug!(bytes, ms = started.elapsed().as_millis(), "library cached");
}

/// Forget it — the user signed out, and this was their music.
pub fn clear() {
    if let Some(path) = cache_file() {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::music::types::{Artwork, TrackId};

    fn a_track() -> Track {
        Track {
            date_added: "2024-01-02T03:04:05Z".into(),
            year: "2024".into(),
            favorite: true,
            in_library: true,
            library_id: Some("i.AbCd".into()),
            id: TrackId("i.AbCd".into()),
            catalog_id: Some("1440857781".into()),
            title: "Bloom".into(),
            artist: "Radiohead".into(),
            album: "The King of Limbs".into(),
            duration_ms: 328_000,
            track_number: 1,
            artwork: Some(Artwork::new("https://x/{w}x{h}bb.jpg")),
        }
    }

    fn write(library: &Library) -> String {
        serde_json::to_string(&Writing {
            version: library.version,
            songs: &library.songs,
            albums: &library.albums,
            artists: &library.artists,
            playlists: &library.playlists,
        })
        .unwrap()
    }

    #[test]
    fn a_library_survives_the_round_trip() {
        let saved = Library {
            version: VERSION,
            songs: vec![a_track()],
            albums: vec![Album {
                id: "l.abc".into(),
                date_added: "2024-01-02T03:04:05Z".into(),
                name: "The King of Limbs".into(),
                artist: "Radiohead".into(),
                artwork: Some(Artwork::new("https://y/{w}x{h}bb.jpg")),
                year: "2011".into(),
                track_count: 8,
                library: true,
            }],
            artists: vec![Artist {
                id: "r.abc".into(),
                name: "Radiohead".into(),
                artwork: None,
                genres: "Alternative".into(),
                library: true,
            }],
            playlists: vec![Playlist {
                id: "p.abc".into(),
                date_added: "2024-01-02T03:04:05Z".into(),
                last_modified: "2024-06-01T00:00:00Z".into(),
                name: "Game on".into(),
                curator: String::new(),
                description: String::new(),
                artwork: None,
                library: true,
            }],
        };
        let back = parse(&write(&saved));
        assert_eq!(back.songs.len(), 1);
        assert_eq!(back.songs[0], saved.songs[0]);
        assert_eq!(back.albums, saved.albums);
        assert_eq!(back.artists, saved.artists);
        assert_eq!(back.playlists, saved.playlists);
    }

    #[test]
    fn the_playable_id_survives_the_round_trip() {
        // The one field whose loss would be silent and expensive: a track with
        // no `catalog_id` renders as unplayable, so dropping it in the cache
        // would grey out a library that plays perfectly well.
        let back = parse(&write(&Library {
            version: VERSION,
            songs: vec![a_track()],
            ..Library::default()
        }));
        assert_eq!(back.songs[0].catalog_id.as_deref(), Some("1440857781"));
        assert!(back.songs[0].playable());
    }

    #[test]
    fn a_file_we_cannot_read_is_survivable() {
        // A truncated write, or a format from a future version. Refusing to
        // start over a cache file would be absurd.
        assert!(parse("").is_empty());
        assert!(parse("{").is_empty());
        assert!(parse(r#"{"version":1,"songs":"not a list"}"#).is_empty());
    }

    #[test]
    fn an_older_format_is_ignored_rather_than_misread() {
        let raw = write(&Library {
            version: VERSION - 1,
            songs: vec![a_track()],
            ..Library::default()
        });
        // Parses fine as JSON — it is the version that rejects it, which is the
        // whole point of having one.
        assert!(serde_json::from_str::<Library>(&raw).is_ok());
        assert!(parse(&raw).is_empty());
    }

    #[test]
    fn a_partial_cache_keeps_what_it_has() {
        // Saved after the songs arrived but before the albums did. The songs
        // must still come back; the empty collections just mean "fetch those".
        let back = parse(&write(&Library {
            version: VERSION,
            songs: vec![a_track()],
            ..Library::default()
        }));
        assert_eq!(back.songs.len(), 1);
        assert!(back.albums.is_empty());
        assert!(!back.is_empty());
    }
}
