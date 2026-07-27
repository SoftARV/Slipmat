// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Album and artist pages — the pages you push into from a search result.
//!
//! Not a relm4 `Component`. A component is a fixed slot in the widget tree, and
//! these are the opposite: created on a click, stacked, and dropped when you
//! navigate back. So this is a plain struct that owns its widgets and reports
//! clicks through closures the caller supplies.
//!
//! **Pages are addressed by id, never by their position in the stack.** Same
//! rule as everything else here: by the time a click arrives the stack may have
//! moved, and an index that was right when the widget was built is a wrong
//! answer that looks like a right one.
//!
//! The list inside is a `TypedListView` but **not virtualised** — it sits in a
//! `Box` under the header rather than being the scrollable child, so GTK asks
//! it for its full height. That is deliberate: an album has a dozen tracks and
//! an artist page twenty-odd albums, and a header that scrolls away with the
//! content is worth more than recycling thirty rows.

use relm4::gtk::prelude::*;
use relm4::typed_view::list::TypedListView;
use relm4::{adw, gtk};

use crate::components::cover::Cover;
use crate::components::track_row::{Entry, LibraryItem, LibraryRowWidgets};
use crate::components::{CurrentTrack, DeadTracks, RowRegistry};
use crate::music::types::{Album, Artist, Artwork, Playlist};

/// Header artwork, in logical pixels. The widget is pinned to exactly this so
/// the `card` background cannot outgrow the picture inside it.
const ART_PX: i32 = 160;

/// What a page is about — and everything needed to ask Apple for it again.
///
/// Catalog and library are separate variants rather than one variant plus a
/// flag, because they are genuinely different endpoints: a library id (`l.…`)
/// 404s against `/catalog`, and a catalog id 404s against `/me/library`. Making
/// the compiler ask which one you have keeps the two from being mixed up in a
/// year's time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageKind {
    Album(String),
    Artist(String),
    Playlist(String),
    LibraryAlbum(String),
    LibraryArtist(String),
    LibraryPlaylist(String),
}

impl PageKind {
    pub fn id(&self) -> &str {
        match self {
            Self::Album(id)
            | Self::Artist(id)
            | Self::Playlist(id)
            | Self::LibraryAlbum(id)
            | Self::LibraryArtist(id)
            | Self::LibraryPlaylist(id) => id,
        }
    }

    /// The right variant for an album, from the flag it was parsed with.
    pub fn album(album: &Album) -> Self {
        if album.library {
            Self::LibraryAlbum(album.id.clone())
        } else {
            Self::Album(album.id.clone())
        }
    }

    pub fn playlist(playlist: &Playlist) -> Self {
        if playlist.library {
            Self::LibraryPlaylist(playlist.id.clone())
        } else {
            Self::Playlist(playlist.id.clone())
        }
    }

    pub fn artist(artist: &Artist) -> Self {
        if artist.library {
            Self::LibraryArtist(artist.id.clone())
        } else {
            Self::Artist(artist.id.clone())
        }
    }

    /// What to put in the header until the real name arrives.
    pub fn heading(&self) -> &'static str {
        match self {
            Self::Album(_) | Self::LibraryAlbum(_) => "Album",
            Self::Artist(_) | Self::LibraryArtist(_) => "Artist",
            Self::Playlist(_) | Self::LibraryPlaylist(_) => "Playlist",
        }
    }
}

/// The row state shared with every other list: who is playing, and what cannot
/// be streamed. Deliberately *not* the widget registry — each list keeps its
/// own. One registry shared across lists would be keyed by catalog id, so the
/// same song on a page and in the results behind it would overwrite each
/// other's entry and the marker would appear on only one of them.
#[derive(Clone)]
pub struct RowState {
    pub current: CurrentTrack,
    pub dead: DeadTracks,
}

pub struct DetailPage {
    /// Stable for the page's whole life. Clicks quote it back.
    pub id: u64,
    /// What the list currently shows. The caller reads this to build a queue.
    pub entries: Vec<Entry>,

    page: adw::NavigationPage,
    list: TypedListView<LibraryItem, gtk::NoSelection>,
    state: RowState,
    registry: RowRegistry<LibraryRowWidgets>,

    header: adw::HeaderBar,
    stack: gtk::Stack,
    cover: Cover,
    title: gtk::Label,
    subtitle: gtk::Label,
    meta: gtk::Label,
    actions: gtk::Box,
    error: adw::StatusPage,
    empty: adw::StatusPage,
}

