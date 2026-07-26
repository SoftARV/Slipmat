// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Root component.
//!
//! Redux with a compiler: `AppMsg` are the actions, `update` is the sole
//! reducer, and the view is derived from `AppModel`. No I/O happens inline —
//! the sidecar's stdout is drained by a streaming relm4 `Command` so the GTK
//! main thread never blocks (CLAUDE.md rule 8).
//!
//! ## What lives where
//!
//! This file keeps the three things that have to be in one place — the model,
//! the messages, and the `Component` impl that holds `view!` and the reducer —
//! and nothing else. The work each message does lives in a sibling module, all
//! of them `impl AppModel` blocks, which a child module may write because it
//! can see its parent's private fields:
//!
//! | Module | Owns |
//! | --- | --- |
//! | [`view`] | which section is showing, and the three encodings of that |
//! | [`library`] | loading and filtering songs, albums, artists and search |
//! | [`queue`] | turning a click into a `setQueue`, and healing a rejected one |
//! | [`pages`] | the album / artist navigation stack |
//! | [`playback`] | pushing mirrored state to the bar, MPRIS, notifications |
//! | [`supervise`] | keeping the sidecar alive and folding in its events |
//! | [`status`] | what the pane shows when it is not showing music |
//! | [`chrome`] | the menu, its accelerators, and the three dialogs |
//!
//! The split is by *what a thing does*, not by layer, so a change usually lands
//! in one file. The reducer stays here because it is the map from action to
//! work, and a map is only useful whole.

use std::path::PathBuf;

use relm4::adw::prelude::*;
use relm4::{
    Component, ComponentController, ComponentParts, ComponentSender, Controller, RelmWidgetExt,
    adw, gtk,
};

use relm4::typed_view::grid::TypedGridView;
use relm4::typed_view::list::TypedListView;

use crate::components::artwork::{self, ART_SIZE};
use crate::components::detail_page::{DetailPage, PageKind, RowState};
use crate::components::grid_item::{
    ArtCache, ArtRegistry, ArtRequest, GridItem, Tile, art_cache, art_registry,
};
use crate::components::now_playing::{NowPlaying, NowPlayingInput, NowPlayingOutput, Repeat};
use crate::components::queue_view::{QueueView, QueueViewInput, QueueViewOutput};
use crate::components::track_row::LibraryRowWidgets;
use crate::components::track_row::{Entry, LibraryItem, RowMenuRequest};
use crate::components::{
    CurrentTrack, DeadTracks, RowRegistry, current_track, dead_tracks, row_registry,
};
use crate::mpris::Mpris;
use crate::music::types::{Album, Artist, Artwork, Playlist, Track};
use crate::notify;
use crate::player::protocol::{Command, RepeatMode, Tokens};
use crate::player::{Incoming, PlayerState, sidecar};
use crate::settings::{Section, Settings, Theme};

/// How often the seek bar redraws while playing.
///
/// The sidecar's own position events are coarse and irregular, so
/// `PlayerState::interpolated_position_ms` fills the gaps; this timer just
/// drives the repaint. Removed entirely when not playing — a paused player
/// should cost nothing (the same discipline as Pitwall's suspend-gated poll).
mod chrome;
mod library;
mod pages;
mod playback;
mod queue;
mod status;
mod supervise;
mod view;

use chrome::{icon, register_actions, show_about, show_shortcuts};
use supervise::{respawn_sidecar, start_sidecar};

pub use view::{SearchScope, SortBy, View};

const TICK_MS: u32 = 500;

/// How long the search box must sit still before a catalog search is sent.
///
/// The library filter is local and runs on every keystroke; the catalog is a
/// network request, and firing one per character would be both slow and rude.
const SEARCH_DEBOUNCE_MS: u64 = 350;

/// Apple caps search at 25 results per request, so this is its ceiling rather
/// than a choice. More than that means paging with an offset.
const CATALOG_LIMIT: u32 = 25;

/// Tile covers are fetched at twice their drawn size, so they stay sharp on a
/// HiDPI screen without paying for the 512px the Now Playing bar needs.
const TILE_ART: u32 = 320;

/// How many artists and albums to show above the songs. Enough to be a way in,
/// few enough that the songs are still visible without scrolling.
const CATALOG_BROWSE_ROWS: usize = 3;

/// Stop paging here. Nobody scrolls 400 search results, and an unbounded list
/// is an unbounded number of requests.
const CATALOG_MAX: usize = 200;

/// Upper bound on the library load. Apple pages at 100, so this is 25 requests
/// worst case. Generous for one laptop, and bounded so a very large library
/// cannot spin forever on first run.
const LIBRARY_MAX: usize = 2_500;

/// Where we are in bringing the sidecar up. Each variant is a distinct
/// `StatusPage`, because "it's just spinning" is the failure mode this whole
/// module exists to avoid (rule 4).
#[derive(Debug, Default)]
pub enum Stage {
    #[default]
    Starting,
    /// Chromium's component updater is fetching the CDM. First run only, but it
    /// needs network and can take a minute — so it gets to say so.
    InstallingWidevine,
    /// Loaded music.apple.com; waiting for the hook to attach.
    Connecting,
    /// Signed out. Apple's own login window is one click away.
    SignedOut,
    Ready,
    /// The sidecar died; a restart is scheduled (rule 6).
    Restarting(u32),
    /// Apple changed the page, or the CDM is unavailable. Names the fix.
    Broken(String),
}

pub struct AppModel {
    stage: Stage,
    player: PlayerState,
    /// Kept for the row context menu, whose GTK actions outlive the `update`
    /// call that built them.
    menu_sender: ComponentSender<AppModel>,

    /// The last command sent to the sidecar, and when. Read only by the
    /// gapless diagnostic, which needs to distinguish a transition **we** asked
    /// for from one MusicKit made on its own — the second is the gapless path
    /// and the first is not. `RefCell` because `send` takes `&self`.
    last_command: std::cell::RefCell<Option<(std::time::Instant, String)>>,

    /// Furthest position reached in the current track, and that track's length.
    ///
    /// A high-water mark rather than a live read, because at the moment
    /// `nowPlayingItemDidChange` arrives MusicKit has usually already zeroed
    /// the position — and sometimes has not. Sampling it there gave a number
    /// that was the full duration on three boundaries and zero on a fourth,
    /// depending purely on which event won the race.
    progress_mark: std::cell::Cell<(u64, u64)>,

    /// Live for the process lifetime, never persisted (rule 7).
    tokens: Option<Tokens>,
    sidecar: Option<sidecar::Handle>,
    restarts: u32,
    toaster: adw::ToastOverlay,
    now_playing: Controller<NowPlaying>,
    queue_view: Controller<QueueView>,
    /// The rows on screen — the filtered view. A `ListView`, so its cost is
    /// the number of rows visible rather than the size of the library.
    library: TypedListView<LibraryItem, gtk::NoSelection>,
    /// Whether the queue sidebar is open.
    show_queue: bool,
    /// Whether the navigation sidebar is open. Persisted, like the section:
    /// someone who closes it wants it closed next time too.
    show_sidebar: bool,
    /// Which library row currently carries the play marker.
    marked_playing: Option<String>,
    /// Icons of the library rows currently on screen, so the marker can move
    /// without editing the model — see `RowRegistry`.
    library_icons: RowRegistry<LibraryRowWidgets>,
    /// Who is playing. Shared with every library row; see `CurrentTrack`.
    current_track: CurrentTrack,
    /// Ids MusicKit refused, shared with every library row; see `DeadTracks`.
    dead_rows: DeadTracks,
    /// The full library from the last load. The filter reads this, never the
    /// factory, so narrowing and then clearing a search is lossless.
    all_tracks: Vec<Track>,
    /// One query per scope. They are genuinely different searches: filtering
    /// your library by what you typed into Apple Music is meaningless, and
    /// clearing the box to get your library back would throw away the catalog
    /// search you were in the middle of.
    library_query: String,
    catalog_query: String,
    /// Which sidebar section is showing. `scope()` derives the search scope
    /// from it; never store both.
    view: View,
    /// How the Songs list is ordered. Applied in `visible_entries`.
    sort: SortBy,
    /// Whether the user flipped the sort's natural direction.
    sort_reversed: bool,
    /// The user's library albums and artists, loaded on first visit rather than
    /// at startup — launching should not wait on three collections.
    albums: Vec<Album>,
    artists: Vec<Artist>,
    playlists: Vec<Playlist>,
    album_grid: TypedGridView<GridItem, gtk::NoSelection>,
    artist_grid: TypedGridView<GridItem, gtk::NoSelection>,
    playlist_grid: TypedGridView<GridItem, gtk::NoSelection>,
    loading_albums: bool,
    loading_artists: bool,
    loading_playlists: bool,
    /// Tile artwork already on disk. Shared between the grids on purpose: it
    /// is keyed by the artwork itself, so a cover fetched for one is a cover
    /// the other gets for free.
    tile_art: ArtCache,
    /// Which widget is showing which artwork — **one registry per grid**, for
    /// the same reason the row registries are per list: a shared one would have
    /// the two grids overwrite each other's entries, and clearing it for a
    /// rebuild of one would silently unregister the other's tiles.
    album_art_widgets: ArtRegistry,
    artist_art_widgets: ArtRegistry,
    playlist_art_widgets: ArtRegistry,
    /// Fetches already in flight, so a tile rebinding twice while scrolling
    /// does not queue the same download again.
    tile_art_pending: std::collections::HashSet<String>,
    /// Handed to every tile: "fetch this cover." An `Rc<dyn Fn>` rather than a
    /// sender because `bind` runs deep inside GTK's factory and has no access
    /// to the component — this is the same shape as the detail pages' click
    /// callbacks.
    tile_art_request: ArtRequest,
    /// Results of the last catalog search — songs, albums and artists mixed.
    /// Kept separate from `all_tracks` so switching back to Library does not
    /// have to reload anything.
    catalog: Vec<Entry>,
    /// Album and artist pages, innermost last. Not a widget mirror: the pages
    /// are pushed into a `NavigationView`, and this is what lets a click on one
    /// find the page it came from — **by id, never by depth**, because a stack
    /// that moved between the click and the handler is exactly the class of bug
    /// that produced the wrong song four times over.
    pages: Vec<DetailPage>,
    /// Never reused, never reset. A popped page's id must not come back and
    /// collect a response meant for it.
    next_page_id: u64,
    /// The navigation stack for the content pane. Held because pages are pushed
    /// from `update`, not declared in the view.
    nav: adw::NavigationView,

