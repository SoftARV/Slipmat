// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! What a list is ordered by, and which way round.

use serde::{Deserialize, Serialize};

use crate::ipc::View;
use crate::music::types::{Album, Artist, Playlist, Track};

/// How a library list is ordered.
///
/// Applied to *our* own types rather than asked of Apple: the whole library is
/// already in memory, sorting 500 of them is instant, and `/me/library/songs`
/// offers no sort parameter worth a round trip anyway.
///
/// **Here rather than in a client** because two of them need it now — the GTK
/// app sorts the cache it reads itself, and the daemon sorts what it serves
/// over `browse`. One set of comparators, so two frontends cannot disagree
/// about what "by artist" means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
    ///
    /// A client showing *search* results asks with `Songs`, because that is
    /// what they are.
    pub fn for_view(view: View) -> &'static [Self] {
        match view {
            View::Songs => &[Self::Title, Self::Artist, Self::Album, Self::Year],
            View::Albums => &[Self::Title, Self::Artist, Self::Year, Self::Added],
            View::Playlists => &[Self::Title, Self::Added, Self::Updated],
            View::Artists => &[Self::Title],
        }
    }

    /// The first key a section offers, used when a restored one does not apply
    /// to it — a playlist cannot sort by artist however the ini file reads.
    pub fn valid_for(self, view: View) -> Self {
        let allowed = Self::for_view(view);
        if allowed.contains(&self) {
            self
        } else {
            allowed[0]
        }
    }

    pub fn label(self) -> &'static str {
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
    pub fn id(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Artist => "artist",
            Self::Album => "album",
            Self::Year => "year",
            Self::Added => "added",
            Self::Updated => "updated",
        }
    }

    pub fn parse(s: &str) -> Self {
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
    pub fn descends_by_default(self) -> bool {
        matches!(self, Self::Year | Self::Added | Self::Updated)
    }

    /// Order two tracks. Every arm falls back to title, so the list is stable —
    /// two tracks that tie must not swap places between rebuilds.
    pub fn compare(self, a: &Track, b: &Track) -> std::cmp::Ordering {
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
    pub fn compare_album(self, a: &Album, b: &Album) -> std::cmp::Ordering {
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
    pub fn compare_playlist(self, a: &Playlist, b: &Playlist) -> std::cmp::Ordering {
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
    pub fn compare_artist(a: &Artist, b: &Artist) -> std::cmp::Ordering {
        a.name.to_lowercase().cmp(&b.name.to_lowercase())
    }
}