impl DetailPage {
    /// Build a page showing its spinner. The content arrives later, through
    /// [`DetailPage::show`].
    ///
    /// `on_activate` is handed the row index that was clicked; `on_play` and
    /// `on_shuffle` fire for the header's two buttons.
    pub fn new(
        id: u64,
        heading: &str,
        state: RowState,
        on_activate: impl Fn(usize) + 'static,
        on_play: impl Fn() + 'static,
        on_shuffle: impl Fn() + 'static,
    ) -> Self {
        let list: TypedListView<LibraryItem, gtk::NoSelection> = TypedListView::new();
        let view = list.view.clone();
        view.set_single_click_activate(true);
        view.add_css_class("navigation-sidebar");
        view.connect_activate(move |_, position| on_activate(position as usize));

        let cover = Cover::new(ART_PX);

        let title = gtk::Label::builder()
            .css_classes(["title-1"])
            .wrap(true)
            .justify(gtk::Justification::Center)
            .label(heading)
            .build();
        let subtitle = gtk::Label::builder()
            .css_classes(["title-4", "dim-label"])
            .wrap(true)
            .justify(gtk::Justification::Center)
            .build();
        let meta = gtk::Label::builder()
            .css_classes(["caption", "dim-label"])
            .build();

        let play = gtk::Button::builder()
            .label("Play")
            .css_classes(["suggested-action", "pill"])
            .build();
        play.connect_clicked(move |_| on_play());

        let shuffle = gtk::Button::builder()
            .icon_name("media-playlist-shuffle-symbolic")
            .tooltip_text("Shuffle")
            .css_classes(["pill"])
            .build();
        shuffle.connect_clicked(move |_| on_shuffle());

        // One box so both appear and disappear together — a Shuffle button
        // beside nothing is as useless as a Play button beside nothing.
        let actions = gtk::Box::builder()
            .spacing(6)
            .halign(gtk::Align::Center)
            .visible(false)
            .build();
        actions.append(&play);
        actions.append(&shuffle);

        let banner = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .halign(gtk::Align::Center)
            .margin_top(24)
            .margin_bottom(24)
            .build();
        cover.attach_first(&banner);
        banner.append(&title);
        banner.append(&subtitle);
        banner.append(&meta);
        banner.append(&actions);

        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        body.append(&banner);
        body.append(&view);

        let content = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&adw::Clamp::builder().maximum_size(800).child(&body).build())
            .build();

        let spinner = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        spinner.append(
            &adw::Spinner::builder()
                .width_request(42)
                .height_request(42)
                .build(),
        );

        let error = adw::StatusPage::builder()
            .icon_name("network-offline-symbolic")
            .title("Could not load this page")
            .build();

        // Distinct from `error`: a playlist you have not put anything in yet
        // loaded perfectly well. Without this it renders as a header floating
        // over nothing, which reads as a failure.
        let empty = adw::StatusPage::builder()
            .icon_name("folder-music-symbolic")
            .title("Nothing here yet")
            .build();

        let stack = gtk::Stack::new();
        stack.add_named(&spinner, Some("loading"));
        stack.add_named(&content, Some("content"));
        stack.add_named(&error, Some("error"));
        stack.add_named(&empty, Some("empty"));
        stack.set_visible_child_name("loading");