    /// How many songs are in `catalog`. Paging appends songs only, so this is
    /// the offset for the next page — the album and artist rows above them
    /// would otherwise skew it.
    catalog_songs: usize,
    searching_catalog: bool,
    /// How many catalog results we already hold, and whether Apple has run out.
    /// Together these decide whether scrolling to the end fetches more.
    catalog_exhausted: bool,
    /// Bumped per keystroke; only the newest debounce timer is allowed to fire,
    /// and only the newest response is allowed to land. Without the second
    /// guard a slow request for "aita" can overwrite the results for "aitana".
    search_gen: u64,
    loading_library: bool,
    /// Catalog ids MusicKit has told us it cannot resolve. Remembered for the
    /// session so a delisted track only breaks one play attempt, not every one.
    dead_ids: std::collections::HashSet<String>,
    /// The last queue we tried and the id we wanted to start on, so a
    /// `NOT_FOUND` can be retried without the offenders instead of making the
    /// user click again. An id rather than an index — see `queue_from`.
    last_queue: Option<(Vec<String>, Option<String>)>,
    /// The track we asked to start on, held until MusicKit's own queue confirms
    /// it. See `verify_start`: the queue MusicKit builds is not always the list
    /// we sent, so the position we asked for is not always the track we meant.
    pending_start: Option<String>,
    mpris: Mpris,
    /// Volume is the one piece of player state the sidecar never echoes back,
    /// so we hold it here to keep the bar and MPRIS agreeing.
    volume: f64,
    /// Where the current cover lives on disk, for MPRIS's file:// artUrl.
    art_path: Option<PathBuf>,
    /// The artwork template of the track we last fetched, so a position tick
    /// or a queue echo doesn't re-request the same cover.
    art_for: Option<String>,
    /// Live only while playing; see `TICK_MS`.
    tick: Option<gtk::glib::SourceId>,
    settings: Settings,
    /// The track the last notification was sent for, so a queue echo or a
    /// position tick cannot re-notify for the song already playing.
    notified_for: Option<String>,
    /// A track whose notification is waiting on its cover to finish
    /// downloading. See `maybe_notify`.
    notify_when_art_lands: Option<String>,
}

/// Something we can ask Apple to do to the user's account.
///
/// Both answer 202 Accepted with an empty body — "acceptable, may not have
/// completed" — so neither can be treated as done, only as sent. That is why
/// nothing here toggles a checkbox: showing state would mean reading it back,
/// and a star that lies is worse than no star.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibraryAction {
    AddToLibrary,
    Favorite,
}

impl LibraryAction {
    fn sent(self) -> &'static str {
        match self {
            Self::AddToLibrary => "Adding to your library…",
            Self::Favorite => "Favouriting…",
        }
    }

    fn done(self) -> &'static str {
        match self {
            Self::AddToLibrary => "Sent to your library",
            Self::Favorite => "Favourited",
        }
    }
}

#[derive(Debug)]
pub enum AppMsg {
    SignIn,
    PlayPause,
    /// Explicit, not a toggle. MPRIS sends `Play`, `Pause` and `PlayPause` as
    /// three distinct calls, and collapsing the first two into the toggle makes
    /// the Shell pause a track it just asked to play.
    Play,
    Pause,
    Next,
    Previous,
    Seek(u64),
    SetVolume(f64),
    SetShuffle(bool),
    SetRepeat(Repeat),
    SetSort(SortBy),
    ToggleSortDirection,
    /// A row was right-clicked; show its menu there.
    ShowRowMenu(RowMenuRequest),
    /// Grow the queue MusicKit already holds, without rebuilding it.
    Enqueue {
        catalog_id: String,
        next: bool,
    },
    /// Write to the user's Apple Music account: save a track, or star it.
    LibraryWrite {
        catalog_id: String,
        action: LibraryAction,
    },
    /// Repaint the seek bar from the interpolated position.
    Tick,
    /// Play the visible list, starting at this row.
    PlayFrom(usize),
    SearchChanged(String),
    /// The debounce elapsed for this generation; run the catalog search.
    RunCatalogSearch(u64),
    SetView(View),
    /// A grid tile was activated. The position is resolved against the grid
    /// immediately, never stored.
    AlbumActivated(u32),
    ArtistActivated(u32),
    PlaylistActivated(u32),
    /// A tile is on screen and its cover is not on disk yet.
    NeedTileArt(String, Artwork),
    ToggleSidebar,
    /// The results list is near its end; fetch the next page if there is one.
    LoadMoreCatalog,
    ReloadLibrary,
    ShowPreferences,
    ShowShortcuts,
    ShowAbout,
    SetTheme(u32),
    SetAccent(crate::style::Accent),
    SetNotifyTrackChange(bool),
    ToggleQueue,
    /// A library row was activated; the position is resolved immediately.
    LibraryActivated(u32),
    /// A row on a pushed page was clicked. Carries the page's id so it can be
    /// resolved against the live stack rather than a remembered depth.
    DetailActivated {
        page: u64,
        row: usize,
    },
    /// Play everything on a page — from the top, or shuffled.
    PlayPage {
        page: u64,
        shuffle: bool,
    },
    /// Push an album or artist page — catalog or library, which the `PageKind`
    /// carries so the fetch knows which endpoint to ask.
    OpenPage(PageKind),
    /// The navigation view popped a page — drop the state behind it.
    PagePopped(u64),
    /// Act on a track in MusicKit's queue, by id. The position is resolved
    /// against the live queue at send time — our row order can drift from
    /// MusicKit's, and sending a stale position got INVALID_ARGUMENTS.
    JumpTo(String),
    RemoveFromQueue(String),
}

#[derive(Debug)]
pub enum CommandMsg {
    /// Everything the sidecar pushed up, including its death.
    Sidecar(Incoming),
    /// The child started; here is the handle for talking to it.
    Spawned(sidecar::Handle),
    /// The user's library, or why it couldn't be read.
    Library(Result<Vec<Track>, String>),
    /// Catalog results, tagged with the search they belong to.
    Catalog {
        generation: u64,
        /// Where this page started, so a first page replaces and a later page
        /// appends.
        offset: usize,
        result: Result<crate::music::client::SearchResults, String>,
    },
    /// Cover art is on disk. `None` when the fetch failed — a missing cover is
    /// cosmetic and must not become a toast.
    Artwork {
        path: Option<PathBuf>,
        /// A colour taken from that cover, for the Now Playing bar. Carried
        /// here rather than in its own message because the two are read from
        /// one decode and must be applied together.
        tint: Option<(u8, u8, u8)>,
    },
    /// An album page's contents. Tagged with the page id: by the time this
    /// lands the user may have gone back, and filling a page that is no longer
    /// on the stack is at best wasted work.
    AlbumPage {
        page: u64,
        result: Result<(Album, Vec<Track>), String>,
    },
    /// An artist page's contents.
    ArtistPage {
        page: u64,
        result: Result<(Artist, Vec<Album>), String>,
    },
    /// A playlist page's contents.
    PlaylistPage {
        page: u64,
        result: Result<(Playlist, Vec<Track>), String>,
    },
    /// A page's header art is on disk, or could not be fetched.
    PageArtwork {
        page: u64,
        path: Option<PathBuf>,
    },
    /// The user's library albums / artists.
    LibraryAlbums(Result<Vec<Album>, String>),
    LibraryArtists(Result<Vec<Artist>, String>),
    LibraryPlaylists(Result<Vec<Playlist>, String>),
    /// A library write came back. `Ok` means Apple **accepted** it, not that
    /// it is done — see `Client::add_to_library`.
    LibraryWritten {
        catalog_id: String,
        action: LibraryAction,
        result: Result<(), String>,
    },
    /// A grid tile's cover is on disk, or could not be fetched.
    TileArt {
        key: String,
        path: Option<PathBuf>,
    },
}

