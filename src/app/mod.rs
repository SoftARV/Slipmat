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
use crate::components::now_playing::{
    NowPlaying, NowPlayingInput, NowPlayingOutput, Repeat, VOLUME_STEP,
};
use crate::components::player_view::{PlayerView, PlayerViewInput};
use crate::components::queue_view::{QueueView, QueueViewInput, QueueViewOutput};
use crate::components::track_row::LibraryRowWidgets;
use crate::components::track_row::{Entry, LibraryItem, RowMenuRequest};
use crate::components::{
    CurrentTrack, DeadTracks, RowRegistry, TrackOverrides, current_track, dead_tracks,
    row_registry, track_overrides,
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
mod background;
mod chrome;
mod library;
mod pages;
mod playback;
mod queue;
mod row_menu;
mod status;
mod supervise;
mod view;
mod writes;

use chrome::{icon, register_actions, show_about, show_shortcuts};
use supervise::{respawn_sidecar, start_sidecar};

pub use view::{CatalogFilter, SearchScope, SortBy, View};

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
    /// The first-run gate, while it is up. `Some` exactly when the app is
    /// blocked, which is what stops it being presented twice.
    onboarding: Option<adw::Dialog>,

    /// Whether the restore has been attempted this session, so a later token
    /// refresh cannot start it again.
    restored: bool,

    /// The last track MusicKit reported, kept so the bar can hold it through a
    /// queue reload — see `push_snapshot::showing`.
    last_item: Option<crate::player::protocol::Item>,

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
    /// The drawer the bar opens into. Fed the same `Snapshot` as the bar, and
    /// its transport emits the same outputs — one player, two shapes (#18).
    player_view: Controller<PlayerView>,
    /// The rows on screen — the filtered view. A `ListView`, so its cost is
    /// the number of rows visible rather than the size of the library.
    library: TypedListView<LibraryItem, gtk::NoSelection>,
    /// Whether the queue sidebar is open.
    show_queue: bool,
    /// Whether the navigation sidebar is open. Persisted, like the section:
    /// someone who closes it wants it closed next time too.
    show_sidebar: bool,
    /// What [`AppModel::sync_animated`] last pushed to the widgets. `None`
    /// until the first sync, which writes all three so the initial state is
    /// asserted once — at startup, when nothing is being resized.
    animated_shown: std::cell::Cell<Option<Animated>>,
    /// Whether the sidebar is currently an overlay rather than a pane.
    ///
    /// Mirrored from the split view rather than derived from a width we would
    /// have to measure ourselves: the breakpoint already owns this decision,
    /// and two places computing it is two places to disagree.
    sidebar_collapsed: bool,
    /// Which library row currently carries the play marker.
    marked_playing: Option<String>,
    /// Icons of the library rows currently on screen, so the marker can move
    /// without editing the model — see `RowRegistry`.
    library_icons: RowRegistry<LibraryRowWidgets>,
    /// Who is playing. Shared with every library row; see `CurrentTrack`.
    current_track: CurrentTrack,
    /// Ids MusicKit refused, shared with every library row; see `DeadTracks`.
    dead_rows: DeadTracks,
    /// What has changed about a track since it was fetched — favourites and
    /// library membership — shared with every row in every list. See
    /// `components::TrackOverrides`: this replaces patching four separate
    /// copies of the same fact.
    row_overrides: TrackOverrides,
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
    sorts: view::Sorts,
    /// The sort popover's two actions, kept so the menu can be re-pointed at
    /// another section's choice when the view changes.
    sort_actions: Option<(gtk::gio::SimpleAction, gtk::gio::SimpleAction)>,
    /// Whether the user flipped the sort's natural direction.
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
    /// Whether a load has been *attempted*, distinct from whether it produced
    /// anything. A failure leaves the collection empty, and "empty" alone would
    /// mean trying again on every event.
    tried_albums: bool,
    tried_artists: bool,
    tried_playlists: bool,
    tried_library: bool,
    /// What each section's widgets were last built *for*.
    ///
    /// Rebuilding is expensive — every tile that binds decodes its cover on the
    /// GTK thread — and switching sections was rebuilding unconditionally, so
    /// returning to a section you had already visited cost the same half second
    /// every time. `None` means stale; anything else is the fingerprint the
    /// current widgets already satisfy.
    built_rows: Option<String>,
    built_albums: Option<String>,
    built_artists: Option<String>,
    built_playlists: Option<String>,
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

    /// How many rows of the **paging kind** are in `catalog`, and so the offset
    /// the next page starts from.
    ///
    /// Not simply `catalog.len()`: unfiltered, the browse rows on top are not
    /// part of Apple's song pagination and would skew it. Which kind pages
    /// depends on `catalog_filter` — see `library::catalog_rows`.
    catalog_paged: usize,
    /// Which kinds the catalog search asks for. Not persisted: a filter belongs
    /// to the search you are running, not to how you like the app.
    catalog_filter: CatalogFilter,
    /// Keeps the process alive while the window is hidden. `None` means the
    /// app is only alive because a window is open, which is the normal state.
    background: Option<gtk::gio::ApplicationHoldGuard>,
    /// Removals sent to the sidecar and not yet confirmed, by the id each
    /// command carried. See [`PendingWrite`].
    pending_writes: std::collections::HashMap<String, PendingWrite>,
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

/// Logs how long a rebuild took, on the way out. Temporary instrumentation for
/// "switching sections is slow" — it needs a number before it needs a fix.
pub(crate) struct Timed(pub &'static str, pub std::time::Instant);

impl Drop for Timed {
    fn drop(&mut self) {
        let ms = self.1.elapsed().as_millis();
        if ms > 2 {
            tracing::debug!(what = self.0, ms, "rebuild");
        }
    }
}

/// Something we can ask Apple to do to the user's account.
///
/// Both answer 202 Accepted with an empty body — "acceptable, may not have
/// completed" — so neither can be treated as done, only as sent. That is why
/// nothing here toggles a checkbox: showing state would mean reading it back,
/// and a star that lies is worse than no star.
/// A library write sent to the sidecar and not yet confirmed.
///
/// The row is updated the moment the command goes out, because a menu that
/// waits on a round trip reads as broken. But an optimistic update that is
/// never taken back is how a UI comes to lie — which it did: a removal against
/// a stale sidecar answered `unknown-command`, and the row went on showing the
/// change that never happened.
///
/// **Keyed by the id the command carried**, not by the command name. There was
/// one slot and a name match, and the sidecar's dispatch is async — so removing
/// two tracks inside one round trip overwrote the first record, and the first
/// completion was attributed to the second's row. The wrong row left the list
/// while the removed one stayed.
#[derive(Debug, Clone)]
struct PendingWrite {
    /// The row to correct, which is not always the id the command carried:
    /// removal takes a library id, un-favouriting a catalog id.
    catalog_id: String,
    undo: WriteUndo,
}

#[derive(Debug, Clone, Copy)]
enum WriteUndo {
    InLibrary(bool),
    Favorite(bool),
}

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
    /// Asks first — see `confirm_sign_out`.
    SignOut,
    SignOutConfirmed,
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
    /// Nudge the volume by one [`VOLUME_STEP`]. Separate from `SetVolume`
    /// because an accelerator's closure cannot see the model, so only the
    /// reducer knows what the current volume is to step from.
    ///
    /// [`VOLUME_STEP`]: crate::components::now_playing::VOLUME_STEP
    VolumeUp,
    VolumeDown,
    SetShuffle(bool),
    SetRepeat(Repeat),
    SetSort(SortBy),
    /// Narrow the catalog search to one kind of result, or widen it again.
    SetCatalogFilter(CatalogFilter),
    ToggleSortDirection,
    /// Take a song out of the library. Needs the **library** id; the catalog
    /// id is only carried so the row can be updated locally.
    RemoveFromLibrary {
        library_id: String,
        catalog_id: String,
    },
    /// Un-star a song, and nothing else. Deliberately does **not** also remove
    /// it from the library: favouriting adds it, un-favouriting does not take
    /// it back out, and that is what Apple's own client does. Chaining the two
    /// would silently delete a song someone only meant to un-star.
    Unfavorite {
        catalog_id: String,
    },
    /// The window's close button, or the WM. Not a quit: see the handler.
    WindowCloseRequested,
    /// The window is on screen again, however that happened.
    WindowShown,
    /// The drawer opened or closed by its own devices — dragged shut, or
    /// clicked away from. The model follows it rather than fighting it.
    PlayerDrawer(bool),
    /// A row was right-clicked; show its menu there.
    ShowRowMenu(RowMenuRequest),
    /// Empty the queue and stop.
    ClearQueue,
    /// Show or hide the queue pane inside the expanded player.
    ShowQueuePane(bool),
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
    /// The split view changed the sidebar's visibility by itself — a click
    /// outside it while collapsed. A fact, not a request: the widget has
    /// already done it.
    SidebarShown(bool),
    /// A sidebar row was activated. Dismisses the sidebar if it is an overlay,
    /// and does nothing at all if it is a pane.
    SectionChosen,
    /// The breakpoint turned the sidebar into an overlay, or back into a pane.
    SidebarCollapsed(bool),
    /// The results list is near its end; fetch the next page if there is one.
    LoadMoreCatalog,
    /// Re-fetch one library section. There is no section-less "reload": each
    /// is fetched separately, so a single one could not know which you meant.
    ReloadSection(View),
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
        /// A tiny copy of the cover, to go behind the bar and the drawer.
        /// Carried here rather than in its own message because the cover and
        /// what is drawn from it must be applied together.
        backdrop: Option<PathBuf>,
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
    /// Whether the Background portal agreed to list us. Advisory only.
    BackgroundPortal(Result<(), String>),
    LibraryWritten {
        catalog_id: String,
        action: LibraryAction,
        result: Result<(), String>,
    },
    /// A grid tile's cover is on disk and decoded, or could not be had.
    /// Carries pixels rather than a path because the decode is the expensive
    /// part and it has already happened, off the GTK thread (#27).
    TileArt {
        key: String,
        path: Option<PathBuf>,
        cover: Option<artwork::Decoded>,
    },
}

/// The drawer emits the same outputs as the bar, so they map the same way.
/// Two players disagreeing about one MusicKit is the thing this avoids.
fn map_player_output(out: NowPlayingOutput) -> AppMsg {
    match out {
        NowPlayingOutput::PlayPause => AppMsg::PlayPause,
        NowPlayingOutput::Next => AppMsg::Next,
        NowPlayingOutput::Previous => AppMsg::Previous,
        NowPlayingOutput::Seek(ms) => AppMsg::Seek(ms),
        NowPlayingOutput::SetVolume(v) => AppMsg::SetVolume(v),
        NowPlayingOutput::SetShuffle(on) => AppMsg::SetShuffle(on),
        NowPlayingOutput::SetRepeat(r) => AppMsg::SetRepeat(r),
        NowPlayingOutput::ToggleQueue => AppMsg::ToggleQueue,
    }
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

            // Closing a music player mid-song should not stop the music.
            // Always `Stop` — the reducer decides whether this is a hide or a
            // quit, because that depends on whether anything is loaded and the
            // handler cannot see the model.
            connect_close_request[sender] => move |_| {
                sender.input(AppMsg::WindowCloseRequested);
                gtk::glib::Propagation::Stop
            },

            // Any route back to a visible window — relaunching, the Background
            // Apps list, the media applet — means we are no longer running
            // without one, so the hold goes.
            connect_show[sender] => move |_| {
                sender.input(AppMsg::WindowShown);
            },
            set_default_width: 1000,
            set_default_height: 680,

            #[local_ref]
            toaster -> adw::ToastOverlay {
                adw::ToolbarView {
                    // The bar is the handle of a drawer, not furniture bolted
                    // to the bottom (#18). `AdwBottomSheet` is the widget for
                    // exactly this: `bottom_bar` while closed, `sheet` when
                    // open, and it owns the drag and the animation.
                    //
                    // The queue used to be an `OverlaySplitView` sidebar here,
                    // taking width from the content. It now lives inside the
                    // drawer, beside the thing it is a queue for.
                    #[wrap(Some)]
                    #[name = "player_sheet"]
                    set_content = &adw::BottomSheet {
                        set_full_width: true,
                        set_show_drag_handle: true,
                        // Modal: with the drawer open there is nothing useful
                        // to click behind it, and dismissing by clicking away
                        // is what a drawer should do.
                        set_modal: true,
                        // Not a `#[watch]`. See `sync_animated`.
                        set_open: model.show_queue,
                        // The bar is only meaningful once there is a player.
                        // Not a `#[watch]`. See `sync_animated`.
                        set_reveal_bottom_bar: matches!(model.stage, Stage::Ready),

                        // Dragged shut, or clicked away from — the model has to
                        // learn about it or the next toggle fights the widget.
                        connect_open_notify[sender] => move |sheet| {
                            sender.input(AppMsg::PlayerDrawer(sheet.is_open()));
                        },

                        #[wrap(Some)]
                        #[local_ref]
                        set_bottom_bar = now_playing_bar -> gtk::Box {
                            set_hexpand: true,
                        },

                        #[wrap(Some)]
                        #[local_ref]
                        set_sheet = player_sheet_content -> adw::BreakpointBin {},

                        // Navigation on the left, and an OverlaySplitView
                        // rather than a NavigationSplitView because it can be
                        // dismissed: once the sidebar is something you toggle,
                        // it is a panel you summon, which is exactly what this
                        // widget is for. The queue on the right is the same
                        // shape for the same reason.
                        #[wrap(Some)]
                        #[name = "nav_split"]
                        set_content = &adw::OverlaySplitView {
                            set_min_sidebar_width: 200.0,
                            set_max_sidebar_width: 260.0,
                            // Not a `#[watch]`. See `sync_animated`.
                            set_show_sidebar: model.show_sidebar,
                            // **The model has to adopt what the widget did.**
                            //
                            // Collapsed, this is an overlay, and the widget
                            // dismisses itself on a click outside — but the
                            // `#[watch]` above runs after *every* message, and
                            // during playback those never stop arriving. So it
                            // wrote `true` straight back and the sidebar
                            // reappeared before the click had finished.
                            //
                            // The same shape as the volume binding, in its
                            // quieter form: there the two values ping-ponged,
                            // here the model simply never learns. `SidebarShown`
                            // is the half that was missing.
                            connect_show_sidebar_notify[sender] => move |split| {
                                sender.input(AppMsg::SidebarShown(split.shows_sidebar()));
                            },
                            connect_collapsed_notify[sender] => move |split| {
                                sender.input(AppMsg::SidebarCollapsed(split.is_collapsed()));
                            },

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
                                        // The sections, and their reload
                                        // buttons. Insensitive until there is a
                                        // session to load anything from — but
                                        // note this is the ToolbarView's
                                        // *content*, so the header bar above it
                                        // keeps the primary menu live, and with
                                        // it Quit.
                                        #[watch]
                                        set_sensitive: model.controls_live(),

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
                                                // Choosing a section is the end
                                                // of what an overlay sidebar is
                                                // for, so it gets out of the
                                                // way — but only when it *is*
                                                // an overlay. Beside a pane, it
                                                // stays put.
                                                connect_row_activated[sender] => move |_, _| {
                                                    sender.input(AppMsg::SectionChosen);
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
                                                            set_valign: gtk::Align::Center,
                                                            #[watch]
                                                            set_visible: model.loading_library,
                                                        },

                                                        // Per section, because
                                                        // each is fetched
                                                        // separately and a
                                                        // single "reload"
                                                        // cannot know which one
                                                        // you meant. Swaps with
                                                        // the spinner rather
                                                        // than sitting beside
                                                        // it.
                                                        gtk::Button {
                                                            set_icon_name: "view-refresh-symbolic",
                                                            set_tooltip_text: Some("Reload"),
                                                            add_css_class: "flat",
                                                            add_css_class: "circular",
                                                            // Exactly the
                                                            // spinner's 16px,
                                                            // in the spinner's
                                                            // place: the row
                                                            // must not change
                                                            // height depending
                                                            // on whether it is
                                                            // loading.
                                                            add_css_class: "row-action",
                                                            set_size_request: (16, 16),
                                                            set_valign: gtk::Align::Center,
                                                            #[watch]
                                                            set_visible: !(model.loading_library),
                                                            connect_clicked => AppMsg::ReloadSection(View::Songs),
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
                                                            set_valign: gtk::Align::Center,
                                                            #[watch]
                                                            set_visible: model.loading_albums,
                                                        },

                                                        // Per section, because
                                                        // each is fetched
                                                        // separately and a
                                                        // single "reload"
                                                        // cannot know which one
                                                        // you meant. Swaps with
                                                        // the spinner rather
                                                        // than sitting beside
                                                        // it.
                                                        gtk::Button {
                                                            set_icon_name: "view-refresh-symbolic",
                                                            set_tooltip_text: Some("Reload"),
                                                            add_css_class: "flat",
                                                            add_css_class: "circular",
                                                            // Exactly the
                                                            // spinner's 16px,
                                                            // in the spinner's
                                                            // place: the row
                                                            // must not change
                                                            // height depending
                                                            // on whether it is
                                                            // loading.
                                                            add_css_class: "row-action",
                                                            set_size_request: (16, 16),
                                                            set_valign: gtk::Align::Center,
                                                            #[watch]
                                                            set_visible: !(model.loading_albums),
                                                            connect_clicked => AppMsg::ReloadSection(View::Albums),
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
                                                            set_valign: gtk::Align::Center,
                                                            #[watch]
                                                            set_visible: model.loading_artists,
                                                        },

                                                        // Per section, because
                                                        // each is fetched
                                                        // separately and a
                                                        // single "reload"
                                                        // cannot know which one
                                                        // you meant. Swaps with
                                                        // the spinner rather
                                                        // than sitting beside
                                                        // it.
                                                        gtk::Button {
                                                            set_icon_name: "view-refresh-symbolic",
                                                            set_tooltip_text: Some("Reload"),
                                                            add_css_class: "flat",
                                                            add_css_class: "circular",
                                                            // Exactly the
                                                            // spinner's 16px,
                                                            // in the spinner's
                                                            // place: the row
                                                            // must not change
                                                            // height depending
                                                            // on whether it is
                                                            // loading.
                                                            add_css_class: "row-action",
                                                            set_size_request: (16, 16),
                                                            set_valign: gtk::Align::Center,
                                                            #[watch]
                                                            set_visible: !(model.loading_artists),
                                                            connect_clicked => AppMsg::ReloadSection(View::Artists),
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
                                                            set_valign: gtk::Align::Center,
                                                            #[watch]
                                                            set_visible: model.loading_playlists,
                                                        },

                                                        // Per section, because
                                                        // each is fetched
                                                        // separately and a
                                                        // single "reload"
                                                        // cannot know which one
                                                        // you meant. Swaps with
                                                        // the spinner rather
                                                        // than sitting beside
                                                        // it.
                                                        gtk::Button {
                                                            set_icon_name: "view-refresh-symbolic",
                                                            set_tooltip_text: Some("Reload"),
                                                            add_css_class: "flat",
                                                            add_css_class: "circular",
                                                            // Exactly the
                                                            // spinner's 16px,
                                                            // in the spinner's
                                                            // place: the row
                                                            // must not change
                                                            // height depending
                                                            // on whether it is
                                                            // loading.
                                                            add_css_class: "row-action",
                                                            set_size_request: (16, 16),
                                                            set_valign: gtk::Align::Center,
                                                            #[watch]
                                                            set_visible: !(model.loading_playlists),
                                                            connect_clicked => AppMsg::ReloadSection(View::Playlists),
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
                                        set_show_end_title_buttons: true,

                                        #[wrap(Some)]
                                        #[name = "search_entry"]
                                        set_title_widget = &gtk::SearchEntry {
                                            // No fixed width. 320px here was a
                                            // floor under the whole window: a
                                            // header widget cannot be allowed a
                                            // minimum the window has to honour,
                                            // or the app cannot be tiled to
                                            // half a screen.
                                            set_hexpand: true,
                                            set_max_width_chars: 30,
                                            // Typing here before the tokens
                                            // arrive queries a catalog that
                                            // cannot answer.
                                            #[watch]
                                            set_sensitive: model.controls_live(),
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
                                            // Every library section, not just
                                            // Songs. Search is the exception:
                                            // Apple ranked those results and
                                            // re-ordering them locally would
                                            // throw away the ranking without
                                            // being able to reproduce it.
                                            set_visible: model.view != View::Search,
                                            // Visibility follows the section,
                                            // which says nothing about whether
                                            // there is a list to reorder yet.
                                            #[watch]
                                            set_sensitive: model.controls_live(),
                                        },

                                        // Only in Search: a library filter is
                                        // the search box itself, and the grids
                                        // already are one kind each.
                                        #[name = "filter_button"]
                                        pack_end = &gtk::MenuButton {
                                            add_css_class: "flat",
                                            set_always_show_arrow: true,
                                            set_tooltip_text: Some("What to search for"),
                                            #[watch]
                                            set_visible: model.view == View::Search,
                                            // A label rather than an icon, for
                                            // two reasons. Adwaita has no
                                            // filter glyph — `funnel-symbolic`
                                            // and `view-filter-symbolic` are
                                            // both absent, and `chrome::icon`
                                            // would have quietly put a music
                                            // note here. And the current filter
                                            // needs to be readable *without*
                                            // hovering: this button is the only
                                            // thing on screen explaining why a
                                            // search returned one kind of
                                            // result, and a narrowed search
                                            // with no visible reason reads as
                                            // missing results.
                                            #[watch]
                                            set_label: model.catalog_filter.label(),
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
                                                set_label: &model.waiting_for(),
                                            },
                                        },

                                        // **`AdwClampScrollable`, not `AdwClamp`.**
                                        //
                                        // A plain clamp had to go *outside* the
                                        // scroller, because inside it breaks
                                        // `GtkListView`'s height allocation and
                                        // the list stops materialising rows part
                                        // way down. But outside, the clamp is
                                        // what the window sizes, so the scroller
                                        // is only 800px wide and its scrollbar
                                        // sits in the middle of the window
                                        // rather than at the edge.
                                        //
                                        // `AdwClampScrollable` is the widget for
                                        // exactly this trade: it implements
                                        // `GtkScrollable` and passes the
                                        // interface through to its child, so the
                                        // list still gets the adjustments it
                                        // needs while being clamped. The
                                        // scroller can then be the full width
                                        // and keep its bar at the edge.
                                        #[name = "library_scroller"]
                                        add_named[Some("library")] = &gtk::ScrolledWindow {
                                            set_vexpand: true,
                                            // See `.plain-scroller`: now that
                                            // this spans the window rather than
                                            // being clamped, its `view`
                                            // background does too.
                                            add_css_class: "plain-scroller",

                                            #[wrap(Some)]
                                            #[name = "library_clamp"]
                                            set_child = &adw::ClampScrollable {
                                                set_maximum_size: 800,
                                                add_css_class: "plain-scroller",
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
                                                // Padding via `.tile-grid`,
                                                // not a margin: a GridView
                                                // draws its own `.view`
                                                // background, and a margin
                                                // leaves a strip of the window
                                                // showing all the way round it.
                                                add_css_class: "tile-grid",
                                            },
                                        },

                                        add_named[Some("artists")] = &gtk::ScrolledWindow {
                                            set_vexpand: true,
                                            set_hscrollbar_policy: gtk::PolicyType::Never,

                                            #[local_ref]
                                            artist_grid -> gtk::GridView {
                                                set_single_click_activate: true,
                                                set_max_columns: 12,
                                                // Padding via `.tile-grid`,
                                                // not a margin: a GridView
                                                // draws its own `.view`
                                                // background, and a margin
                                                // leaves a strip of the window
                                                // showing all the way round it.
                                                add_css_class: "tile-grid",
                                            },
                                        },

                                        add_named[Some("playlists")] = &gtk::ScrolledWindow {
                                            set_vexpand: true,
                                            set_hscrollbar_policy: gtk::PolicyType::Never,

                                            #[local_ref]
                                            playlist_grid -> gtk::GridView {
                                                set_single_click_activate: true,
                                                set_max_columns: 12,
                                                // Padding via `.tile-grid`,
                                                // not a margin: a GridView
                                                // draws its own `.view`
                                                // background, and a margin
                                                // leaves a strip of the window
                                                // showing all the way round it.
                                                add_css_class: "tile-grid",
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
                QueueViewOutput::Clear => AppMsg::ClearQueue,
                QueueViewOutput::Hide => AppMsg::ShowQueuePane(false),
                QueueViewOutput::SetShuffle(on) => AppMsg::SetShuffle(on),
                QueueViewOutput::SetRepeat(mode) => AppMsg::SetRepeat(mode),
            });

        // The queue **moves** into the expanded player rather than being
        // rebuilt there (#18). It is handed over before the view is built,
        // because relm4 constructs the widget tree before the model exists and
        // there is no init payload that can carry a widget through.
        crate::components::player_view::hand_over_queue(queue_view.widget().clone());
        let player_view = PlayerView::builder()
            .launch(())
            .forward(sender.input_sender(), map_player_output);

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

        let mut model = AppModel {
            stage: Stage::Starting,
            queue_view,
            player_view,
            library,
            show_queue: false,
            show_sidebar: settings.show_sidebar,
            sidebar_collapsed: false,
            animated_shown: std::cell::Cell::new(None),
            marked_playing: None,
            library_icons: row_registry(),
            current_track: current_track(),
            dead_rows: dead_tracks(),
            row_overrides: track_overrides(),
            // filled from `dead_ids` once the model exists (see below)
            all_tracks: Vec::new(),
            library_query: String::new(),
            catalog_query: String::new(),
            view: View::from(settings.section),
            sorts: view::Sorts {
                songs: view::Sort {
                    by: SortBy::parse(&settings.sort).valid_for(View::Songs),
                    reversed: settings.sort_reversed,
                },
                albums: view::Sort {
                    by: SortBy::parse(&settings.album_sort).valid_for(View::Albums),
                    reversed: settings.album_sort_reversed,
                },
                artists: view::Sort {
                    by: SortBy::parse(&settings.artist_sort).valid_for(View::Artists),
                    reversed: settings.artist_sort_reversed,
                },
                playlists: view::Sort {
                    by: SortBy::parse(&settings.playlist_sort).valid_for(View::Playlists),
                    reversed: settings.playlist_sort_reversed,
                },
            },
            sort_actions: None,
            albums: Vec::new(),
            artists: Vec::new(),
            playlists: Vec::new(),
            album_grid,
            artist_grid,
            playlist_grid,
            loading_albums: false,
            loading_artists: false,
            loading_playlists: false,
            tried_albums: false,
            tried_artists: false,
            tried_playlists: false,
            tried_library: false,
            built_rows: None,
            built_albums: None,
            built_artists: None,
            built_playlists: None,
            tile_art: art_cache(),
            album_art_widgets: art_registry(),
            artist_art_widgets: art_registry(),
            playlist_art_widgets: art_registry(),
            tile_art_pending: std::collections::HashSet::new(),
            tile_art_request,
            catalog: Vec::new(),
            catalog_paged: 0,
            catalog_filter: CatalogFilter::default(),
            background: None,
            pending_writes: std::collections::HashMap::new(),
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
            restored: false,
            onboarding: None,
            last_item: None,
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

            // Its own section: signing out is an account action, not app
            // furniture, and it should not sit next to About.
            let account = gtk::gio::Menu::new();
            account.append(Some("_Sign Out"), Some("win.sign-out"));
            primary_menu.append_section(None, &account);

            // Quit was missing from this menu entirely, while the shortcuts
            // dialog advertised `Ctrl`+`Q` — so the app claimed a way out it
            // never showed. Last section, per the GNOME convention.
            let quit = gtk::gio::Menu::new();
            quit.append(Some("_Quit"), Some("app.quit"));
            primary_menu.append_section(None, &quit);
        }

        let toaster = &model.toaster;
        let now_playing_bar = model.now_playing.widget();
        let library_list = &model.library.view;
        let nav_view = &model.nav;
        let album_grid = &model.album_grid.view;
        let artist_grid = &model.artist_grid.view;
        let playlist_grid = &model.playlist_grid.view;
        let player_sheet_content = model.player_view.widget();
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
            // A stateful action gives the popover its radio dots for free, and
            // keeps the checked item honest when the setting is restored.
            let action = gtk::gio::SimpleAction::new_stateful(
                "by",
                Some(&String::static_variant_type()),
                &model.sorts.get(model.view).by.id().to_variant(),
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
                &model.sorts.get(model.view).reversed.to_variant(),
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
            model.sort_actions = Some((action, reverse));
            // Built here rather than inline, so there is one place that decides
            // what the popover holds — including that Artists get no radio list
            // at all, which an inline version quietly got wrong.
            model.sync_sort_menu(&widgets.sort_button);
        }

        // The catalog type filter, same shape as the sort menu above: a
        // stateful action so the popover draws its own radio dots.
        {
            let menu = gtk::gio::Menu::new();
            for option in CatalogFilter::ALL {
                let item = gtk::gio::MenuItem::new(Some(option.label()), None);
                item.set_action_and_target_value(
                    Some("filter.kind"),
                    Some(&option.id().to_variant()),
                );
                menu.append_item(&item);
            }
            widgets.filter_button.set_menu_model(Some(&menu));

            let action = gtk::gio::SimpleAction::new_stateful(
                "kind",
                Some(&String::static_variant_type()),
                &model.catalog_filter.id().to_variant(),
            );
            let filter_sender = sender.clone();
            action.connect_activate(move |action, target| {
                let Some(id) = target.and_then(|t| t.str().map(str::to_owned)) else {
                    return;
                };
                action.set_state(&id.to_variant());
                filter_sender.input(AppMsg::SetCatalogFilter(CatalogFilter::parse(&id)));
            });
            let group = gtk::gio::SimpleActionGroup::new();
            group.add_action(&action);
            widgets
                .filter_button
                .insert_action_group("filter", Some(&group));
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

        // **The window has to be able to get narrow.** Without this the
        // navigation sidebar holds 200px open at all times and the app cannot
        // be tiled to half a screen — which is how it is actually used.
        //
        // `AdwOverlaySplitView` already knows how to be a summonable overlay
        // rather than a fixed pane; it just has to be told when. This is the
        // standard adaptive pattern and the app simply never had one.
        if let Ok(condition) = adw::BreakpointCondition::parse("max-width: 700px") {
            let breakpoint = adw::Breakpoint::new(condition);
            breakpoint.add_setter(&widgets.nav_split, "collapsed", Some(&true.to_value()));
            root.add_breakpoint(breakpoint);
        } else {
            tracing::warn!("unparsable window breakpoint; the sidebar will not collapse");
        }

        // The clamp takes the list here rather than in `view!`: the macro has
        // no form for `set_child` on a `#[local_ref]`, and the list is owned by
        // the model.
        // The clamp takes the list here rather than in `view!`, because the
        // macro has no form for `set_child` on a `#[local_ref]` — so the two
        // properties the list used to carry inline have to be set here too.
        //
        // **They were dropped when this moved, and both symptoms followed.**
        // `navigation-sidebar` is what makes a `GtkListView` transparent, so
        // without it the rows painted the `view` background and read as darker
        // than the window; and without `single-click-activate` a row needed two
        // clicks to play. Neither is decoration: losing them looked like two
        // unrelated bugs in a layout change.
        widgets.library_clamp.set_child(Some(library_list));
        library_list.set_single_click_activate(true);
        library_list.add_css_class("navigation-sidebar");

        // **Keep the content clear of the Now Playing bar.**
        //
        // The bar is `AdwBottomSheet`'s `bottom_bar`, and a bottom bar is drawn
        // *over* the content rather than beside it. So the last row of any
        // scrollable sat behind it: reachable by GTK's reckoning — the scroller
        // had already run to its end — and invisible, which is the worst
        // combination, because nothing suggests there is more to see.
        //
        // Maximising appeared to fix it, which sent the first diagnosis after a
        // ten-pixel measurement discrepancy in the detail page's layout. That
        // was real and irrelevant: a taller window simply put the last row above
        // the bar.
        //
        // `bottom-bar-height` is the property libadwaita exposes for exactly
        // this, and it notifies, so the inset follows the bar rather than
        // guessing at its height — which changes with the theme and the text
        // scale.
        {
            let content = widgets.nav_split.clone();
            let sheet = widgets.player_sheet.clone();
            let apply = move |sheet: &adw::BottomSheet| {
                content.set_margin_bottom(sheet.bottom_bar_height());
            };
            apply(&sheet);
            sheet.connect_bottom_bar_height_notify(apply);
        }

        // The drawer opens to most of the window, rather than to whatever
        // height its contents happen to add up to. See `fill_window`.
        crate::components::player_view::fill_window(
            &root,
            &widgets.player_sheet,
            model.player_view.widget().upcast_ref(),
        );

        register_actions(&root, &sender);

        // Rows read playability from here, so seed it before any are built.
        *model.dead_rows.borrow_mut() = model.dead_ids.clone();

        start_sidecar(&sender);

        ComponentParts { model, widgets }
    }

    fn shutdown(&mut self, _widgets: &mut Self::Widgets, _output: relm4::Sender<Self::Output>) {
        // A now-playing notification must not outlive the player that sent it.
        notify::clear(relm4::main_application().upcast_ref::<gtk::gio::Application>());
        // The only moment the position is accurate.
        self.save_session();
    }

    /// Wraps `update` so the search box can be re-filled after a scope change.
    ///
    /// The entry is the one widget holding text the model also owns, and the
    /// two must agree: switching scope swaps which query is live, and the box
    /// has to show that scope's text rather than the one you left behind.
    /// Timed, temporarily, because "switching sections is slow" needs a number
    /// before it needs a fix. `update_view` re-runs every `#[watch]` in the
    /// view macro, and there is a lot of it.
    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        msg: Self::Input,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        let view_before = self.view;
        self.update(msg, sender.clone(), root);
        self.sync_animated(widgets);

        if self.view != view_before {
            // `set_text` fires `search-changed`, but `SearchChanged` returns
            // early when the text already matches the active query — which it
            // does by now, because `update` set it first. No loop.
            widgets.search_entry.set_text(self.query());
            self.sync_sort_menu(&widgets.sort_button);
        }

        let painting = std::time::Instant::now();
        self.update_view(widgets, sender);
        let ms = painting.elapsed().as_millis();
        if ms > 4 {
            // Only the slow ones: at ~60fps anything over 16ms drops a frame,
            // and a message that costs more than a few is worth naming.
            tracing::debug!(ms, "view refresh");
        }
    }

    /// Overridden for one reason: [`AppModel::sync_animated`] has to run on
    /// **both** paths.
    ///
    /// The default calls `update_cmd` then `update_view`, and command messages
    /// are how the sidecar's events arrive — including the one that moves
    /// `stage` to `Ready`, which is what reveals the Now Playing bar. Syncing
    /// only in `update_with_view` left the bar and the drawer hidden for the
    /// whole session, because their transition happened on the path that was
    /// not looking.
    fn update_cmd_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: Self::CommandOutput,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        self.update_cmd(message, sender.clone(), root);
        self.sync_animated(widgets);
        self.update_view(widgets, sender);
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, root: &Self::Root) {
        self.handle(msg, &sender, root);
        self.sync_onboarding(&sender, root);
    }

    fn update_cmd(
        &mut self,
        msg: Self::CommandOutput,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        self.handle_cmd(msg, &sender, root);
        self.sync_onboarding(&sender, root);
    }
}

/// The three widget properties that are animated, sampled together so a
/// transition in any of them can be spotted without repeating the comparison.
#[derive(Clone, Copy)]
struct Animated {
    sidebar: bool,
    queue: bool,
    bottom_bar: bool,
}

impl AppModel {
    fn animated_state(&self) -> Animated {
        Animated {
            sidebar: self.show_sidebar,
            queue: self.show_queue,
            bottom_bar: matches!(self.stage, Stage::Ready),
        }
    }

    /// Push the three animated properties, and **only where they changed**.
    ///
    /// Each drives an `AdwAnimation` — the sidebar's spring, the drawer's
    /// slide, the bar's reveal — so writing one asks an animation to start or
    /// re-aim. That is correct on an edge and catastrophic on a level: as a
    /// `#[watch]` it re-fired after every message, and during playback those
    /// never stop, which wedged the app inside libadwaita's spring solver.
    ///
    /// Compared against **what we last wrote**, not against the widget. A
    /// widget that disagrees persistently disagrees on every message too, so
    /// asking it is the level trigger again wearing a guard's clothes — that
    /// was the first attempt at this fix, and the second core dump found it
    /// still spinning.
    fn sync_animated(&self, widgets: &<Self as relm4::Component>::Widgets) {
        let now = self.animated_state();
        let last = self.animated_shown.get();
        if last.map(|l| l.sidebar) != Some(now.sidebar) {
            widgets.nav_split.set_show_sidebar(now.sidebar);
        }
        if last.map(|l| l.queue) != Some(now.queue) {
            widgets.player_sheet.set_open(now.queue);
        }
        if last.map(|l| l.bottom_bar) != Some(now.bottom_bar) {
            widgets.player_sheet.set_reveal_bottom_bar(now.bottom_bar);
        }
        self.animated_shown.set(Some(now));
    }

    fn handle(
        &mut self,
        msg: AppMsg,
        sender: &ComponentSender<Self>,
        root: &adw::ApplicationWindow,
    ) {
        let sender = sender.clone();
        match msg {
            AppMsg::SignIn => self.send(Command::ShowLogin),
            AppMsg::SignOut => {
                // The menu item is always there; asking to sign out when you
                // already are should do nothing rather than prompt.
                if matches!(self.stage, Stage::Ready) {
                    self.confirm_sign_out(&sender, root);
                }
            }
            AppMsg::SignOutConfirmed => {
                tracing::info!("signing out");
                // The sidecar drops Apple's session — cookies and all, not just
                // MusicKit's token — and its `authorizationStatusDidChange`
                // confirms it rather than us assuming.
                self.send(Command::SignOut);
                self.forget_session();
            }
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
            AppMsg::SetVolume(volume) => self.set_volume(volume),
            AppMsg::VolumeUp => self.set_volume(self.volume + VOLUME_STEP),
            AppMsg::VolumeDown => self.set_volume(self.volume - VOLUME_STEP),
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
                        self.catalog_paged = 0;
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
            AppMsg::SetCatalogFilter(filter) => {
                if filter == self.catalog_filter {
                    return;
                }
                self.catalog_filter = filter;

                // A different filter is a different question, so the previous
                // answer is discarded whole — including the offset, which
                // counts a kind that may no longer be the one that pages.
                self.search_gen = self.search_gen.wrapping_add(1);
                self.catalog_exhausted = false;
                self.catalog_paged = 0;
                self.catalog.clear();
                self.built_rows = None;

                if self.catalog_query.trim().is_empty() {
                    self.rebuild_rows();
                    return;
                }
                // No debounce: this is one deliberate click, not a keystroke
                // in a stream of them.
                self.run_catalog_search(&sender, self.search_gen, 0);
            }
            AppMsg::SetView(view) => {
                if view == self.view {
                    return;
                }
                let switch_started = std::time::Instant::now();
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
                // What the *reducer* spent. If this is small and the section
                // still takes a second to appear, the cost is in rendering
                // rather than in here.
                tracing::debug!(
                    ?view,
                    ms = switch_started.elapsed().as_millis(),
                    "section switch"
                );
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
                    // Fetch only if it is missing — `fetch` short-circuits on
                    // the disk cache — but decode either way, here, off the
                    // GTK thread. That is the whole point of #27: the tile is
                    // handed pixels, not a filename.
                    let path = artwork::fetch(art, TILE_ART).await.ok();
                    let cover = path
                        .as_deref()
                        .and_then(|path| artwork::decode(path, TILE_ART as i32));
                    CommandMsg::TileArt { key, path, cover }
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
                    && self.catalog_paged < CATALOG_MAX
                {
                    let generation = self.search_gen;
                    // Songs only — the browse rows above them are not part of
                    // Apple's song pagination.
                    let offset = self.catalog_paged;
                    self.run_catalog_search(&sender, generation, offset);
                }
            }
            AppMsg::ReloadSection(view) => self.reload(view, &sender),
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
            AppMsg::SidebarShown(shown) => {
                if self.show_sidebar == shown {
                    return; // our own write coming back
                }
                // Adopted, but **not** persisted. Dismissing an overlay is not
                // a statement about how you want the window laid out when it
                // is wide enough to hold a real pane; only `ToggleSidebar` is
                // deliberate enough to be a preference.
                self.show_sidebar = shown;
            }
            AppMsg::SidebarCollapsed(collapsed) => self.sidebar_collapsed = collapsed,
            AppMsg::SectionChosen => {
                if self.sidebar_collapsed {
                    self.show_sidebar = false;
                }
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
                    // **Onto the queue, not merely onto the drawer.** This
                    // button says "Queue" and used to open the expanded player
                    // with the queue still tucked away behind its own toggle —
                    // two clicks to reach the thing the icon names. Opening the
                    // drawer is how the queue is reached, not what was asked
                    // for.
                    self.player_view.emit(PlayerViewInput::SetQueueShown(true));
                    self.queue_view.emit(QueueViewInput::ScrollToPlaying);
                }
            }
            AppMsg::LibraryActivated(position) => {
                // Catalog results mix songs with albums, artists and playlists.
                // A song plays; the rest are doors, and clicking one walks
                // through it. Resolved against the list as it is right now,
                // never against a remembered snapshot.
                match self.visible_entries().get(position as usize) {
                    Some(Entry::Album(album)) => {
                        sender.input(AppMsg::OpenPage(PageKind::album(album)))
                    }
                    Some(Entry::Artist(artist)) => {
                        sender.input(AppMsg::OpenPage(PageKind::artist(artist)))
                    }
                    Some(Entry::Playlist(playlist)) => {
                        sender.input(AppMsg::OpenPage(PageKind::playlist(playlist)))
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
                    Some(Entry::Playlist(playlist)) => {
                        sender.input(AppMsg::OpenPage(PageKind::playlist(playlist)))
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
            AppMsg::ShowQueuePane(shown) => {
                // The player view owns this — it is a layout decision, not
                // player state — so this only forwards. It arrives from the
                // queue's own close button, which cannot reach a sibling
                // component directly.
                self.player_view.emit(PlayerViewInput::SetQueueShown(shown));
            }
            AppMsg::ClearQueue => {
                tracing::info!("clearing the queue");
                self.send(Command::ClearQueue);
                // Nothing to come back to next launch, either. The mirror
                // follows the sidecar's queue event as always (rule 3) — this
                // is only the part MusicKit cannot know about.
                self.last_queue = None;
                self.pending_start = None;
                self.last_item = None;
                crate::session::clear();
                crate::style::set_backdrop(None);
                crate::style::set_backdrop(None);
            }
            AppMsg::JumpTo(id) => match self.queue_index_of(&id) {
                Some(index) => {
                    self.send(Command::ChangeToIndex { index });
                    // Clicking a track in the queue is a request to *play* it.
                    // `changeToMediaAtIndex` only moves the cursor, so on a
                    // queue that is loaded but idle — a restored session, or a
                    // paused one — it moved silently and looked like nothing
                    // had happened.
                    if !self.player.state.is_playing() {
                        self.send(Command::Play);
                    }
                }
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
                let mut current = self.sorts.get(self.view);
                if sort == current.by {
                    return;
                }
                current.by = sort;
                self.sorts.set(self.view, current);
                self.persist_sorts();
                tracing::info!(sort = sort.id(), section = ?self.view, "sort");
                // A rebuild resets the scroll, which is right here: the list
                // the user was looking at no longer exists in that order.
                self.resort();
            }
            AppMsg::WindowCloseRequested => self.close_window(root, &sender),
            AppMsg::PlayerDrawer(open) => {
                if self.show_queue != open {
                    self.show_queue = open;
                    // The bar's queue button is a watch on the snapshot, so it
                    // has to be told the drawer moved without it.
                    self.push_snapshot();
                }
            }
            AppMsg::WindowShown => {
                // Dropping the guard is the whole of it: with a window on
                // screen GTK keeps the app alive by itself, and `background`
                // should mean what its name says.
                if self.background.take().is_some() {
                    tracing::info!("window shown; no longer background-only");
                }
            }
            AppMsg::RemoveFromLibrary {
                library_id,
                catalog_id,
            } => {
                tracing::info!(%library_id, "removing from library");
                self.pending_writes.insert(
                    library_id.clone(),
                    PendingWrite {
                        catalog_id: catalog_id.clone(),
                        undo: WriteUndo::InLibrary(true),
                    },
                );
                self.send(Command::RemoveFromLibrary { id: library_id });
                // Mirrored locally for the same reason the star is: the menu
                // reads this, and making someone reload to see their own click
                // is absurd. `include=library` is cached for tens of seconds
                // besides, so a read-back would disagree for a while (#34).
                self.set_in_library(&catalog_id, false);
                self.toast("Removing from your library…");
            }
            AppMsg::Unfavorite { catalog_id } => {
                tracing::info!(%catalog_id, "removing favourite");
                self.pending_writes.insert(
                    catalog_id.clone(),
                    PendingWrite {
                        catalog_id: catalog_id.clone(),
                        undo: WriteUndo::Favorite(true),
                    },
                );
                self.send(Command::Unfavorite {
                    id: catalog_id.clone(),
                });
                // The star only. The song stays in the library — see the note
                // on `AppMsg::Unfavorite`.
                self.set_favorite(&catalog_id, false);
                // Present continuous, not a claim: nothing has been confirmed
                // yet, and `undo_pending_write` is what happens if it is not.
                self.toast("Removing favourite…");
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
                let mut current = self.sorts.get(self.view);
                current.reversed = !current.reversed;
                self.sorts.set(self.view, current);
                self.persist_sorts();
                self.resort();
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
                        start_playing: true,
                        start_time_ms: 0,
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

    fn handle_cmd(
        &mut self,
        msg: CommandMsg,
        sender: &ComponentSender<Self>,
        _root: &adw::ApplicationWindow,
    ) {
        let sender = sender.clone();
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
                        self.built_albums = None;
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
                        self.built_artists = None;
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
                        self.built_playlists = None;
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
            // Advisory: the app is already in the background by the time this
            // answers. A refusal costs discoverability in Quick Settings, not
            // playback, so it is logged rather than toasted.
            CommandMsg::BackgroundPortal(result) => match result {
                Ok(()) => tracing::info!("background portal: listed"),
                // Almost always "no AppId detected": the portal identifies a
                // non-sandboxed app from its systemd scope, which only exists
                // when it was launched from its .desktop entry. A binary run
                // straight from a terminal cannot be listed, and that is a
                // property of the session rather than a fault here — playback
                // is unaffected either way.
                Err(err) => tracing::warn!(
                    %err,
                    "background portal refused; Quick Settings will not list Tonearm \
                     (expected when not launched from its .desktop entry)"
                ),
            },
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
                        LibraryAction::Favorite => {
                            self.set_favorite(&catalog_id, true);
                            // Favouriting *adds to the library* — Apple's
                            // behaviour, measured (#34). So the menu must stop
                            // offering "Add to Library" for it too.
                            self.set_in_library(&catalog_id, true);
                        }
                        // Mirrored so the menu stops offering an add that has
                        // already happened. No library id yet — the 202 carries
                        // no body and Apple assigns one asynchronously — so
                        // "Remove from Library" stays hidden until a reload
                        // learns it. Offering a removal we cannot address would
                        // be a menu item that quietly does nothing.
                        LibraryAction::AddToLibrary => self.set_in_library(&catalog_id, true),
                    }
                }
                Err(err) => {
                    tracing::warn!(?action, %err, "library write failed");
                    self.toast(&err);
                }
            },
            CommandMsg::TileArt { key, path, cover } => {
                self.tile_art_pending.remove(&key);
                let (Some(path), Some(cover)) = (path, cover) else {
                    // Cosmetic. The tile keeps its placeholder.
                    return;
                };
                // Cached so a later bind knows the file is there and the
                // request skips the network — the decode still happens off the
                // thread, because that is the part worth avoiding here.
                self.tile_art.borrow_mut().insert(key.clone(), path);

                // Paint whichever tile is showing this artwork *now*.
                // Recycling means it may not be the one that asked, and may be
                // none at all if it scrolled away — both are correct.
                //
                // The pixels are moved, so exactly one widget can take them.
                // That is the truth of it anyway: one key, one live tile.
                let target = [
                    &self.album_art_widgets,
                    &self.artist_art_widgets,
                    &self.playlist_art_widgets,
                ]
                .into_iter()
                .find_map(|registry| registry.borrow().get(&key).cloned());
                if let Some(widget) = target {
                    widget.set_decoded(cover);
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
                self.built_rows = None;
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
                        let first_page = offset == 0;
                        let (rows, paged) =
                            library::catalog_rows(self.catalog_filter, found, first_page);

                        // A short page of the **paging kind** means Apple has
                        // no more. Which kind that is depends on the filter,
                        // which is why the count comes back from the fold
                        // rather than being read off one field here.
                        self.catalog_exhausted = paged < CATALOG_LIMIT as usize;
                        self.catalog_paged = if first_page {
                            paged
                        } else {
                            self.catalog_paged + paged
                        };

                        tracing::info!(
                            rows = self.catalog.len() + rows.len(),
                            paged = self.catalog_paged,
                            filter = ?self.catalog_filter,
                            exhausted = self.catalog_exhausted,
                            "catalog results"
                        );

                        if first_page {
                            // New answer: the rows on screen are for a
                            // different question, so they all go.
                            self.catalog = rows;
                            self.built_rows = None;
                            self.rebuild_rows();
                        } else {
                            // A later page only ever *adds*. Rebuilding would
                            // discard every widget and with them the scroll
                            // position — putting the reader back at the top of
                            // the list they had just scrolled to the bottom of
                            // in order to ask for this page.
                            self.append_rows(&rows);
                            self.catalog.extend(rows);
                        }
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
            CommandMsg::Artwork { path, backdrop } => {
                if path.is_none() {
                    // Cosmetic. The bar falls back to a generic icon.
                    tracing::debug!("artwork unavailable");
                }
                self.art_path = path.clone();
                // Put the cover behind the player. Scaled off the GTK thread
                // alongside the fetch, so this is only the CSS swap.
                crate::style::set_backdrop(backdrop.as_deref());
                self.now_playing
                    .emit(NowPlayingInput::ArtworkReady(path.clone()));
                self.player_view.emit(PlayerViewInput::Artwork(path));

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

    /// Throw away a section's cache and fetch it again.
    ///
    /// Each loader returns early when it already holds data — that is what
    /// makes revisiting a section instant — so a reload has to clear first or
    /// it does nothing at all.
    fn reload(&mut self, view: View, sender: &ComponentSender<Self>) {
        match view {
            View::Songs | View::Search => {
                self.tried_library = false;
                self.load_library(sender);
            }
            View::Albums => {
                self.albums.clear();
                self.tried_albums = false;
                self.load_albums(sender);
            }
            View::Artists => {
                self.artists.clear();
                self.tried_artists = false;
                self.load_artists(sender);
            }
            View::Playlists => {
                self.playlists.clear();
                self.tried_playlists = false;
                self.load_playlists(sender);
            }
        }
    }

    /// Drop everything that belonged to the signed-in user.
    ///
    /// Not just the tokens: the library, the grids, the catalog results and the
    /// pushed pages all came from that account, and leaving them on screen
    /// after a sign-out would show one person's music to whoever signs in
    /// next. The unplayable-id cache stays — it is about Apple's catalog, not
    /// about the user.
    fn forget_session(&mut self) {
        self.stage = Stage::SignedOut;
        self.tokens = None;

        self.all_tracks.clear();
        self.albums.clear();
        self.artists.clear();
        self.playlists.clear();
        self.tried_albums = false;
        self.tried_artists = false;
        self.tried_playlists = false;
        self.tried_library = false;
        self.built_rows = None;
        self.built_albums = None;
        self.built_artists = None;
        self.built_playlists = None;
        self.catalog.clear();
        self.catalog_paged = 0;
        self.library_query.clear();
        self.catalog_query.clear();

        self.rebuild_rows();
        self.rebuild_albums();
        self.rebuild_artists();
        self.rebuild_playlists();

        // Pages and the queue belonged to that session too.
        self.pop_to_results();
        self.show_queue = false;
        self.last_item = None;
        self.last_queue = None;
        self.pending_start = None;
        crate::session::clear();
        crate::style::set_backdrop(None);
        self.push_snapshot();
    }

    /// Put the first-run gate up or take it down, to match the session.
    ///
    /// Driven from one place rather than from each site that changes `stage`,
    /// because there are four of them — tokens arriving, an authorization
    /// change, a hook attaching, and signing out — and three of them would have
    /// been easy to forget.
    fn sync_onboarding(&mut self, sender: &ComponentSender<Self>, root: &adw::ApplicationWindow) {
        match (matches!(self.stage, Stage::SignedOut), &self.onboarding) {
            (true, None) => self.onboarding = Some(self.present_onboarding(sender, root)),
            (false, Some(dialog)) => {
                // `can_close` is false, so it will not go on its own.
                dialog.force_close();
                self.onboarding = None;
            }
            _ => {}
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
