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

/// Spinner or status page, while the sidecar is still coming up.
///
/// The distinction is whether waiting is the whole answer. Starting, fetching
/// the CDM and connecting all resolve by themselves in a few seconds — measured
/// at ~5s total, of which roughly 95% is Apple's own page and MusicKit booting,
/// so there is very little here to make faster. Those stages want motion.
///
/// Signed out, broken and reconnecting are different: each needs the user to
/// read something or do something, and a spinner over them would promise a
/// resolution that is not coming.
fn startup_page(stage: &Stage) -> &'static str {
    match stage {
        Stage::Starting | Stage::InstallingWidevine | Stage::Connecting => "loading",
        Stage::SignedOut | Stage::Broken(_) | Stage::Restarting(_) | Stage::Ready => "status",
    }
}

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
            View::Playlists => !self.playlists.is_empty() || self.loading_playlists,
            _ => !self.all_tracks.is_empty(),
        }
    }

    /// What the spinner page says it is waiting for.
    ///
    /// Bringing the sidecar up comes first: during those stages no section is
    /// loading anything, so naming the section would be a lie about what is
    /// actually slow.
    pub(super) fn waiting_for(&self) -> String {
        if !matches!(self.stage, Stage::Ready) {
            return self.headline();
        }
        match self.view {
            View::Search => "Searching Apple Music",
            View::Albums => "Loading your albums",
            View::Artists => "Loading your artists",
            View::Playlists => "Loading your playlists",
            View::Songs => "Loading your library",
        }
        .into()
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
            //
            // But *how* it says so depends on whether waiting is the answer.
            // Bringing the sidecar up takes about five seconds, measured, and
            // roughly 95% of that is Apple's own page and MusicKit booting —
            // there is almost nothing here to make faster. What there was to
            // fix is that those seconds were spent on a `StatusPage` whose icon
            // never moves, which reads as frozen rather than busy.
            //
            // So the transient stages get the spinner, and the ones that need a
            // decision — signed out, broken, reconnecting — keep the status
            // page, which can say what to do about it.
            return startup_page(&self.stage);
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
            View::Playlists => {
                if self.loading_playlists && self.playlists.is_empty() {
                    "loading"
                } else if self.playlist_grid.is_empty() {
                    "no-results"
                } else {
                    "playlists"
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
            // The app's own icon, not a generic avatar: this is the first
            // thing anyone sees, and it should say which program is asking.
            Stage::SignedOut => crate::APP_ID,
            Stage::Broken(_) => "dialog-warning-symbolic",
            _ => "content-loading-symbolic",
        }
    }

    pub(super) fn headline(&self) -> String {
        match &self.stage {
            Stage::Starting => "Starting the playback engine".into(),
            Stage::InstallingWidevine => "Preparing playback".into(),
            Stage::Connecting => "Connecting to Apple Music".into(),
            Stage::SignedOut => "Welcome to Tonearm".into(),
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
            // The onboarding proper. Two things a first-time reader needs and
            // cannot find out by clicking: that a subscription is required —
            // Tonearm is a front-end, not a source — and that everything after
            // sign-in happens here rather than in a browser.
            Stage::SignedOut => "Tonearm plays your Apple Music library natively on GNOME. \
                 It needs an active Apple Music subscription."
                .into(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_stages_that_resolve_themselves_get_a_spinner() {
        // Motion promises "wait and this will finish". True while the sidecar
        // is coming up; a lie for the three that need the user to read or do
        // something, and a spinner over those is how a stuck app looks busy.
        for stage in [
            Stage::Starting,
            Stage::InstallingWidevine,
            Stage::Connecting,
        ] {
            assert_eq!(startup_page(&stage), "loading", "{stage:?}");
        }
        for stage in [
            Stage::SignedOut,
            Stage::Broken("Apple changed the page".into()),
            Stage::Restarting(2),
        ] {
            assert_eq!(startup_page(&stage), "status", "{stage:?}");
        }
    }
}