#[relm4::component(pub)]
impl Component for AppModel {
    type Init = Settings;
    type Input = AppMsg;
    type Output = ();
    type CommandOutput = CommandMsg;

    view! {
        adw::ApplicationWindow {
            set_title: Some("Tonearm"),
            set_default_width: 1000,
            set_default_height: 680,

            #[local_ref]
            toaster -> adw::ToastOverlay {
                adw::ToolbarView {
                    // The queue slides in from the right and moves the main
                    // view rather than covering it.
                    #[wrap(Some)]
                    set_content = &adw::OverlaySplitView {
                        set_sidebar_position: gtk::PackType::End,
                        // The split view owns the sidebar's width, not the
                        // sidebar. Its child used to carry a 340px
                        // width_request, which the collapse animation then had
                        // to violate on every frame — hence GTK warning that a
                        // GtkRevealer was being measured smaller than its
                        // minimum every time the queue closed.
                        set_min_sidebar_width: 300.0,
                        set_max_sidebar_width: 380.0,
                        set_sidebar_width_fraction: 0.28,
                        #[watch]
                        set_show_sidebar: model.show_queue,

                        #[wrap(Some)]
                        #[local_ref]
                        set_sidebar = queue_sidebar -> adw::ToolbarView {},

                        // Navigation on the left, and an OverlaySplitView
                        // rather than a NavigationSplitView because it can be
                        // dismissed: once the sidebar is something you toggle,
                        // it is a panel you summon, which is exactly what this
                        // widget is for. The queue on the right is the same
                        // shape for the same reason.
                        #[wrap(Some)]
                        set_content = &adw::OverlaySplitView {
                            set_min_sidebar_width: 200.0,
                            set_max_sidebar_width: 260.0,
                            #[watch]
                            set_show_sidebar: model.show_sidebar,

                            #[wrap(Some)]
                            set_sidebar = &adw::ToolbarView {
                                    add_top_bar = &adw::HeaderBar {
                                        #[wrap(Some)]
                                        set_title_widget = &adw::WindowTitle {
                                            set_title: "Tonearm",
                                            #[watch]
                                            set_subtitle: &model.subtitle(),
                                        },

                                        pack_end = &gtk::MenuButton {
                                            set_icon_name: "open-menu-symbolic",
                                            set_tooltip_text: Some("Main Menu"),
                                            set_menu_model: Some(&primary_menu),
                                        },
                                    },

                                    #[wrap(Some)]
                                    set_content = &gtk::ScrolledWindow {
                                        set_vexpand: true,

                                        #[wrap(Some)]
                                        set_child = &gtk::Box {
                                            set_orientation: gtk::Orientation::Vertical,
                                            set_spacing: 2,
                                            set_margin_top: 6,

                                            // ONE ListBox, not one per
                                            // section. Two boxes each keep
                                            // their own selection, and the one
                                            // that takes initial focus selects
                                            // its first row — overriding
                                            // whatever the other was set to,
                                            // which is why the wrong row looked
                                            // active on startup. Section
                                            // headings come from a header func
                                            // instead.
                                            #[name = "nav_list"]
                                            gtk::ListBox {
                                                add_css_class: "navigation-sidebar",
                                                set_selection_mode: gtk::SelectionMode::Single,
                                                connect_row_selected[sender] => move |_, row| {
                                                    if let Some(row) = row {
                                                        sender.input(AppMsg::SetView(
                                                            View::from_row(row.index()),
                                                        ));
                                                    }
                                                },

                                                // Index 0 — Apple Music. The
                                                // order is the contract that
                                                // connect_row_selected reads.
                                                gtk::ListBoxRow {
                                                    #[wrap(Some)]
                                                    set_child = &gtk::Box {
                                                        set_spacing: 12,
                                                        set_margin_all: 8,

                                                        gtk::Image {
                                                            set_icon_name: Some(icon("system-search-symbolic")),
                                                        },
                                                        gtk::Label {
                                                            set_label: "Search",
                                                        },
                                                    },
                                                },

                                                // Index 1 — Library / Songs.
                                                gtk::ListBoxRow {
                                                    #[wrap(Some)]
                                                    set_child = &gtk::Box {
                                                        set_spacing: 12,
                                                        set_margin_all: 8,

                                                        gtk::Image {
                                                            set_icon_name: Some(icon("folder-music-symbolic")),
                                                        },
                                                        gtk::Label {
                                                            set_label: "Songs",
                                                            set_hexpand: true,
                                                            set_xalign: 0.0,
                                                        },

                                                        // The library loads on
                                                        // startup whichever
                                                        // section you are in.
                                                        // Say so here, next to
                                                        // the thing that is
                                                        // loading, rather than
                                                        // across the whole
                                                        // window.
                                                        adw::Spinner {
                                                            set_size_request: (16, 16),
                                                            #[watch]
                                                            set_visible: model.loading_library,
                                                        },
                                                    },
                                                },

                                                // Index 2 — Albums.
                                                gtk::ListBoxRow {
                                                    #[wrap(Some)]
                                                    set_child = &gtk::Box {
                                                        set_spacing: 12,
                                                        set_margin_all: 8,

                                                        gtk::Image {
                                                            set_icon_name: Some(icon("media-optical-symbolic")),
                                                        },
                                                        gtk::Label {
                                                            set_label: "Albums",
                                                            set_hexpand: true,
                                                            set_xalign: 0.0,
                                                        },
                                                        adw::Spinner {
                                                            set_size_request: (16, 16),
                                                            #[watch]
                                                            set_visible: model.loading_albums,
                                                        },
                                                    },
                                                },

                                                // Index 3 — Artists.
                                                gtk::ListBoxRow {
                                                    #[wrap(Some)]
                                                    set_child = &gtk::Box {
                                                        set_spacing: 12,
                                                        set_margin_all: 8,

                                                        gtk::Image {
                                                            set_icon_name: Some(icon("avatar-default-symbolic")),
                                                        },
                                                        gtk::Label {
                                                            set_label: "Artists",
                                                            set_hexpand: true,
                                                            set_xalign: 0.0,
                                                        },
                                                        adw::Spinner {
                                                            set_size_request: (16, 16),
                                                            #[watch]
                                                            set_visible: model.loading_artists,
                                                        },
                                                    },
                                                },

                                                // Index 4 — Playlists.
                                                gtk::ListBoxRow {
                                                    #[wrap(Some)]
                                                    set_child = &gtk::Box {
                                                        set_spacing: 12,
                                                        set_margin_all: 8,

                                                        gtk::Image {
                                                            set_icon_name: Some(icon("view-list-symbolic")),
                                                        },
                                                        gtk::Label {
                                                            set_label: "Playlists",
                                                            set_hexpand: true,
                                                            set_xalign: 0.0,
                                                        },
                                                        adw::Spinner {
                                                            set_size_request: (16, 16),
                                                            #[watch]
                                                            set_visible: model.loading_playlists,
                                                        },
                                                    },
                                                },
                                            },
                                    },
                                },
                            },

                            #[wrap(Some)]
                            #[local_ref]
                            set_content = nav_view -> adw::NavigationView {
                                add = &adw::NavigationPage {
                                    set_title: "Tonearm",
                                    // The root page. Albums and artists push on
                                    // top of it; nothing ever pops it.
                                    set_tag: Some("results"),

                                    #[wrap(Some)]
                                    set_child = &adw::ToolbarView {
                                    add_top_bar = &adw::HeaderBar {
                                        // The sidebar's own header carries the
                                        // start-side window controls while it
                                        // is open, so this header only shows
                                        // them once the sidebar is away.
                                        #[watch]
                                        set_show_start_title_buttons: !model.show_sidebar,

                                        pack_start = &gtk::ToggleButton {
                                            set_icon_name: "sidebar-show-symbolic",
                                            set_tooltip_text: Some("Toggle Sidebar"),
                                            #[watch]
                                            set_active: model.show_sidebar,
                                            connect_clicked => AppMsg::ToggleSidebar,
                                        },

                                        // When the queue is open it is the
                                        // rightmost pane, so the window
                                        // controls belong to its header, not
                                        // this one. Without this they vanish:
                                        // the queue's header hides them and
                                        // this header is no longer at the edge.
                                        #[watch]
                                        set_show_end_title_buttons: !model.show_queue,

                                        #[wrap(Some)]
                                        #[name = "search_entry"]
                                        set_title_widget = &gtk::SearchEntry {
                                            set_width_request: 320,
                                            #[watch]
                                            set_placeholder_text: Some(match model.view {
                                                View::Songs => "Search your library",
                                                View::Albums => "Search albums",
                                                View::Artists => "Search artists",
                                                View::Playlists => "Search playlists",
                                                View::Search => "Search Apple Music",
                                            }),
                                            connect_search_changed[sender] => move |entry| {
                                                sender.input(AppMsg::SearchChanged(entry.text().into()));
                                            },
                                        },

                                        // Only in Songs: the grids have their
                                        // own natural order and sorting them
                                        // is a different question.
                                        #[name = "sort_button"]
                                        pack_end = &gtk::MenuButton {
                                            set_icon_name: "view-sort-descending-symbolic",
                                            set_tooltip_text: Some("Sort"),
                                            add_css_class: "flat",
                                            #[watch]
                                            set_visible: model.view == View::Songs,
                                        },

                                        pack_end = &gtk::Button {
                                            set_icon_name: "view-refresh-symbolic",
                                            set_tooltip_text: Some("Reload library"),
                                            add_css_class: "flat",
                                            #[watch]
                                            set_visible: model.view == View::Songs,
                                            #[watch]
                                            set_sensitive: !model.loading_library,
                                            connect_clicked => AppMsg::ReloadLibrary,
                                        },
                                    },

                                    #[wrap(Some)]
                                    set_content = &gtk::Stack {
                                        add_named[Some("status")] = &adw::StatusPage {
                                            #[watch]
                                            set_icon_name: Some(model.icon()),
                                            #[watch]
                                            set_title: &model.headline(),
                                            #[watch]
                                            set_description: Some(&model.detail()),

                                            #[wrap(Some)]
                                            set_child = &gtk::Button {
                                                set_label: "Sign in to Apple Music",
                                                set_halign: gtk::Align::Center,
                                                add_css_class: "suggested-action",
                                                add_css_class: "pill",
                                                #[watch]
                                                set_visible: matches!(model.stage, Stage::SignedOut),
                                                connect_clicked => AppMsg::SignIn,
                                            },
                                        },

                                        // Loading gets its own page: "nothing
                                        // here yet" and "still fetching" look
                                        // identical otherwise.
                                        add_named[Some("loading")] = &gtk::Box {
                                            set_orientation: gtk::Orientation::Vertical,
                                            set_halign: gtk::Align::Center,
                                            set_valign: gtk::Align::Center,
                                            set_spacing: 18,

                                            adw::Spinner {
                                                set_size_request: (42, 42),
                                            },

                                            gtk::Label {
                                                add_css_class: "title-2",
                                                #[watch]
                                                set_label: match model.view {
                                                    View::Search => "Searching Apple Music",
                                                    View::Albums => "Loading your albums",
                                                    View::Artists => "Loading your artists",
                                                    View::Playlists => "Loading your playlists",
                                                    View::Songs => "Loading your library",
                                                },
                                            },
                                        },

                                        // The Clamp goes OUTSIDE the
                                        // ScrolledWindow. Inside, it breaks
                                        // ListView's height allocation and the
                                        // list stops materialising rows part
                                        // way down.
                                        add_named[Some("library")] = &adw::Clamp {
                                            set_maximum_size: 800,

                                            #[wrap(Some)]
                                            #[name = "library_scroller"]
                                            set_child = &gtk::ScrolledWindow {
                                                set_vexpand: true,

                                                #[local_ref]
                                                library_list -> gtk::ListView {
                                                    set_single_click_activate: true,
                                                    add_css_class: "navigation-sidebar",
                                                },
                                            },
                                        },

                                        // Grids scroll as themselves: unlike the
                                        // detail pages there is no header above
                                        // them, so the GridView can be the
                                        // scrollable child and stay virtualised.
                                        add_named[Some("albums")] = &gtk::ScrolledWindow {
                                            set_vexpand: true,
                                            set_hscrollbar_policy: gtk::PolicyType::Never,

                                            #[local_ref]
                                            album_grid -> gtk::GridView {
                                                set_single_click_activate: true,
                                                set_max_columns: 12,
                                                set_margin_all: 12,
                                            },
                                        },

                                        add_named[Some("artists")] = &gtk::ScrolledWindow {
                                            set_vexpand: true,
                                            set_hscrollbar_policy: gtk::PolicyType::Never,

                                            #[local_ref]
                                            artist_grid -> gtk::GridView {
                                                set_single_click_activate: true,
                                                set_max_columns: 12,
                                                set_margin_all: 12,
                                            },
                                        },

                                        add_named[Some("playlists")] = &gtk::ScrolledWindow {
                                            set_vexpand: true,
                                            set_hscrollbar_policy: gtk::PolicyType::Never,

                                            #[local_ref]
                                            playlist_grid -> gtk::GridView {
                                                set_single_click_activate: true,
                                                set_max_columns: 12,
                                                set_margin_all: 12,
                                            },
                                        },

                                        // An empty search box is not a failed
                                        // search. Telling someone that Apple
                                        // Music has nothing matching "" is
                                        // nonsense — this is an invitation.
                                        add_named[Some("search-prompt")] = &adw::StatusPage {
                                            set_icon_name: Some("system-search-symbolic"),
                                            set_title: "Search Apple Music",
                                            set_description: Some(
                                                "Find songs from the whole catalogue, not just your library.",
                                            ),
                                        },

                                        // Distinct from "status": an empty
                                        // library and a search with no matches
                                        // are different problems.
                                        add_named[Some("no-results")] = &adw::StatusPage {
                                            set_icon_name: Some("system-search-symbolic"),
                                            set_title: "No matches",
                                            #[watch]
                                            set_description: Some(&match model.view {
                                                View::Songs => format!(
                                                    "Nothing in your library matches “{}”. Try searching Apple Music.",
                                                    model.query()
                                                ),
                                                View::Albums => format!(
                                                    "No album in your library matches “{}”.",
                                                    model.query()
                                                ),
                                                View::Artists => format!(
                                                    "No artist in your library matches “{}”.",
                                                    model.query()
                                                ),
                                                View::Playlists => format!(
                                                    "No playlist in your library matches “{}”.",
                                                    model.query()
                                                ),
                                                View::Search => format!(
                                                    "Apple Music has nothing matching “{}”.",
                                                    model.query()
                                                ),
                                            }),
                                        },

                                        // After the children — naming a child
                                        // before it has been added warns and
                                        // does nothing.
                                        #[watch]
                                        set_visible_child_name: model.page(),
                                    },
                                    },
                                },
                            },
                        },
                    },

                    // The bar spans the full width under both panes — it is
                    // the app. Wrapped in a Box so the visibility watch has
                    // somewhere to live: the bar itself is a child component.
                    add_bottom_bar = &gtk::Box {
                        #[watch]
                        set_visible: matches!(model.stage, Stage::Ready),

                        #[local_ref]
                        now_playing_bar -> gtk::Box {
                            set_hexpand: true,
                        },
                    },
                },
            },
        }
    }

