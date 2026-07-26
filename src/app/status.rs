// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! What the content pane shows when it is not showing music, and the words on
//! it.
//!
//! Every one of these is asked **per section**. A global answer is how "Loading
//! your library" once covered the Apple Music pane and read as the whole app
//! being stuck — an empty Albums grid says nothing about whether Songs has 500
//! tracks in it.

use relm4::gtk;

use super::{AppModel, Stage, View};

impl AppModel {
    /// Is there anything for the content pane to show?
    ///
    /// Asked per section, not globally. The Albums grid being empty says
    /// nothing about whether the Songs list has 500 tracks in it, and a global
    /// answer is how "Loading your library" ended up covering the Apple Music
    /// pane.
    pub(super) fn showing_library(&self) -> bool {
        if !matches!(self.stage, Stage::Ready) {
            return false;
        }
        match self.view {
            View::Albums => !self.albums.is_empty() || self.loading_albums,
            View::Artists => !self.artists.is_empty() || self.loading_artists,
            _ => !self.all_tracks.is_empty(),
        }
    }

    pub(super) fn page(&self) -> &'static str {
        // Only the *first* load takes over the screen. A reload with content
        // already on show keeps it up and just disables the refresh button —
        // yanking the list away to show a spinner is worse. Paging in more
        // catalog results happens *below* a list the user is already reading,
        // and replacing that mid-scroll is worse than a moment with no new
        // rows.
        //
        // Each section answers for itself. The library loads at startup
        // whichever section you are in, and taking over the Apple Music pane to
        // say "Loading your library" reads as the whole app being stuck; the
        // sidebar spinners cover that instead.
        if !self.showing_library() {
            // A dead sidecar or a signed-out session outranks everything: no
            // section has anything to show.
            return "status";
        }

        match self.view {
            View::Songs => {
                if self.loading_library && self.all_tracks.is_empty() {
                    "loading"
                } else if self.library.is_empty() {
                    "no-results"
                } else {
                    "library"
                }
            }
            View::Albums => {
                if self.loading_albums && self.albums.is_empty() {
                    "loading"
                } else if self.album_grid.is_empty() {
                    "no-results"
                } else {
                    "albums"
                }
            }
            View::Artists => {
                if self.loading_artists && self.artists.is_empty() {
                    "loading"
                } else if self.artist_grid.is_empty() {
                    "no-results"
                } else {
                    "artists"
                }
            }
            View::Search => {
                if self.searching_catalog && self.catalog.is_empty() {
                    "loading"
                } else if self.catalog_query.trim().is_empty() {
                    // Nothing typed yet: invite a search rather than report a
                    // failed one.
                    "search-prompt"
                } else if self.library.is_empty() {
                    "no-results"
                } else {
                    "library"
                }
            }
        }
    }

    pub(super) fn icon(&self) -> &'static str {
        match self.stage {
            Stage::Ready => "audio-x-generic-symbolic",
            Stage::SignedOut => "avatar-default-symbolic",
            Stage::Broken(_) => "dialog-warning-symbolic",
            _ => "content-loading-symbolic",
        }
    }

    pub(super) fn headline(&self) -> String {
        match &self.stage {
            Stage::Starting => "Starting the playback engine".into(),
            Stage::InstallingWidevine => "Preparing playback".into(),
            Stage::Connecting => "Connecting to Apple Music".into(),
            Stage::SignedOut => "Sign in to Apple Music".into(),
            Stage::Restarting(n) => format!("Reconnecting (attempt {n})"),
            Stage::Broken(_) => "Playback unavailable".into(),
            Stage::Ready => self
                .player
                .now_playing
                .as_ref()
                .map(|i| i.title.clone())
                .unwrap_or_else(|| "Ready".into()),
        }
    }

    pub(super) fn detail(&self) -> String {
        match &self.stage {
            Stage::InstallingWidevine => {
                "Downloading the components needed for protected playback. \
                 This only happens once."
                    .into()
            }
            Stage::SignedOut => {
                "Apple's sign-in window opens once. After that Tonearm runs entirely \
                 in this window."
                    .into()
            }
            Stage::Broken(why) => why.clone(),
            Stage::Ready => self
                .player
                .now_playing
                .as_ref()
                // adw::StatusPage always parses its description as Pango
                // markup — there is no use-markup to turn off — so a track like
                // "Mercury - Acts 1 & 2" has to be escaped. It warns even while
                // this page is behind the library, because #[watch] still runs.
                .map(|i| {
                    gtk::glib::markup_escape_text(&format!("{} — {}", i.artist, i.album))
                        .to_string()
                })
                .unwrap_or_else(|| "Nothing playing".into()),
            _ => String::new(),
        }
    }

    pub(super) fn subtitle(&self) -> String {
        match &self.stage {
            Stage::Ready => self
                .tokens
                .as_ref()
                .map(|t| t.storefront.to_uppercase())
                .unwrap_or_default(),
            _ => String::new(),
        }
    }
}
