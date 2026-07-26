// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Where the user is: which sidebar section is showing, and what that implies.
//!
//! Small enough to inline, kept apart because it is the one piece of app state
//! with a *persisted* representation ([`Section`]) and a *widget*
//! representation (the sidebar row index). Three encodings of the same idea is
//! exactly where they drift, so the conversions and the tests that pin them
//! live together.

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
}

impl View {
    /// Sidebar row order — the contract `connect_row_selected` reads, and the
    /// only place the mapping lives.
    pub(super) fn from_row(index: i32) -> Self {
        match index {
            0 => Self::Search,
            2 => Self::Albums,
            3 => Self::Artists,
            _ => Self::Songs,
        }
    }

    pub(super) fn row(self) -> i32 {
        match self {
            Self::Search => 0,
            Self::Songs => 1,
            Self::Albums => 2,
            Self::Artists => 3,
        }
    }

    pub(super) fn scope(self) -> SearchScope {
        match self {
            Self::Search => SearchScope::Catalog,
            _ => SearchScope::Library,
        }
    }
}

impl From<Section> for View {
    fn from(section: Section) -> Self {
        match section {
            Section::Library => Self::Songs,
            Section::Albums => Self::Albums,
            Section::Artists => Self::Artists,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sidebar_row_order_round_trips() {
        // `connect_row_selected` reads a row index and `row()` writes one back
        // when restoring the last section. If those two ever disagree the app
        // opens with one section selected and another one showing — which is
        // exactly the bug this pins down.
        for view in [View::Search, View::Songs, View::Albums, View::Artists] {
            assert_eq!(View::from_row(view.row()), view);
        }
    }

    #[test]
    fn an_unknown_sidebar_row_falls_back_to_songs() {
        assert_eq!(View::from_row(99), View::Songs);
        assert_eq!(View::from_row(-1), View::Songs);
    }

    #[test]
    fn the_view_round_trips_through_the_persisted_section() {
        for view in [View::Search, View::Songs, View::Albums, View::Artists] {
            assert_eq!(View::from(Section::from(view)), view);
        }
    }

    #[test]
    fn only_search_looks_at_the_catalog() {
        assert_eq!(View::Search.scope(), SearchScope::Catalog);
        for view in [View::Songs, View::Albums, View::Artists] {
            assert_eq!(view.scope(), SearchScope::Library);
        }
    }
}