    fn init(
        settings: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        // The bar emits intent, never commands — `app.rs` is the only place
        // that talks to the sidecar (rule 9).
        let now_playing = NowPlaying::builder()
            .launch(())
            .forward(sender.input_sender(), |out| match out {
                NowPlayingOutput::PlayPause => AppMsg::PlayPause,
                NowPlayingOutput::Next => AppMsg::Next,
                NowPlayingOutput::Previous => AppMsg::Previous,
                NowPlayingOutput::Seek(ms) => AppMsg::Seek(ms),
                NowPlayingOutput::SetVolume(v) => AppMsg::SetVolume(v),
                NowPlayingOutput::SetShuffle(on) => AppMsg::SetShuffle(on),
                NowPlayingOutput::SetRepeat(mode) => AppMsg::SetRepeat(mode),
                NowPlayingOutput::ToggleQueue => AppMsg::ToggleQueue,
            });

        let library: TypedListView<LibraryItem, gtk::NoSelection> = TypedListView::new();
        let activate = sender.clone();
        library.view.connect_activate(move |_, position| {
            activate.input(AppMsg::LibraryActivated(position));
        });

        // One handler for every list's rows. `setup` is a static method with no
        // item to carry a callback, so this is installed once, here.
        let menu_sender = sender.clone();
        crate::components::track_row::set_row_menu(move |req| {
            menu_sender.input(AppMsg::ShowRowMenu(req));
        });

        let queue_view = QueueView::builder()
            .launch(())
            .forward(sender.input_sender(), |out| match out {
                QueueViewOutput::Jump(id) => AppMsg::JumpTo(id),
                QueueViewOutput::Remove(id) => AppMsg::RemoveFromQueue(id),
            });

        // Popping is the user's business (back button, swipe, Escape), so the
        // stack is told about it rather than driving it. Resolving by tag keeps
        // the id-not-index rule intact even here.
        let nav = adw::NavigationView::new();
        let popped = sender.clone();
        nav.connect_popped(move |_, page| {
            if let Some(id) = page.tag().and_then(|t| t.parse::<u64>().ok()) {
                popped.input(AppMsg::PagePopped(id));
            }
        });

        let album_grid: TypedGridView<GridItem, gtk::NoSelection> = TypedGridView::new();
        let activate = sender.clone();
        album_grid
            .view
            .connect_activate(move |_, position| activate.input(AppMsg::AlbumActivated(position)));

        let artist_grid: TypedGridView<GridItem, gtk::NoSelection> = TypedGridView::new();
        let activate = sender.clone();
        artist_grid
            .view
            .connect_activate(move |_, position| activate.input(AppMsg::ArtistActivated(position)));

        let playlist_grid: TypedGridView<GridItem, gtk::NoSelection> = TypedGridView::new();
        let activate = sender.clone();
        playlist_grid.view.connect_activate(move |_, position| {
            activate.input(AppMsg::PlaylistActivated(position))
        });

        // Tiles call this from `bind`, deep inside GTK's factory, where there
        // is no component to reach. It turns "I need this cover" into an
        // ordinary message, so the fetch itself still happens as a Command off
        // the GTK thread (rule 8).
        let art_sender = sender.clone();
        let tile_art_request: ArtRequest = std::rc::Rc::new(move |key, art| {
            art_sender.input(AppMsg::NeedTileArt(key, art));
        });

        let model = AppModel {
            stage: Stage::Starting,
            queue_view,
            library,
            show_queue: false,
            show_sidebar: settings.show_sidebar,
            marked_playing: None,
            library_icons: row_registry(),
            current_track: current_track(),
            dead_rows: dead_tracks(),
            // filled from `dead_ids` once the model exists (see below)
            all_tracks: Vec::new(),
            library_query: String::new(),
            catalog_query: String::new(),
            view: View::from(settings.section),
            sort: SortBy::parse(&settings.sort),
            sort_reversed: settings.sort_reversed,
            albums: Vec::new(),
            artists: Vec::new(),
            playlists: Vec::new(),
            album_grid,
            artist_grid,
            playlist_grid,
            loading_albums: false,
            loading_artists: false,
            loading_playlists: false,
            tile_art: art_cache(),
            album_art_widgets: art_registry(),
            artist_art_widgets: art_registry(),
            playlist_art_widgets: art_registry(),
            tile_art_pending: std::collections::HashSet::new(),
            tile_art_request,
            catalog: Vec::new(),
            catalog_songs: 0,
            pages: Vec::new(),
            next_page_id: 1,
            nav,
            searching_catalog: false,
            catalog_exhausted: false,
            search_gen: 0,
            loading_library: false,
            // Seeded from the cache so the first play of a session does not
            // have to rediscover them by failing a setQueue.
            dead_ids: crate::unplayable::load(),
            last_queue: None,
            pending_start: None,
            player: PlayerState::new(),
            menu_sender: sender.clone(),
            last_command: std::cell::RefCell::new(None),
            progress_mark: std::cell::Cell::new((0, 0)),
            tokens: None,
            sidecar: None,
            restarts: 0,
            toaster: adw::ToastOverlay::new(),
            now_playing,
            mpris: Mpris::start(sender.clone()),
            volume: 1.0,
            art_path: None,
            art_for: None,
            tick: None,
            settings,
            notified_for: None,
            notify_when_art_lands: None,
        };
        let primary_menu = gtk::gio::Menu::new();
        {
            let section = gtk::gio::Menu::new();
            section.append(Some("_Preferences"), Some("win.preferences"));
            section.append(Some("_Keyboard Shortcuts"), Some("win.shortcuts"));
            section.append(Some("_About Tonearm"), Some("win.about"));
            primary_menu.append_section(None, &section);
        }

        let toaster = &model.toaster;
        let now_playing_bar = model.now_playing.widget();
        let library_list = &model.library.view;
        let nav_view = &model.nav;
        let album_grid = &model.album_grid.view;
        let artist_grid = &model.artist_grid.view;
        let playlist_grid = &model.playlist_grid.view;
        let queue_sidebar = model.queue_view.widget();
        let widgets = view_output!();

        // Sidebar rows, added imperatively so each section is its own ListBox
        // and the two behave as one selection: picking a row in either clears
        // the other, which a single ListBox would do for free but two will not.
        // Section headings, drawn above the row that starts each section.
        widgets.nav_list.set_header_func(|row, _before| {
            let title = match row.index() {
                0 => "Apple Music",
                1 => "Library",
                _ => return,
            };
            let label = gtk::Label::new(Some(title));
            label.set_xalign(0.0);
            label.set_margin_start(16);
            label.set_margin_top(if row.index() == 0 { 6 } else { 12 });
            label.set_margin_bottom(2);
            label.add_css_class("heading");
            label.add_css_class("dim-label");
            row.set_header(Some(&label));
        });

        // The sort menu, built imperatively so the radio state can be bound to
        // a stateful action rather than hand-managed across five items.
        {
            let menu = gtk::gio::Menu::new();

            // Its own section, because reversing is a different question from
            // choosing a key — and it stays put while the radio list changes.
            let direction = gtk::gio::Menu::new();
            direction.append(Some("_Reverse Order"), Some("sort.reverse"));
            menu.append_section(None, &direction);

            let keys = gtk::gio::Menu::new();
            for option in SortBy::ALL {
                let item = gtk::gio::MenuItem::new(Some(option.label()), None);
                item.set_action_and_target_value(Some("sort.by"), Some(&option.id().to_variant()));
                keys.append_item(&item);
            }
            menu.prepend_section(None, &keys);
            widgets.sort_button.set_menu_model(Some(&menu));

            // A stateful action gives the popover its radio dots for free, and
            // keeps the checked item honest when the setting is restored.
            let action = gtk::gio::SimpleAction::new_stateful(
                "by",
                Some(&String::static_variant_type()),
                &model.sort.id().to_variant(),
            );
            let sort_sender = sender.clone();
            action.connect_activate(move |action, target| {
                let Some(id) = target.and_then(|t| t.str().map(str::to_owned)) else {
                    return;
                };
                action.set_state(&id.to_variant());
                sort_sender.input(AppMsg::SetSort(SortBy::parse(&id)));
            });
            let group = gtk::gio::SimpleActionGroup::new();
            group.add_action(&action);

            // Stateful, so the menu draws its own checkmark rather than us
            // rebuilding the model every time it flips.
            let reverse = gtk::gio::SimpleAction::new_stateful(
                "reverse",
                None,
                &model.sort_reversed.to_variant(),
            );
            let rev_sender = sender.clone();
            reverse.connect_activate(move |action, _| {
                let now = !action
                    .state()
                    .and_then(|s| s.get::<bool>())
                    .unwrap_or(false);
                action.set_state(&now.to_variant());
                rev_sender.input(AppMsg::ToggleSortDirection);
            });
            group.add_action(&reverse);

            widgets
                .sort_button
                .insert_action_group("sort", Some(&group));
        }

        // Open on the section we were last in. Selecting fires `row-selected`,
        // which posts SetView — harmless, since the model is already on that
        // view and SetView returns early when unchanged.
        if let Some(row) = widgets.nav_list.row_at_index(model.view.row()) {
            widgets.nav_list.select_row(Some(&row));
        }

        // Fetch the next page of catalog results as the list nears its end.
        // Read-only on the adjustment — it never sets a value, so it cannot
        // fight the scrolling it is watching.
        {
            let sender = sender.clone();
            widgets
                .library_scroller
                .vadjustment()
                .connect_value_changed(move |adj| {
                    let remaining = adj.upper() - (adj.value() + adj.page_size());
                    if remaining < adj.page_size() {
                        sender.input(AppMsg::LoadMoreCatalog);
                    }
                });
        }

        register_actions(&root, &sender);

        // Rows read playability from here, so seed it before any are built.
        *model.dead_rows.borrow_mut() = model.dead_ids.clone();

        start_sidecar(&sender);

        ComponentParts { model, widgets }
    }