        let header = adw::HeaderBar::new();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&stack));

        let page = adw::NavigationPage::builder()
            .title(heading)
            // The tag is how a pop finds its way back to this struct. An id,
            // not a depth — see the module docs.
            .tag(id.to_string())
            .child(&toolbar)
            .build();

        Self {
            id,
            entries: Vec::new(),
            page,
            list,
            state,
            registry: crate::components::row_registry(),
            cover,
            header,
            stack,
            title,
            subtitle,
            meta,
            actions,
            error,
            empty,
        }
    }

    pub fn widget(&self) -> &adw::NavigationPage {
        &self.page
    }

    /// Whether this page's header draws the window controls. False while the
    /// queue is open: the queue is then the rightmost pane and they are its.
    pub fn set_end_controls(&self, show: bool) {
        self.header.set_show_end_title_buttons(show);
    }

    /// Update a track's favourite flag in this page's own copy of the list, so
    /// a later rebind does not undo what the user just did.
    pub fn set_favorite(&mut self, catalog_id: &str, on: bool) {
        for entry in &mut self.entries {
            if let Entry::Song(track) = entry
                && track.catalog_id.as_deref() == Some(catalog_id)
            {
                track.favorite = on;
            }
        }
    }

    /// As [`Self::set_favorite`], for library membership — which the row menu
    /// reads to decide whether "Add to Library" is worth offering.
    pub fn set_in_library(&mut self, catalog_id: &str, in_library: bool) {
        for entry in &mut self.entries {
            if let Entry::Song(track) = entry
                && track.catalog_id.as_deref() == Some(catalog_id)
            {
                track.in_library = in_library;
            }
        }
    }

    /// This page's own row widgets, so the play marker can find them.
    pub fn registry(&self) -> &RowRegistry<LibraryRowWidgets> {
        &self.registry
    }

    /// Fill an album page: cover, artist, year, and its tracks.
    pub fn show_album(&mut self, album: &Album, tracks: Vec<Entry>) {
        self.cover.square("media-optical-symbolic");
        self.head(&album.name, &album.artist, album.artwork.as_ref());

        let songs = tracks.len();
        let mut meta = String::new();
        if !album.year.is_empty() {
            meta.push_str(&album.year);
        }
        if songs > 0 {
            if !meta.is_empty() {
                meta.push_str(" · ");
            }
            meta.push_str(&format!(
                "{songs} {}",
                if songs == 1 { "song" } else { "songs" }
            ));
        }
        self.meta.set_label(&meta);
        self.meta.set_visible(!meta.is_empty());

        self.set_empty_kind("album");
        self.fill(tracks);
    }

    /// What the empty state calls the thing that is empty.
    fn set_empty_kind(&self, plural: &str) {
        self.empty
            .set_description(Some(&format!("This {plural} has no songs.")));
    }

    /// Fill a playlist page: cover, curator or blurb, and its tracks.
    pub fn show_playlist(&mut self, playlist: &Playlist, tracks: Vec<Entry>) {
        self.cover.square("view-list-symbolic");
        // Unlike the tile, a page *can* show the blurb: its subtitle label
        // wraps and is centred, which is where a sentence belongs. The curator
        // still wins when there is one.
        let subtitle = if playlist.curator.is_empty() {
            &playlist.description
        } else {
            &playlist.curator
        };
        self.head(&playlist.name, subtitle, playlist.artwork.as_ref());

        let songs = tracks.len();
        self.meta.set_label(&format!(
            "{songs} {}",
            if songs == 1 { "song" } else { "songs" }
        ));
        self.meta.set_visible(songs > 0);

        self.set_empty_kind("playlist");
        self.fill(tracks);
    }

    /// Fill an artist page: portrait, genres, and their albums.
    pub fn show_artist(&mut self, artist: &Artist, albums: Vec<Entry>) {
        // A round portrait, the way every other GNOME app shows a person —
        // and an `adw::Avatar`, which is the only way to actually get one. See
        // `components::cover`.
        self.cover.round(&artist.name);
        self.head(&artist.name, &artist.genres, artist.artwork.as_ref());

        let count = albums.len();
        self.meta.set_label(&format!(
            "{count} {}",
            if count == 1 { "album" } else { "albums" }
        ));
        self.meta.set_visible(count > 0);

        self.empty
            .set_description(Some("Apple Music lists no albums for this artist."));
        self.fill(albums);
    }

    fn head(&mut self, title: &str, subtitle: &str, artwork: Option<&Artwork>) {
        // Spelled out: `set_title` also exists on the window trait in scope,
        // and there it means the *window* title.
        adw::prelude::NavigationPageExt::set_title(&self.page, title);
        self.title.set_label(title);
        self.subtitle.set_label(subtitle);
        self.subtitle.set_visible(!subtitle.is_empty());
        // Artwork lands separately once it is on disk (see `set_artwork`) — the
        // page has to be readable before the network says anything. `artwork`
        // is only consulted for whether one is coming at all.
        let _ = artwork;
    }

    fn fill(&mut self, entries: Vec<Entry>) {
        self.list.clear();
        // The rows about to be discarded owned those widgets; none of them are
        // ours now.
        self.registry.borrow_mut().clear();
        let items = entries.iter().cloned().map(|entry| {
            LibraryItem::new(
                entry,
                self.registry.clone(),
                self.state.current.clone(),
                self.state.dead.clone(),
            )
        });
        self.list.extend_from_iter(items);

        // Only offer them where there is something to play — an artist page
        // lists albums, and a Play button that does nothing is a bug you have
        // to click to find.
        self.actions
            .set_visible(entries.iter().any(|e| e.catalog_id().is_some()));

        let anything = !entries.is_empty();
        self.entries = entries;
        self.stack
            .set_visible_child_name(if anything { "content" } else { "empty" });
    }

    /// Show the cover, once it has been fetched to disk.
    pub fn set_artwork(&self, path: &std::path::Path) {
        if path.is_file() {
            self.cover.set_file(path);
        }
    }

    pub fn fail(&self, message: &str) {
        self.error.set_description(Some(message));
        self.stack.set_visible_child_name("error");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_asks_the_collection_its_id_came_from() {
        // Library ids 404 against /catalog and vice versa, so the flag set at
        // parse time — not the id's shape — decides the endpoint.
        let mut album = Album {
            id: "1234".into(),
            name: "Superestrella".into(),
            artist: "Aitana".into(),
            artwork: None,
            year: "2020".into(),
            track_count: 12,
            library: false,
        };
        assert_eq!(PageKind::album(&album), PageKind::Album("1234".into()));
        album.library = true;
        album.id = "l.1234".into();
        assert_eq!(
            PageKind::album(&album),
            PageKind::LibraryAlbum("l.1234".into())
        );

        let mut artist = Artist {
            id: "9".into(),
            name: "Aitana".into(),
            artwork: None,
            genres: String::new(),
            library: false,
        };
        assert_eq!(PageKind::artist(&artist), PageKind::Artist("9".into()));
        artist.library = true;
        artist.id = "r.9".into();
        assert_eq!(
            PageKind::artist(&artist),
            PageKind::LibraryArtist("r.9".into())
        );
    }
}
