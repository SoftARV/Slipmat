// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! A row in a list of things, whatever list it is.
//!
//! Every list Slipmat shows — the library, a search, an album's tracks — is a
//! sequence of these, and every one of them can become a queue. It lives here
//! rather than beside a widget because it is a sum of four types that are
//! already here, and because a client that draws rows in a terminal needs
//! exactly the same enum.

use serde::{Deserialize, Serialize};

use crate::ipc::PageKind;
use crate::music::types::{Album, Artist, Playlist, Track};

/// What a row stands for. Songs play; everything else opens a page.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Entry {
    Song(Track),
    Album(Album),
    Artist(Artist),
    Playlist(Playlist),
}

impl Entry {
    /// The id used to match a row against what is playing. Only songs have one.
    pub fn catalog_id(&self) -> Option<&str> {
        match self {
            Entry::Song(track) => track.catalog_id.as_deref(),
            _ => None,
        }
    }

    pub fn title(&self) -> &str {
        match self {
            Entry::Song(t) => &t.title,
            Entry::Album(a) => &a.name,
            Entry::Artist(a) => &a.name,
            Entry::Playlist(p) => &p.name,
        }
    }

    /// The second line. Collapses rather than rendering a dangling separator
    /// when a field is missing, which real catalogue entries often are.
    pub fn subtitle(&self) -> String {
        match self {
            Entry::Song(t) => match (t.artist.is_empty(), t.album.is_empty()) {
                (false, false) => format!("{} — {}", t.artist, t.album),
                (false, true) => t.artist.clone(),
                (true, false) => t.album.clone(),
                (true, true) => String::new(),
            },
            Entry::Album(a) => match (a.artist.is_empty(), a.year.is_empty()) {
                (false, false) => format!("{} · {}", a.artist, a.year),
                (false, true) => a.artist.clone(),
                (true, false) => a.year.clone(),
                (true, true) => String::new(),
            },
            Entry::Artist(a) => a.genres.clone(),
            // The curator is the useful line — Apple's editors made most of
            // what a catalogue search returns. The blurb is prose and belongs
            // on the page, not in a row.
            Entry::Playlist(p) => p.curator.clone(),
        }
    }

    /// Albums and artists lead somewhere; songs are the destination.
    pub fn opens_a_page(&self) -> bool {
        !matches!(self, Entry::Song(_))
    }

    /// What opening this row asks the daemon for, if it opens anything.
    ///
    /// **The library/catalog split comes from the flag, never from the id.**
    /// The two id spaces are not interchangeable — a library id 404s against
    /// `/catalog` and a catalog id against `/me/library` — and the flag is set
    /// at the parse site by whichever call fetched the row, which is the only
    /// place that actually knows.
    pub fn page_target(&self) -> Option<(PageKind, String)> {
        match self {
            Entry::Song(_) => None,
            Entry::Album(a) => Some((
                if a.library {
                    PageKind::LibraryAlbum
                } else {
                    PageKind::Album
                },
                a.id.clone(),
            )),
            Entry::Artist(a) => Some((
                if a.library {
                    PageKind::LibraryArtist
                } else {
                    PageKind::Artist
                },
                a.id.clone(),
            )),
            Entry::Playlist(p) => Some((
                if p.library {
                    PageKind::LibraryPlaylist
                } else {
                    PageKind::Playlist
                },
                p.id.clone(),
            )),
        }
    }
}