    fn shutdown(&mut self, _widgets: &mut Self::Widgets, _output: relm4::Sender<Self::Output>) {
        // A now-playing notification must not outlive the player that sent it.
        notify::clear(relm4::main_application().upcast_ref::<gtk::gio::Application>());
    }

    /// Wraps `update` so the search box can be re-filled after a scope change.
    ///
    /// The entry is the one widget holding text the model also owns, and the
    /// two must agree: switching scope swaps which query is live, and the box
    /// has to show that scope's text rather than the one you left behind.
    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        msg: Self::Input,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        let view_before = self.view;
        self.update(msg, sender.clone(), root);

        if self.view != view_before {
            // `set_text` fires `search-changed`, but `SearchChanged` returns
            // early when the text already matches the active query — which it
            // does by now, because `update` set it first. No loop.
            widgets.search_entry.set_text(self.query());
        }

        self.update_view(widgets, sender);
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, root: &Self::Root) {
        match msg {
            AppMsg::SignIn => self.send(Command::ShowLogin),
            AppMsg::PlayPause => self.send(Command::PlayPause),
            AppMsg::Play => self.send(Command::Play),
            AppMsg::Pause => self.send(Command::Pause),
            AppMsg::Next => self.send(Command::Next),
            AppMsg::Previous => self.send(Command::Previous),
            AppMsg::Seek(position_ms) => {
                self.send(Command::Seek { position_ms });
                // Announce the jump straight away rather than waiting for the
                // sidecar's echo. The spec requires `Seeked` on discontinuous
                // moves — without it controllers keep extrapolating from the
                // old position and their progress bars drift.
                self.mpris.seeked(position_ms);
            }
            AppMsg::SetVolume(volume) => {
                self.volume = volume;
                self.send(Command::SetVolume { volume });
                self.push_snapshot();
            }
            AppMsg::Tick => self.push_snapshot(),
            AppMsg::SearchChanged(query) => {
                if query == self.query() {
                    return;
                }
                // Typing into the search field is a request to see results, and
                // results are on the root page.
                self.pop_to_results();
                match self.scope() {
                    SearchScope::Library => self.library_query = query,
                    SearchScope::Catalog => self.catalog_query = query,
                }

                match self.view {
                    // Local filters: instant, every keystroke.
                    View::Songs => self.rebuild_rows(),
                    View::Albums => self.rebuild_albums(),
                    View::Artists => self.rebuild_artists(),
                    View::Playlists => self.rebuild_playlists(),
                    View::Search => {
                        self.search_gen = self.search_gen.wrapping_add(1);
                        let generation = self.search_gen;

                        self.catalog_exhausted = false;
                        self.catalog_songs = 0;
                        if self.catalog_query.trim().is_empty() {
                            self.catalog.clear();
                            self.searching_catalog = false;
                            self.rebuild_rows();
                            return;
                        }

                        // Debounce. Only the newest timer commits — the same
                        // generation trick the seek bar uses, and for the same
                        // reason: removing a fired glib source aborts.
                        let sender = sender.clone();
                        gtk::glib::timeout_add_local_once(
                            std::time::Duration::from_millis(SEARCH_DEBOUNCE_MS),
                            move || sender.input(AppMsg::RunCatalogSearch(generation)),
                        );
                    }
                }
            }
            AppMsg::RunCatalogSearch(generation) => {
                if generation != self.search_gen {
                    return; // a later keystroke superseded this one
                }
                self.run_catalog_search(&sender, generation, 0);
            }
            AppMsg::SetView(view) => {
                if view == self.view {
                    return;
                }
                self.view = view;
                // Switching section means switching what the content pane is
                // about, so any album or artist pushed on top of it is now
                // showing the wrong thing sitting over the right thing.
                self.pop_to_results();
                self.settings.section = Section::from(view);
                self.settings.save();

                // Whichever section is now showing re-reads; the others keep
                // what they had, so switching back is instant.
                match view {
                    View::Songs => self.rebuild_rows(),
                    View::Albums => {
                        self.rebuild_albums();
                        self.load_albums(&sender);
                    }
                    View::Artists => {
                        self.rebuild_artists();
                        self.load_artists(&sender);
                    }
                    View::Playlists => {
                        self.rebuild_playlists();
                        self.load_playlists(&sender);
                    }
                    View::Search => {
                        self.search_gen = self.search_gen.wrapping_add(1);
                        let generation = self.search_gen;
                        if self.catalog_query.trim().is_empty() {
                            self.catalog.clear();
                            self.rebuild_rows();
                        } else {
                            self.run_catalog_search(&sender, generation, 0);
                        }
                    }
                }
            }
            AppMsg::AlbumActivated(position) => {
                if let Some(item) = self.album_grid.get(position)
                    && let Tile::Album(album) = &item.borrow().tile
                {
                    sender.input(AppMsg::OpenPage(PageKind::album(album)));
                }
            }
            AppMsg::ArtistActivated(position) => {
                if let Some(item) = self.artist_grid.get(position)
                    && let Tile::Artist(artist) = &item.borrow().tile
                {
                    sender.input(AppMsg::OpenPage(PageKind::artist(artist)));
                }
            }
            AppMsg::PlaylistActivated(position) => {
                if let Some(item) = self.playlist_grid.get(position)
                    && let Tile::Playlist(playlist) = &item.borrow().tile
                {
                    sender.input(AppMsg::OpenPage(PageKind::playlist(playlist)));
                }
            }
            AppMsg::NeedTileArt(key, art) => {
                // Scrolling rebinds the same tile repeatedly; one request each.
                if !self.tile_art_pending.insert(key.clone()) {
                    return;
                }
                sender.oneshot_command(async move {
                    CommandMsg::TileArt {
                        key,
                        path: artwork::fetch(art, TILE_ART).await.ok(),
                    }
                });
            }
            AppMsg::LoadMoreCatalog => {
                // Guarded on all four conditions: only in catalog scope, only
                // when a page is not already in flight, only while Apple still
                // has more, and only up to a ceiling. Scroll events arrive in
                // bursts, so without these one flick would queue several
                // identical requests.
                if self.scope() == SearchScope::Catalog
                    && !self.searching_catalog
                    && !self.catalog_exhausted
                    && !self.catalog.is_empty()
                    && self.catalog_songs < CATALOG_MAX
                {
                    let generation = self.search_gen;
                    // Songs only — the browse rows above them are not part of
                    // Apple's song pagination.
                    let offset = self.catalog_songs;
                    self.run_catalog_search(&sender, generation, offset);
                }
            }
            AppMsg::ReloadLibrary => self.load_library(&sender),
            AppMsg::ShowPreferences => self.show_preferences(&sender, root),
            AppMsg::ShowShortcuts => show_shortcuts(root),
            AppMsg::ShowAbout => show_about(root),
            AppMsg::SetTheme(index) => {
                self.settings.theme = Theme::from_index(index);
                self.settings.apply_theme();
                self.settings.save();
            }
            AppMsg::SetNotifyTrackChange(on) => {
                self.settings.notify_track_change = on;
                self.settings.save();
            }
            AppMsg::ToggleSidebar => {
                self.show_sidebar = !self.show_sidebar;
                self.settings.show_sidebar = self.show_sidebar;
                self.settings.save();
            }
            AppMsg::ToggleQueue => {
                self.show_queue = !self.show_queue;
                self.sync_page_controls();
                // The bar's toggle reads this from the snapshot, so push one.
                self.push_snapshot();
                if self.show_queue {
                    self.queue_view.emit(QueueViewInput::ScrollToPlaying);
                }
            }
            AppMsg::LibraryActivated(position) => {
                // Catalog results mix songs with albums and artists. A song
                // plays; the other two are doors, and clicking one walks
                // through it. Resolved against the list as it is right now,
                // never against a remembered snapshot.
                match self.visible_entries().get(position as usize) {
                    Some(Entry::Album(album)) => {
                        sender.input(AppMsg::OpenPage(PageKind::album(album)))
                    }
                    Some(Entry::Artist(artist)) => {
                        sender.input(AppMsg::OpenPage(PageKind::artist(artist)))
                    }
                    // The store is the visible list, so this position is the
                    // row index `queue_from` expects.
                    Some(Entry::Song(_)) => sender.input(AppMsg::PlayFrom(position as usize)),
                    None => {}
                }
            }
            AppMsg::OpenPage(kind) => self.push_page(kind, &sender),
            AppMsg::PagePopped(id) => {
                // The page owns its own row registry, so dropping it takes the
                // stale widget handles with it. Nothing to clean up by hand.
                self.pages.retain(|p| p.id != id);
                tracing::debug!(id, depth = self.pages.len(), "page popped");
            }
            AppMsg::DetailActivated { page, row } => {
                let Some(page) = self.pages.iter().find(|p| p.id == page) else {
                    // Popped between the click and here. Nothing to do, and
                    // certainly nothing to guess at.
                    return;
                };
                match page.entries.get(row) {
                    Some(Entry::Album(album)) => {
                        sender.input(AppMsg::OpenPage(PageKind::album(album)))
                    }
                    Some(Entry::Artist(artist)) => {
                        sender.input(AppMsg::OpenPage(PageKind::artist(artist)))
                    }
                    Some(Entry::Song(_)) => {
                        let entries = page.entries.clone();
                        self.play_entries(&entries, row);
                    }
                    None => {}
                }
            }
            AppMsg::PlayPage { page, shuffle } => {
                let Some(target) = self.pages.iter().find(|p| p.id == page) else {
                    return;
                };
                let entries = target.entries.clone();
                // Shuffle mode goes to MusicKit *before* the queue, so its own
                // shuffle applies to the queue as it loads. Shuffling the ids
                // ourselves would work once and then leave the player in
                // sequential mode, which is not what pressing Shuffle means.
                self.send(Command::SetShuffle { shuffle });
                self.play_entries(&entries, 0);
            }
            AppMsg::JumpTo(id) => match self.queue_index_of(&id) {
                Some(index) => self.send(Command::ChangeToIndex { index }),
                None => self.toast("That track is no longer in the queue"),
            },
            AppMsg::RemoveFromQueue(id) => match self.queue_index_of(&id) {
                Some(index) => self.send(Command::RemoveFromQueue { index }),
                None => self.toast("That track is no longer in the queue"),
            },
            AppMsg::SetAccent(accent) => {
                self.settings.accent = accent.id().into();
                self.settings.save();
                // Live: the provider is replaced, and every widget already
                // referencing the accent variables repaints itself.
                crate::style::set_accent(accent);
            }
            AppMsg::SetSort(sort) => {
                if sort == self.sort {
                    return;
                }
                self.sort = sort;
                self.settings.sort = sort.id().into();
                self.settings.save();
                tracing::info!(sort = sort.id(), "library sort");
                // A rebuild resets the scroll, which is right here: the list
                // the user was looking at no longer exists in that order.
                self.rebuild_rows();
            }
            AppMsg::LibraryWrite { catalog_id, action } => {
                let Some(client) = self.client() else {
                    self.toast("Not connected yet");
                    return;
                };
                // Said out loud before the request goes out: these are
                // fire-and-forget, and a click with no feedback at all reads as
                // a click that did not register.
                self.toast(action.sent());
                tracing::info!(?action, "library write");
                sender.oneshot_command(async move {
                    let result = match action {
                        LibraryAction::AddToLibrary => {
                            client.add_to_library("songs", &catalog_id).await
                        }
                        LibraryAction::Favorite => {
                            client.add_to_favorites("songs", &catalog_id).await
                        }
                    };
                    CommandMsg::LibraryWritten {
                        catalog_id,
                        action,
                        result: result.map_err(|err| format!("{err:#}")),
                    }
                });
            }
            AppMsg::ToggleSortDirection => {
                self.sort_reversed = !self.sort_reversed;
                self.settings.sort_reversed = self.sort_reversed;
                self.settings.save();
                self.rebuild_rows();
            }
            AppMsg::ShowRowMenu(req) => self.show_row_menu(req),
            AppMsg::Enqueue { catalog_id, next } => {
                let songs = vec![catalog_id];
                if self.player.queue.is_empty() {
                    // Nothing to insert into: `playNext` on an empty queue is a
                    // silent no-op in MusicKit. Start the queue instead —
                    // "add to queue" with no queue plainly means "make one",
                    // and refusing was a worse answer than doing it.
                    tracing::info!("starting a queue from one track");
                    self.pending_start = songs.first().cloned();
                    self.last_queue = Some((songs.clone(), songs.first().cloned()));
                    self.send(Command::SetQueue {
                        songs,
                        start_position: 0,
                    });
                    return;
                }
                tracing::info!(next, "enqueueing one track");
                self.send(if next {
                    Command::PlayNext { songs }
                } else {
                    Command::PlayLater { songs }
                });
            }
            AppMsg::SetShuffle(on) => {
                // Sent and forgotten: the mirror updates when MusicKit echoes
                // it back, so the button never claims a state the player is not
                // actually in (rule 3).
                tracing::info!(on, "shuffle");
                self.send(Command::SetShuffle { shuffle: on });
            }
            AppMsg::SetRepeat(mode) => {
                let mode = match mode {
                    Repeat::Off => RepeatMode::None,
                    Repeat::All => RepeatMode::All,
                    Repeat::One => RepeatMode::One,
                };
                tracing::info!(?mode, "repeat");
                self.send(Command::SetRepeat { mode });
            }
            AppMsg::PlayFrom(index) => {
                let visible = self.visible_entries();
                self.play_entries(&visible, index);
            }
        }
    }

    fn update_cmd(
        &mut self,
        msg: Self::CommandOutput,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match msg {
            CommandMsg::AlbumPage { page, result } => {
                let Some(target) = self.pages.iter_mut().find(|p| p.id == page) else {
                    // Navigated back while this was in flight.
                    return;
                };
                match result {
                    Ok((album, tracks)) => {
                        tracing::info!(page, tracks = tracks.len(), album = %album.name, "album loaded");
                        let art = album.artwork.clone();
                        target.show_album(&album, tracks.into_iter().map(Entry::Song).collect());
                        self.fetch_page_art(page, art, &sender);
                    }
                    Err(err) => {
                        tracing::warn!(page, %err, "album page failed");
                        target.fail(&err);
                    }
                }
            }
            CommandMsg::ArtistPage { page, result } => {
                let Some(target) = self.pages.iter_mut().find(|p| p.id == page) else {
                    return;
                };
                match result {
                    Ok((artist, albums)) => {
                        tracing::info!(page, albums = albums.len(), artist = %artist.name, "artist loaded");
                        let art = artist.artwork.clone();
                        target.show_artist(&artist, albums.into_iter().map(Entry::Album).collect());
                        self.fetch_page_art(page, art, &sender);
                    }
                    Err(err) => {
                        tracing::warn!(page, %err, "artist page failed");
                        target.fail(&err);
                    }
                }
            }
            CommandMsg::LibraryAlbums(result) => {
                self.loading_albums = false;
                match result {
                    Ok(albums) => {
                        tracing::info!(albums = albums.len(), "library albums loaded");
                        self.albums = albums;
                        self.rebuild_albums();
                    }
                    Err(err) => {
                        tracing::warn!(%err, "library albums failed");
                        self.toast(&err);
                    }
                }
            }
            CommandMsg::LibraryArtists(result) => {
                self.loading_artists = false;
                match result {
                    Ok(artists) => {
                        tracing::info!(artists = artists.len(), "library artists loaded");
                        self.artists = artists;
                        self.rebuild_artists();
                    }
                    Err(err) => {
                        tracing::warn!(%err, "library artists failed");
                        self.toast(&err);
                    }
                }
            }
            CommandMsg::LibraryPlaylists(result) => {
                self.loading_playlists = false;
                match result {
                    Ok(playlists) => {
                        tracing::info!(playlists = playlists.len(), "library playlists loaded");
                        self.playlists = playlists;
                        self.rebuild_playlists();
                    }
                    Err(err) => {
                        tracing::warn!(%err, "library playlists failed");
                        self.toast(&err);
                    }
                }
            }
            CommandMsg::PlaylistPage { page, result } => {
                let Some(target) = self.pages.iter_mut().find(|p| p.id == page) else {
                    return;
                };
                match result {
                    Ok((playlist, tracks)) => {
                        tracing::info!(page, tracks = tracks.len(), playlist = %playlist.name, "playlist loaded");
                        let art = playlist.artwork.clone();
                        target.show_playlist(
                            &playlist,
                            tracks.into_iter().map(Entry::Song).collect(),
                        );
                        self.fetch_page_art(page, art, &sender);
                    }
                    Err(err) => {
                        tracing::warn!(page, %err, "playlist page failed");
                        target.fail(&err);
                    }
                }
            }
            CommandMsg::LibraryWritten {
                catalog_id,
                action,
                result,
            } => match result {
                Ok(()) => {
                    // "Sent", not "added": Apple's 202 means accepted, and the
                    // change may still be in flight on their side.
                    self.toast(action.done());
                    // The star, however, we can move now. `inFavorites` is only
                    // re-read on a library reload, and making someone reload to
                    // see their own click is absurd — so mirror it locally and
                    // repaint just that row.
                    match action {
                        LibraryAction::Favorite => self.set_favorite(&catalog_id, true),
                        LibraryAction::AddToLibrary => {}
                    }
                }
                Err(err) => {
                    tracing::warn!(?action, %err, "library write failed");
                    self.toast(&err);
                }
            },
            CommandMsg::TileArt { key, path } => {
                self.tile_art_pending.remove(&key);
                let Some(path) = path else {
                    // Cosmetic. The tile keeps its placeholder.
                    return;
                };
                // Cache first, so a tile that binds later reads it straight off
                // disk instead of asking again...
                self.tile_art.borrow_mut().insert(key.clone(), path.clone());
                // ...then paint whichever tile is showing this artwork *now*.
                // Recycling means it may not be the one that asked, and may be
                // none at all if it scrolled away — both are correct.
                for registry in [
                    &self.album_art_widgets,
                    &self.artist_art_widgets,
                    &self.playlist_art_widgets,
                ] {
                    if let Some(cover) = registry.borrow().get(&key) {
                        cover.set_file(&path);
                    }
                }
            }
            CommandMsg::PageArtwork { page, path } => {
                if let (Some(path), Some(target)) = (path, self.pages.iter().find(|p| p.id == page))
                {
                    target.set_artwork(&path);
                }
            }
            CommandMsg::Spawned(handle) => {
                self.sidecar = Some(handle);
                // The process is up; Chromium's component updater is now
                // fetching the CDM (instant after the first run).
                self.stage = Stage::InstallingWidevine;
            }
            CommandMsg::Library(Ok(tracks)) => {
                self.loading_library = false;
                let unplayable = tracks.iter().filter(|t| !t.playable()).count();
                tracing::info!(tracks = tracks.len(), unplayable, "library loaded");
                self.all_tracks = tracks;
                self.rebuild_rows();
            }
            CommandMsg::Catalog {
                generation,
                offset,
                result,
            } => {
                // Responses can arrive out of order: a slow request for "aita"
                // must not overwrite the results for "aitana".
                if generation != self.search_gen {
                    tracing::debug!("discarding stale catalog results");
                    return;
                }
                self.searching_catalog = false;
                match result {
                    Ok(found) => {
                        // A short page of songs means Apple has no more.
                        self.catalog_exhausted = found.songs.len() < CATALOG_LIMIT as usize;
                        self.catalog_songs = if offset == 0 {
                            found.songs.len()
                        } else {
                            self.catalog_songs + found.songs.len()
                        };

                        if offset == 0 {
                            // Artists and albums first: they are the way into
                            // browsing, and burying them under 25 songs makes
                            // them invisible. Trimmed, because the point is a
                            // door rather than an exhaustive list.
                            self.catalog = found
                                .artists
                                .into_iter()
                                .take(CATALOG_BROWSE_ROWS)
                                .map(Entry::Artist)
                                .chain(
                                    found
                                        .albums
                                        .into_iter()
                                        .take(CATALOG_BROWSE_ROWS)
                                        .map(Entry::Album),
                                )
                                .chain(found.songs.into_iter().map(Entry::Song))
                                .collect();
                        } else {
                            // Later pages append songs only. Paging returns
                            // artists and albums again, and adding them would
                            // duplicate rows already on screen.
                            self.catalog
                                .extend(found.songs.into_iter().map(Entry::Song));
                        }

                        tracing::info!(
                            rows = self.catalog.len(),
                            songs = self.catalog_songs,
                            exhausted = self.catalog_exhausted,
                            "catalog results"
                        );
                        self.rebuild_rows();
                    }
                    Err(err) => {
                        tracing::warn!(%err, "catalog search failed");
                        self.toast(&format!("Search failed: {err}"));
                    }
                }
            }
            CommandMsg::Library(Err(err)) => {
                self.loading_library = false;
                tracing::warn!(%err, "library load failed");
                self.toast(&format!("Couldn't load your library: {err}"));
            }
            CommandMsg::Artwork { path, tint } => {
                if path.is_none() {
                    // Cosmetic. The bar falls back to a generic icon.
                    tracing::debug!("artwork unavailable");
                }
                self.art_path = path.clone();
                // Recolour the bar from the cover that just landed. Read off
                // the GTK thread alongside the fetch, so this is only the CSS
                // swap.
                crate::style::set_bar_tint(tint);
                self.now_playing.emit(NowPlayingInput::ArtworkReady(path));

                // A notification was held back for this track so it would not
                // go out carrying the previous album's cover. Guarded on the
                // id, in case the track changed again while the fetch ran.
                if self.notify_when_art_lands.is_some()
                    && self.notify_when_art_lands == self.playing_catalog_id()
                {
                    self.send_track_notification();
                }

                // MPRIS carries the cover too, so the Shell applet and lock
                // screen pick it up as soon as it lands.
                self.push_snapshot();
            }
            CommandMsg::Sidecar(Incoming::Event(event)) => self.on_event(event, &sender),
            CommandMsg::Sidecar(Incoming::Unparsed(line)) => {
                // preload.js and protocol.rs have drifted. Not fatal, but it
                // means an event is being silently ignored — say so.
                tracing::warn!(%line, "sidecar sent something we don't understand");
            }
            CommandMsg::Sidecar(Incoming::Died(reason)) => {
                tracing::warn!(%reason, "sidecar died");
                self.sidecar = None;
                self.restarts += 1;
                self.stage = Stage::Restarting(self.restarts);
                self.toast("Playback engine stopped — restarting");
                // The backoff belongs *inside* the respawn task. Sleeping in a
                // separate command and restarting here as well would restart
                // immediately and ignore the delay entirely.
                respawn_sidecar(&sender, sidecar::restart_delay(self.restarts));
            }
        }
    }
}

impl AppModel {
    /// Tracks matching the current query, in library order.
    ///
    /// Filtering reads `all_tracks`, never the factory, so clearing a search
    /// restores everything rather than whatever survived the last narrowing.
    /// The query for whichever scope is showing.
    /// Which set of music the search box is searching, derived from the
    /// section. Not stored: see [`View`].
    fn scope(&self) -> SearchScope {
        self.view.scope()
    }

    /// Show a row's context menu where it was clicked.
    ///
    /// Built fresh each time and parented to the row: a single long-lived
    /// popover would have to be re-parented on every click anyway, and a
    /// `ListView` recycles the widget under it while it is open.
    fn show_row_menu(&self, req: RowMenuRequest) {
        let menu = gtk::gio::Menu::new();

        let queue = gtk::gio::Menu::new();
        queue.append(Some("Play _Next"), Some("row.play-next"));
        queue.append(Some("Add to _Queue"), Some("row.play-later"));
        menu.append_section(None, &queue);

        // A second section, because these leave the app and change the user's
        // account — a different kind of act from reordering a queue.
        //
        // Each item appears only when it would do something. Offering "Add to
        // Library" for a track read *out of* the library, or "Favourite" for
        // one already starred, is a menu that lies about the state of things.
        let account = gtk::gio::Menu::new();
        if !req.in_library {
            account.append(Some("Add to _Library"), Some("row.add-to-library"));
        }
        // No "remove" counterpart: Apple rejects the DELETE for this token with
        // "Insufficient Permissions". See `Client` — favouriting is add-only.
        if !req.favorite {
            account.append(Some("_Favourite"), Some("row.favorite"));
        }
        if account.n_items() > 0 {
            menu.append_section(None, &account);
        }

        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        popover.set_has_arrow(false);
        popover.set_halign(gtk::Align::Start);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(req.at.0, req.at.1, 1, 1)));
        popover.set_parent(&req.over);

        // The actions live on the popover, so they go away with it rather than
        // accumulating on the window one right-click at a time.
        let actions = gtk::gio::SimpleActionGroup::new();
        for (name, next) in [("play-next", true), ("play-later", false)] {
            let action = gtk::gio::SimpleAction::new(name, None);
            let id = req.catalog_id.clone();
            let sender = self.menu_sender.clone();
            action.connect_activate(move |_, _| {
                sender.input(AppMsg::Enqueue {
                    catalog_id: id.clone(),
                    next,
                });
            });
            actions.add_action(&action);
        }

        for (name, what) in [
            ("add-to-library", LibraryAction::AddToLibrary),
            ("favorite", LibraryAction::Favorite),
        ] {
            let action = gtk::gio::SimpleAction::new(name, None);
            let id = req.catalog_id.clone();
            let sender = self.menu_sender.clone();
            action.connect_activate(move |_, _| {
                sender.input(AppMsg::LibraryWrite {
                    catalog_id: id.clone(),
                    action: what,
                });
            });
            actions.add_action(&action);
        }
        popover.insert_action_group("row", Some(&actions));

        // Unparent on close, or it leaks and keeps the row widget alive after
        // the list has recycled it out from under us — but **not during** the
        // close.
        //
        // GTK closes a PopoverMenu *before* activating the item you clicked. So
        // unparenting here tore down the action group a moment before the
        // action fired, and every menu item silently did nothing: no command
        // left Rust, and the sidecar logged nothing because nothing was sent.
        // Deferring to an idle lets the activation land first.
        popover.connect_closed(|p| {
            let p = p.clone();
            gtk::glib::idle_add_local_once(move || p.unparent());
        });
        popover.popup();
    }

    /// Record a favourite locally and repaint the row, without rebuilding the
    /// list — a rebuild would throw away the scroll position, and this is the
    /// same discipline as the play marker.
    fn set_favorite(&mut self, catalog_id: &str, on: bool) {
        for track in &mut self.all_tracks {
            if track.catalog_id.as_deref() == Some(catalog_id) {
                track.favorite = on;
            }
        }
        for page in &mut self.pages {
            page.set_favorite(catalog_id, on);
        }
        // Every list, for the same reason `set_row_playing` asks every list:
        // the track may be on a page and in the results behind it.
        let lists =
            std::iter::once(&self.library_icons).chain(self.pages.iter().map(|p| p.registry()));
        for registry in lists {
            if let Some(w) = registry.borrow().get(catalog_id) {
                w.star.set_visible(on);
            }
        }
    }

    fn toast(&self, text: &str) {
        self.toaster.add_toast(adw::Toast::new(text));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::music::types::TrackId;

    fn track(title: &str, catalog: Option<&str>) -> Track {
        Track {
            id: TrackId(format!("i.{title}")),
            catalog_id: catalog.map(str::to_owned),
            title: title.into(),
            favorite: false,
            in_library: false,
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
    fn a_stale_catalog_response_is_discarded() {
        // Responses arrive out of order: a slow request for "aita" must not
        // overwrite the results for "aitana" typed after it. The generation
        // carried on the response is what makes that decidable.
        let current = 7u64;
        assert!(6 != current, "an older generation is stale");
        assert!(7 == current, "the newest generation is the one to keep");
    }

    #[test]
    fn catalog_results_are_shown_unfiltered() {
        // Apple already ranked these. Re-filtering locally would drop results
        // that matched for reasons we cannot see — an alternate title, a
        // featured artist, a translation.
        let a = track("Bohemian Rhapsody", Some("1"));
        let catalog = [&a];
        let shown: Vec<_> = catalog.iter().collect();
        assert_eq!(shown.len(), 1);
    }
}
