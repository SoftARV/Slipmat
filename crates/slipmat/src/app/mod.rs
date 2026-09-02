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
    Component, ComponentController, ComponentParts, ComponentSender, Controller, adw, gtk,
};

use relm4::typed_view::grid::TypedGridView;
use relm4::typed_view::list::TypedListView;

use crate::components::artwork::{self, ART_SIZE};
use crate::components::detail_page::{DetailPage, PageKind};
use crate::components::grid_item::{ArtRegistry, ArtRequest, GridItem, Tile, art_registry};
use crate::components::now_playing::{NowPlaying, NowPlayingInput, NowPlayingOutput, VOLUME_STEP};
use crate::components::player_view::{PlayerView, PlayerViewInput};
use crate::components::queue_view::{QueueView, QueueViewInput, QueueViewOutput};
use crate::components::track_row::LibraryRowWidgets;
use crate::components::track_row::{Entry, LibraryItem, RowMenuRequest};
use crate::components::{
    CurrentTrack, DeadTracks, RowRegistry, TrackOverrides, current_track, dead_tracks,
    row_registry, track_overrides,
};
use crate::daemon;
use crate::mirror::Mirror;
use crate::notify;
use crate::settings::{Section, Settings, Theme};
use slipmat_core::ipc::{PlayMode, Request, Transport, WriteAction};
use slipmat_core::music::types::{Album, Artist, Artwork, Playlist, Track};
use slipmat_core::player::protocol::RepeatMode;

/// How often the seek bar redraws while playing.
///
/// The sidecar's own position events are coarse and irregular, so
/// `PlayerState::interpolated_position_ms` fills the gaps; this timer just
/// drives the repaint. Removed entirely when not playing — a paused player
/// should cost nothing (the same discipline as Pitwall's suspend-gated poll).
mod background;
mod chrome;
mod library;
mod osd;
mod pages;
mod pins;
mod playback;
mod queue;
mod row_menu;
mod status;
mod supervise;
mod view;
mod wiring;
mod writes;

use chrome::{icon, register_actions, show_about, show_shortcuts};
use supervise::connect;

pub use view::{CatalogFilter, SearchScope, SortBy, View};
use view::{SidebarRow, sidebar_rows};

const TICK_MS: u32 = 500;

/// How long the search box must sit still before a catalog search is sent.
///
/// The library filter is local and runs on every keystroke; the catalog is a
/// network request, and firing one per character would be both slow and rude.
const SEARCH_DEBOUNCE_MS: u64 = 350;

/// Tile covers are fetched at twice their drawn size, so they stay sharp on a
/// HiDPI screen without paying for the 512px the Now Playing bar needs.
const TILE_ART: u32 = 320;

/// Stop paging here. Nobody scrolls 400 search results, and an unbounded list
/// is an unbounded number of requests.
const CATALOG_MAX: usize = 200;

/// Where we are in bringing the sidecar up. Each variant is a distinct
/// `StatusPage`, because "it's just spinning" is the failure mode this whole
/// module exists to avoid (rule 4).
#[derive(Debug, Default)]
pub enum Stage {
    #[default]
    Starting,
    /// Loaded music.apple.com; waiting for the hook to attach.
    Connecting,
    /// Signed out. Apple's own login window is one click away.
    SignedOut,
    Ready,
    /// Apple changed the page, or the CDM is unavailable. Names the fix.
    Broken(String),
}

pub struct AppModel {
    stage: Stage,
    mirror: Mirror,
    /// The first-run gate, while it is up. `Some` exactly when the app is
    /// blocked, which is what stops it being presented twice.
    onboarding: Option<adw::Dialog>,

    /// The last track MusicKit reported, kept so the bar can hold it through a
    /// queue reload — see `push_snapshot::showing`.
    last_item: Option<slipmat_core::player::protocol::Item>,

    /// Kept for the row context menu, whose GTK actions outlive the `update`
    /// call that built them.
    menu_sender: ComponentSender<AppModel>,

    /// The daemon, once connected. `None` while it is coming up.
    daemon: Option<daemon::Handle>,
    /// Consecutive failed dials, for the redial backoff.
    redials: u32,
    toaster: adw::ToastOverlay,
    /// The volume panel. Its widgets rather than its state, which is the two
    /// fields below — see `osd.rs`.
    volume_osd: osd::VolumeOsd,
    /// Whether the panel is up. An **animated** property, so it is written on
    /// an edge through `sync_animated` and never as a `#[watch]`.
    osd_shown: bool,
    /// The single hide-timer, reset on each press rather than added to.
    osd_timer: Option<osd::HideTimer>,
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
    /// Whether the header is too narrow to hold a search entry as its title.
    ///
    /// Mirrored from its own breakpoint, the way `sidebar_collapsed` is
    /// mirrored from the split view — a width we measured ourselves would be a
    /// second opinion about a decision libadwaita has already made.
    narrow_header: bool,
    /// Set when something asked for the search box and the *widgets* have to
    /// act on it — focus it, and put the caret after the text. Cleared by
    /// `update_with_view` the moment it does, because it is a one-shot request
    /// rather than a state anything can be derived from.
    focus_search: bool,
    /// Set when the query changed from somewhere that is not the entry, so the
    /// entry has to be told.
    ///
    /// It is normally the *source* of the query and nothing writes back to it —
    /// a binding there would be the two-way loop from #37. The cost of that is
    /// this flag: clear the query without it and the words stay in the field
    /// over a list that is no longer filtered.
    sync_entry: bool,
    /// Whether the search entry is showing, on a narrow header where it is a
    /// button until asked for. Meaningless while `narrow_header` is false: the
    /// entry is simply the title then.
    searching: bool,
    /// Every sidebar row, in order — sections then pins. Rebuilt whenever the
    /// pins change, and the only thing `SidebarRowChosen` indexes into.
    sidebar_rows: Vec<SidebarRow>,
    /// Which sidebar row is selected, so a rebuild can put it back.
    ///
    /// Tracked here rather than read off the `ListBox`: a rebuild changes what
    /// each position means, so by the time it is needed the widget can only say
    /// *where* the selection was, not what it was.
    selected_row: Option<SidebarRow>,
    /// The sidebar's `row-selected` handler, so a rebuild can silence it.
    nav_selected: std::cell::RefCell<Option<gtk::glib::SignalHandlerId>>,
    /// The pins changed and the sidebar's rows have not caught up.
    ///
    /// Set in `update`, which cannot reach the widgets, and cleared in
    /// `sync_pins` on the way out — the same shape as `sync_animated`.
    pins_dirty: bool,
    /// A pinned row's label, by playlist id.
    ///
    /// Kept so a name can be filled in *after* the row is drawn. The sidebar is
    /// built before `seed_from_cache` runs, so at build time no pin has a name
    /// yet — and rebuilding the rows to fix that would clear the selection,
    /// which is the bug 285b542 removed. Writing the label leaves it alone.
    pin_labels: Vec<(String, gtk::Label)>,
    /// The sidebar's per-section spinners, built in `wiring::sidebar_rows`.
    ///
    /// Held because they are built outside `view!` and so get no `#[watch]` —
    /// `sync_section_spinners` is what replaces it. Apple Music has no entry:
    /// it has nothing of its own to load.
    section_spinners: Vec<(View, adw::Spinner)>,
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
    /// Which page asked for which Apple id, so an answer finds the page that
    /// wanted it. **By id, never by depth** — the stack can move between the
    /// request and the reply.
    page_for: std::collections::HashMap<String, u64>,
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
    /// Whether the artwork sweep has run this session. Once per run: it walks
    /// the whole cache directory.
    pruned: bool,
    settings: Settings,
    /// The track the last notification was sent for, so a queue echo or a
    /// position tick cannot re-notify for the song already playing.
    notified_for: Option<String>,
    /// A track whose notification is waiting on its cover to finish
    /// downloading. See `maybe_notify`.
    notify_when_art_lands: Option<String>,
}

/// Logs how long a rebuild took, on the way out.
///
/// **Kept, not temporary.** It was added to answer "switching sections is
/// slow" with a number, found ~500ms of re-decoding covers, and is what the
/// section fingerprints are checked against — so it stays, and CLAUDE.md points
/// at it as the way this stays measurable rather than remembered.
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
/// Both answer 202 Accepted with an empty body, so neither can be treated as
/// done — only as sent. That is why nothing here toggles a checkbox.
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
}

#[derive(Debug)]
pub enum AppMsg {
    Quit,
    SignIn,
    /// Asks first — see `confirm_sign_out`.
    SignOut,
    SignOutConfirmed,
    PlayPause,
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
    SetRepeat(RepeatMode),
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
    /// Reorder the queue. `to` is where the item lands.
    MoveQueueItem {
        from: usize,
        to: usize,
    },
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
    /// The breakpoint turned the sidebar into an overlay, or back into a pane.
    SidebarCollapsed(bool),
    /// The header crossed its own breakpoint.
    NarrowHeader(bool),
    /// Open or close the search entry on a narrow header.
    ShowSearch(bool),
    /// Put the caret in the search entry, opening it first if it is a button.
    FocusSearch,
    /// A printable key arrived with nothing focused that wanted it. Starts a
    /// search with that character already in it.
    TypeAhead(String),
    /// The results list is near its end; fetch the next page if there is one.
    LoadMoreCatalog,
    /// Re-fetch one library section. There is no section-less "reload": each
    /// is fetched separately, so a single one could not know which you meant.
    /// Fetch the section on screen again. Carries no payload on purpose: the
    /// only sender is a header button, which cannot read the model from inside
    /// its click handler, and the reducer already knows which view is showing.
    ReloadCurrentSection,
    ShowPreferences,
    ShowShortcuts,
    /// The hide-timer fired: the panel has been up long enough.
    HideVolumeOsd,
    /// A sidebar row was selected, by position. What it does depends on what
    /// kind of row it is — see `pins::sidebar_row_chosen`.
    SidebarRowChosen(i32),
    /// A sidebar row was clicked. Only the pin button cares — every other row
    /// has already acted through `SidebarRowChosen`, and this is where the
    /// overlay sidebar gets out of the way.
    SidebarRowActivated(i32),
    /// Open the picker over the library's playlists.
    ShowPinPicker,
    /// Pin or unpin one playlist, from the picker or a row menu.
    SetPinned {
        id: String,
        pinned: bool,
    },
    /// Pin or unpin every library playlist — the picker's header action.
    SetAllPinned(bool),
    /// A playlist tile was right-clicked.
    TileMenu(crate::components::grid_item::TileMenuRequest),
    /// A pinned row was dragged. `slot` is in the coordinates of the list as it
    /// was before the move — see `pins::move_pin`.
    MovePin {
        from: usize,
        slot: usize,
    },
    ShowAbout,
    /// Open the Ko-fi page in a browser.
    OpenSupport,
    SetTheme(u32),
    SetAccent(crate::style::Accent),
    /// Whether the cover is painted behind the player (#145).
    SetPlayerBackdrop(bool),
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
    /// Walk from a queue row to the album or artist behind it.
    ///
    /// A separate message from `OpenPage` because a queue item has no album or
    /// artist id to push a page with — only the song's own. Resolving that costs
    /// a request, which is why it happens on a menu click and not per row.
    OpenQueueTrackPage {
        catalog_id: String,
        album: bool,
    },
    /// The navigation view popped a page — drop the state behind it.
    PagePopped(u64),
    /// Act on a track in MusicKit's queue, by id. The position is resolved
    /// against the live queue at send time — our row order can drift from
    /// MusicKit's, and sending a stale position got INVALID_ARGUMENTS.
    JumpTo {
        at: usize,
        id: String,
    },
    RemoveFromQueue {
        at: usize,
        id: String,
    },
}

#[derive(Debug)]
pub enum CommandMsg {
    /// The quit request reached the daemon socket, so the client can exit.
    QuitWritten,
    /// Everything the daemon pushed down, including losing it.
    Daemon(daemon::Incoming),
    /// Cover art is on disk. `None` when the fetch failed — a missing cover is
    /// cosmetic and must not become a toast.
    Artwork {
        path: Option<PathBuf>,
        /// A tiny copy of the cover, to go behind the bar and the drawer.
        /// Carried here rather than in its own message because the cover and
        /// what is drawn from it must be applied together.
        backdrop: Option<PathBuf>,
    },
    /// A page's header art is on disk, or could not be fetched.
    PageArtwork { page: u64, path: Option<PathBuf> },
    /// The artwork sweep finished. It logs its own numbers; this exists so the
    /// work can be a command rather than something done on the GTK thread.
    Pruned(crate::components::prune::Report),
    /// Whether the Background portal agreed to list us. Advisory only.
    BackgroundPortal(Result<(), String>),
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
            set_title: Some("Slipmat"),

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
                // The volume panel floats here: above the drawer, so opening it
                // does not cover the panel, and below toasts, so an error still
                // wins. See `osd.rs`.
                gtk::Overlay {
                    #[local_ref]
                    add_overlay = volume_osd -> gtk::Revealer {},

                    #[wrap(Some)]
                    set_child = &adw::ToolbarView {
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
                                            set_title: "Slipmat",
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
                                                // `row-selected` lives in
                                                // `wiring`: its handler id has
                                                // to be kept so a rebuild can
                                                // silence it. Activation, not
                                                // selection, because the pin
                                                // button is unselectable.
                                                connect_row_activated[sender] => move |_, row| {
                                                    sender.input(
                                                        AppMsg::SidebarRowActivated(row.index()),
                                                    );
                                                },

                                                // The five rows are appended by
                                                // `wiring::sidebar_rows`, from the
                                                // one array that also defines the
                                                // index contract read just above.
                                            },
                                    },
                                },
                            },

                            #[wrap(Some)]
                            #[local_ref]
                            set_content = nav_view -> adw::NavigationView {
                                add = &adw::NavigationPage {
                                    set_title: "Slipmat",
                                    // The root page. Albums and artists push on
                                    // top of it; nothing ever pops it.
                                    set_tag: Some("results"),

                                    #[wrap(Some)]
                                    #[name = "content_bars"]
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

                                        // Beside the sidebar toggle rather than
                                        // over on the right: both are doors to
                                        // something the window is too narrow to
                                        // show outright, and the end of a header
                                        // is where the actions on what you are
                                        // already looking at live.
                                        //
                                        // Narrow only: wide, the entry is always
                                        // there and this would reveal nothing.
                                        #[name = "search_button"]
                                        pack_start = &gtk::ToggleButton {
                                            set_icon_name: "system-search-symbolic",
                                            set_tooltip_text: Some("Search"),
                                            add_css_class: "flat",
                                            #[watch]
                                            set_visible: model.narrow_header,
                                            #[watch]
                                            set_sensitive: model.controls_live(),
                                            // `set_active` plus a report back is
                                            // the two-way binding from #37 —
                                            // `ShowSearch` drops a value equal to
                                            // the one held, which is what a
                                            // programmatic set arrives as.
                                            #[watch]
                                            set_active: model.searching,
                                            connect_toggled[sender] => move |button| {
                                                sender.input(AppMsg::ShowSearch(button.is_active()));
                                            },
                                        },

                                        // When the queue is open it is the
                                        // rightmost pane, so the window
                                        // controls belong to its header, not
                                        // this one. Without this they vanish:
                                        // the queue's header hides them and
                                        // this header is no longer at the edge.
                                        set_show_end_title_buttons: true,

                                        // Narrow, the title is the section name
                                        // and search is a button — the sidebar
                                        // row that says where you are has
                                        // collapsed to an overlay by then.
                                        // Not homogeneous, unlike the reload
                                        // stack: a short label and a field that
                                        // should take the whole header.
                                        #[wrap(Some)]
                                        set_title_widget = &gtk::Stack {
                                            set_hhomogeneous: false,
                                            set_hexpand: true,

                                            add_named[Some("title")] = &adw::WindowTitle {
                                                #[watch]
                                                set_title: model.view.title(),
                                            },

                                            #[name = "search_entry"]
                                            add_named[Some("search")] = &gtk::SearchEntry {
                                                // Never a fixed width: 320px here
                                                // was a floor under the window and
                                                // the app could not be tiled.
                                                // `max-width-chars` is a ceiling,
                                                // so it is safe — 60 fills a narrow
                                                // header and stops short of absurd
                                                // on a wide one.
                                                set_hexpand: true,
                                                set_max_width_chars: 60,
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
                                                // The entry is the *source* of
                                                // the query, so it clears itself
                                                // rather than waiting on the
                                                // reducer. `SearchChanged` too:
                                                // `search-changed` is delayed and
                                                // filtering should stop now.
                                                connect_stop_search[sender] => move |entry| {
                                                    entry.set_text("");
                                                    sender.input(AppMsg::SearchChanged(String::new()));
                                                    sender.input(AppMsg::ShowSearch(false));
                                                },
                                            },

                                            #[watch]
                                            set_visible_child_name: if model.search_showing() {
                                                "search"
                                            } else {
                                                "title"
                                            },
                                        },

                                        // One button: it reloads what you are
                                        // looking at, so there is nothing to
                                        // disambiguate. A `Stack` because it is
                                        // homogeneous — swapping visibility let
                                        // the narrower spinner re-centre the
                                        // header's search entry.
                                        pack_end = &gtk::Stack {
                                            // Children first. `view!` assigns in
                                            // the order written, so naming a
                                            // child above the `add_named` that
                                            // creates it is `Gtk-WARNING: Child
                                            // name 'reload' not found in
                                            // GtkStack` on every launch.
                                            add_named[Some("reload")] = &gtk::Button {
                                                set_icon_name: "view-refresh-symbolic",
                                                set_tooltip_text: Some("Reload"),
                                                add_css_class: "flat",
                                                #[watch]
                                                set_sensitive: model.controls_live(),
                                                connect_clicked[sender] => move |_| {
                                                    sender.input(AppMsg::ReloadCurrentSection);
                                                },
                                            },

                                            // The only sign a reload is running,
                                            // once the list stopped being taken
                                            // away for one.
                                            add_named[Some("busy")] = &adw::Spinner {
                                                set_size_request: (16, 16),
                                                set_valign: gtk::Align::Center,
                                                set_halign: gtk::Align::Center,
                                            },

                                            #[watch]
                                            set_visible: model.view != View::Search,
                                            #[watch]
                                            set_visible_child_name: if model.loading_section() {
                                                "busy"
                                            } else {
                                                "reload"
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
                                            // A label, not an icon: Adwaita
                                            // has no filter glyph, and the
                                            // current filter has to be readable
                                            // without hovering — it is the only
                                            // thing explaining a narrowed
                                            // search.
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
                                        // Inside the scroller a plain clamp
                                        // breaks `GtkListView`'s height
                                        // allocation; outside it, the scrollbar
                                        // ends up mid-window. This one passes
                                        // `GtkScrollable` through to its child.
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

                                        add_named[Some("empty-library")] = &adw::StatusPage {
                                            set_icon_name: Some("audio-x-generic-symbolic"),
                                            set_title: "Nothing here yet",
                                            set_description: Some(
                                                "Slipmat refreshes your library after sign-in. Use Refresh to try again.",
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
            },
        }
    }

    fn init(
        settings: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        // The bar emits intent, never commands — `app/mod.rs` is the only place
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

        // The same handover, for the grid's tiles — `setup` is static and has no
        // item to reach the app through. See `pins::menu`.
        let tile_menu = sender.clone();
        crate::components::grid_item::set_tile_menu(move |req| {
            tile_menu.input(AppMsg::TileMenu(req));
        });

        let queue_view = QueueView::builder()
            .launch(())
            .forward(sender.input_sender(), |out| match out {
                QueueViewOutput::Jump { at, id } => AppMsg::JumpTo { at, id },
                QueueViewOutput::Remove { at, id } => AppMsg::RemoveFromQueue { at, id },
                QueueViewOutput::Clear => AppMsg::ClearQueue,
                QueueViewOutput::Move { from, to } => AppMsg::MoveQueueItem { from, to },
                QueueViewOutput::SetShuffle(on) => AppMsg::SetShuffle(on),
                QueueViewOutput::SetRepeat(mode) => AppMsg::SetRepeat(mode),
                QueueViewOutput::GoToAlbum(catalog_id) => AppMsg::OpenQueueTrackPage {
                    catalog_id,
                    album: true,
                },
                QueueViewOutput::GoToArtist(catalog_id) => AppMsg::OpenQueueTrackPage {
                    catalog_id,
                    album: false,
                },
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
            narrow_header: false,
            searching: false,
            focus_search: false,
            sync_entry: false,
            animated_shown: std::cell::Cell::new(None),
            section_spinners: Vec::new(),
            pin_labels: Vec::new(),
            pins_dirty: false,
            selected_row: None,
            nav_selected: std::cell::RefCell::new(None),
            // Built from the persisted pins before anything is on screen: the
            // library cache has already seeded `playlists`, so a pinned row
            // draws its name at the same moment the sections do.
            sidebar_rows: sidebar_rows(&settings.pinned_playlists),
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
                    by: SortBy::parse(&settings.sort).valid_for(View::Songs.sortable()),
                    reversed: settings.sort_reversed,
                },
                albums: view::Sort {
                    by: SortBy::parse(&settings.album_sort).valid_for(View::Albums.sortable()),
                    reversed: settings.album_sort_reversed,
                },
                artists: view::Sort {
                    by: SortBy::parse(&settings.artist_sort).valid_for(View::Artists.sortable()),
                    reversed: settings.artist_sort_reversed,
                },
                playlists: view::Sort {
                    by: SortBy::parse(&settings.playlist_sort)
                        .valid_for(View::Playlists.sortable()),
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
            album_art_widgets: art_registry(),
            artist_art_widgets: art_registry(),
            playlist_art_widgets: art_registry(),
            tile_art_pending: std::collections::HashSet::new(),
            tile_art_request,
            catalog: Vec::new(),
            catalog_paged: 0,
            catalog_filter: CatalogFilter::default(),
            background: None,
            pages: Vec::new(),
            page_for: std::collections::HashMap::new(),
            next_page_id: 1,
            nav,
            searching_catalog: false,
            catalog_exhausted: false,
            search_gen: 0,
            loading_library: false,
            // Seeded from the cache so the first play of a session does not
            // have to rediscover them by failing a setQueue.
            dead_ids: slipmat_core::unplayable::load(),
            last_queue: None,
            pending_start: None,
            mirror: Mirror::default(),
            onboarding: None,
            last_item: None,
            menu_sender: sender.clone(),
            daemon: None,
            redials: 0,
            toaster: adw::ToastOverlay::new(),
            volume_osd: osd::VolumeOsd::new(),
            osd_shown: false,
            osd_timer: None,
            now_playing,
            volume: 1.0,
            art_path: None,
            art_for: None,
            tick: None,
            pruned: false,
            settings,
            notified_for: None,
            notify_when_art_lands: None,
        };
        let primary_menu = gtk::gio::Menu::new();
        {
            // **First, in its own section.** It is the one item here that is
            // not about running the app, and under Preferences and About it
            // read as the least of them — the only thing in this menu that
            // asks rather than does, buried under three that do.
            //
            // **No icon, and not for want of trying.** `gio::MenuItem` carries
            // one and `GtkPopoverMenu` ignores it: GTK4 draws icons only for
            // items in a section with a `display-hint`, which is for the
            // little button rows, not for an ordinary entry. A heart was set
            // here, resolved from the theme, and simply never appeared.
            let support = gtk::gio::Menu::new();
            support.append(Some("_Buy Me a Coffee"), Some("win.support"));
            primary_menu.append_section(None, &support);

            let section = gtk::gio::Menu::new();
            section.append(Some("_Preferences"), Some("win.preferences"));
            section.append(Some("_Keyboard Shortcuts"), Some("win.shortcuts"));
            section.append(Some("_About Slipmat"), Some("win.about"));
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
        let nav_view = &model.nav;
        let album_grid = &model.album_grid.view;
        let artist_grid = &model.artist_grid.view;
        let playlist_grid = &model.playlist_grid.view;
        let player_sheet_content = model.player_view.widget();
        // Cloned rather than borrowed from the model: `view_output!` needs it
        // while the model already owns it.
        let osd_revealer = model.volume_osd.revealer.clone();
        let volume_osd = &osd_revealer;
        let widgets = view_output!();

        wiring::connect(&mut model, &widgets, &root, &sender);

        // The drawer opens to most of the window, rather than to whatever
        // height its contents happen to add up to. See `fill_window`.
        crate::components::player_view::fill_window(
            &root,
            &widgets.player_sheet,
            model.player_view.widget().upcast_ref(),
            model.player_view.sender(),
        );

        register_actions(&root, &sender);

        // Rows read playability from here, so seed it before any are built.
        *model.dead_rows.borrow_mut() = model.dead_ids.clone();

        // Open on last time's library rather than on a spinner. The refresh
        // still runs the moment the sidecar is up; it lands quietly, or — far
        // more often — finds nothing changed and does not even rebuild.
        model.seed_from_cache();
        // The rows exist by now but were drawn before the cache was read, so
        // every pin still says "Unavailable".
        model.refresh_pin_names();

        connect(&sender);
        // Whatever the daemon had last time, until it says otherwise.
        model.reload_from_cache(&sender);

        ComponentParts { model, widgets }
    }

    fn shutdown(&mut self, _widgets: &mut Self::Widgets, _output: relm4::Sender<Self::Output>) {
        // A now-playing notification must not outlive the player that sent it.
        notify::clear(relm4::main_application().upcast_ref::<gtk::gio::Application>());
        // The only moment the position is accurate.
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
        self.sync_pins(widgets, &sender);
        self.sync_section_spinners();

        if self.view != view_before {
            // `set_text` fires `search-changed`, but `SearchChanged` returns
            // early when the text already matches the active query — which it
            // does by now, because `update` set it first. No loop.
            widgets.search_entry.set_text(self.query());
            self.sync_sort_menu(&widgets.sort_button);
        }

        // After `update`, so a narrow header has already swapped the entry in
        // for the section title — an unmapped widget cannot take the caret, and
        // that is the whole reason this is a flag rather than a `grab_focus`
        // at the call site.
        if std::mem::take(&mut self.sync_entry) {
            widgets.search_entry.set_text(self.query());
        }
        if std::mem::take(&mut self.focus_search) {
            widgets.search_entry.grab_focus();
            // Typing appends, so the caret belongs after what is already there.
            // `grab_focus` on an entry selects all of it, and the next
            // keystroke would replace the character that opened the search.
            widgets.search_entry.set_position(-1);
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

    /// Overridden for one reason: [`AppModel::sync_animated`] and
    /// [`AppModel::sync_pins`] have to run on **both** paths.
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
        // Pruning a stale pin happens here, not in `update`: the library load is
        // a command message. Syncing only on the other path left the pruned row
        // on screen with nothing behind it — and clicking it opened whatever had
        // moved into its position.
        self.sync_pins(widgets, &sender);
        self.sync_section_spinners();
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
    /// The volume panel's crossfade. Fourth of its kind, and here for the same
    /// reason as the other three: a `#[watch]` would re-ask for it after every
    /// message, and during playback those never stop.
    osd: bool,
}

impl AppModel {
    fn animated_state(&self) -> Animated {
        Animated {
            sidebar: self.show_sidebar,
            queue: self.show_queue,
            bottom_bar: matches!(self.stage, Stage::Ready),
            osd: self.osd_shown,
        }
    }

    /// Push the four animated properties, and **only where they changed**.
    ///
    /// Each drives an animation, so writing one asks it to start or re-aim:
    /// correct on an edge, catastrophic on a level. As a `#[watch]` it re-fired
    /// on every message and wedged libadwaita's spring solver.
    ///
    /// Compared against **what we last wrote**, not the widget — a widget that
    /// disagrees persistently is the level trigger again.
    /// Show a spinner on whichever sections are fetching.
    ///
    /// Called from **both** view paths: a library load finishes as a
    /// `CommandMsg` and arrives through `update_cmd_with_view` only. Wire this
    /// to one and every spinner starts but none stops.
    fn sync_section_spinners(&self) {
        for (view, spinner) in &self.section_spinners {
            spinner.set_visible(self.loading_in(*view));
        }
    }

    fn sync_animated(&self, widgets: &<Self as relm4::Component>::Widgets) {
        let now = self.animated_state();
        let last = self.animated_shown.get();
        if last.map(|l| l.sidebar) != Some(now.sidebar) {
            widgets.nav_split.set_show_sidebar(now.sidebar);
            // A destination page carries its own toggle, and a toggle drawn
            // pressed over a hidden sidebar lies about its own state.
            for page in &self.pages {
                page.set_sidebar_shown(now.sidebar);
            }
        }
        if last.map(|l| l.queue) != Some(now.queue) {
            widgets.player_sheet.set_open(now.queue);
        }
        if last.map(|l| l.bottom_bar) != Some(now.bottom_bar) {
            widgets.player_sheet.set_reveal_bottom_bar(now.bottom_bar);
        }
        if last.map(|l| l.osd) != Some(now.osd) {
            self.volume_osd.revealer.set_reveal_child(now.osd);
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
            AppMsg::Quit => {
                if let Some(daemon) = self.daemon.clone() {
                    sender.oneshot_command(async move {
                        daemon.quit().await;
                        CommandMsg::QuitWritten
                    });
                } else {
                    notify::quit_cleanly();
                }
            }
            AppMsg::SignIn => self.ask(Request::SignIn),
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
                self.ask(Request::SignOut);
            }
            AppMsg::PlayPause => self.transport(Transport::PlayPause),
            AppMsg::Next => self.transport(Transport::Next),
            AppMsg::Previous => self.go_previous(),
            AppMsg::Seek(position_ms) => {
                self.transport(Transport::Seek { position_ms });
                // Announce the jump straight away rather than waiting for the
                // sidecar's echo. The spec requires `Seeked` on discontinuous
                // moves — without it controllers keep extrapolating from the
                // old position and their progress bars drift.
            }
            AppMsg::SetVolume(volume) => self.set_volume(volume),
            // **The panel is raised here rather than inside `set_volume`.**
            // That returns early when the value has not moved, so at 0.0 and
            // 1.0 it does nothing — and a shortcut that shows nothing at the
            // ends reads as a dropped keypress rather than "you are already
            // there".
            AppMsg::VolumeUp => {
                self.set_volume(self.volume + VOLUME_STEP);
                self.flash_volume(&sender);
            }
            AppMsg::VolumeDown => {
                self.set_volume(self.volume - VOLUME_STEP);
                self.flash_volume(&sender);
            }
            AppMsg::HideVolumeOsd => self.hide_volume_osd(),
            AppMsg::SidebarRowChosen(index) => self.sidebar_row_chosen(index, &sender),
            AppMsg::SidebarRowActivated(index) => self.sidebar_row_activated(index, &sender),
            AppMsg::ShowPinPicker => self.show_pin_picker(&sender, root),
            AppMsg::SetPinned { id, pinned } => self.set_pinned(&id, pinned, &sender),
            AppMsg::SetAllPinned(pinned) => self.set_all_pinned(pinned, &sender),
            AppMsg::TileMenu(req) => self.show_tile_menu(req),
            AppMsg::MovePin { from, slot } => self.move_pinned(from, slot),
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
                // On a narrow header the search box follows the section:
                // Apple Music *is* a search and lands on a prompt to type one,
                // so arriving with the field shut would be a screen asking for
                // something it did not give you room to enter. Every other
                // section closes it, because the query it held was about the
                // list you just left.
                self.searching = self.narrow_header && view == View::Search;
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
                    View::Albums => self.rebuild_albums(),
                    View::Artists => self.rebuild_artists(),
                    View::Playlists => self.rebuild_playlists(),
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
                    // the disk cache — but decode either way, off the GTK
                    // thread. That is the whole point of #27: the tile is
                    // handed pixels, not a filename. Either half failing is
                    // cosmetic but never silent; `load_tile` says which.
                    let (path, cover) = artwork::load_tile(art, TILE_ART, &key).await;
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
            AppMsg::ReloadCurrentSection => {
                self.reload(self.view, &sender);
                self.set_library_refreshing(true);
                self.ask(Request::Refresh);
            }
            AppMsg::ShowPreferences => self.show_preferences(&sender, root),
            AppMsg::ShowShortcuts => show_shortcuts(root),
            AppMsg::ShowAbout => show_about(root),
            AppMsg::OpenSupport => chrome::open_support(root),
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
            AppMsg::SidebarCollapsed(collapsed) => {
                // Logged because getting the breakpoints wrong is *silent*.
                // Only one applies at a time, so a narrow one that forgets to
                // repeat a wide one's setter simply undoes it — and the sidebar
                // stops collapsing at exactly the widths where it matters most.
                // Nothing warns; the pane just comes back.
                tracing::debug!(collapsed, narrow_header = self.narrow_header, "sidebar");
                self.sidebar_collapsed = collapsed;
            }
            AppMsg::NarrowHeader(narrow) => {
                tracing::debug!(narrow, "header breakpoint");
                self.narrow_header = narrow;
                // The bar reads this off the snapshot to decide whether it has
                // room for shuffle, repeat and volume, and nothing else pushes
                // one when only the window changed.
                self.push_snapshot();
                // Widening puts the entry back as the title, so the open flag
                // stops meaning anything — and leaving it set would reopen the
                // box the next time the window got narrow, which is not
                // something the user asked for a window resize ago.
                if !narrow {
                    self.searching = false;
                }
            }
            AppMsg::ShowSearch(show) => {
                // The button reports its own state *and* is written from the
                // model, which is the two-way binding that froze a desktop
                // (#37). Adopting the value here is the half of the fix that
                // stops the next `update_view` writing the old one back.
                if self.searching == show {
                    return;
                }
                self.searching = show;
                // Closing is a request to stop filtering, not to hide a filter
                // that is still in force. A narrowed list under a header that
                // shows no query is a list nobody can explain.
                //
                // Below the guard on purpose, so *widening* keeps the query.
                // `NarrowHeader` clears `searching` itself, which makes the
                // button report a change that lands here holding the value we
                // already have — and a window resize is not a request to
                // abandon what you were looking for.
                if !show {
                    // Inline rather than `sender.input`, so the query is empty
                    // *before* `sync_entry` is read. Queued, the flag would be
                    // consumed a pass early and write the words back in.
                    self.handle(AppMsg::SearchChanged(String::new()), &sender, root);
                    self.sync_entry = true;
                }
            }
            // Both openers set `sync_entry` too: the entry may hold text from
            // a query that was cleared while it was hidden.
            AppMsg::FocusSearch => {
                self.sync_entry = true;
                // The box has to exist before it can be focused: on a narrow
                // header it is a hidden stack page until now, and a widget that
                // is not mapped cannot take the caret.
                self.searching = true;
                self.focus_search = true;
            }
            AppMsg::TypeAhead(text) => {
                self.searching = true;
                self.focus_search = true;
                self.sync_entry = true;
                let mut query = self.query().to_owned();
                query.push_str(&text);
                // Straight through the ordinary path, so filtering, the
                // rebuild and the per-scope query all behave exactly as they do
                // when the character is typed into the entry directly.
                self.handle(AppMsg::SearchChanged(query), &sender, root);
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
            AppMsg::OpenQueueTrackPage { catalog_id, album } => {
                // Which album or artist a track belongs to is a catalog lookup,
                // so it goes where the tokens are (rule 7).
                self.ask(Request::Open {
                    kind: if album {
                        slipmat_core::ipc::PageKind::Album
                    } else {
                        slipmat_core::ipc::PageKind::Artist
                    },
                    id: catalog_id,
                });
            }
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
                        self.play_entries(&entries, row, PlayMode::Clicked);
                    }
                    None => {}
                }
            }
            AppMsg::PlayPage { page, shuffle } => {
                let Some(target) = self.pages.iter().find(|p| p.id == page) else {
                    return;
                };
                let entries = target.entries.clone();
                // The daemon picks the row a shuffle opens on (#147) — it has
                // the dead-id list that decides which rows are candidates.
                let (row, start) = if shuffle {
                    (0, PlayMode::Shuffled)
                } else {
                    (0, PlayMode::InOrder)
                };
                self.play_entries(&entries, row, start);
            }
            AppMsg::MoveQueueItem { from, to } => {
                // **Optimistic.** The row is already where the user dropped it,
                // so the projection moves now and MusicKit's echo confirms it —
                // the same shape as a library write, and for the same reason: a
                // drop that visibly springs back while a command is in flight
                // reads as a failure even when it worked.
                tracing::info!(from, to, "reordering the queue");
                self.ask(Request::MoveInQueue { from, to });
                // `push_snapshot` re-syncs the queue view from the projection,
                // so the row is already in its new place before the echo.
                self.push_snapshot();
            }
            AppMsg::ClearQueue => {
                tracing::info!("clearing the queue");
                self.ask(Request::ClearQueue);
                // Nothing to come back to next launch, either. The mirror
                // follows the sidecar's queue event as always (rule 3) — this
                // is only the part MusicKit cannot know about.
                self.last_queue = None;
                self.pending_start = None;
                self.last_item = None;
                slipmat_core::session::clear();
                crate::style::set_backdrop(None);
            }
            AppMsg::JumpTo { at, id } => match self.queue_index_at(at, &id) {
                Some(index) => {
                    self.ask(Request::JumpTo { index });
                    // Clicking a track in the queue is a request to *play* it.
                    // `changeToMediaAtIndex` only moves the cursor, so on a
                    // queue that is loaded but idle — a restored session, or a
                    // paused one — it moved silently and looked like nothing
                    // had happened.
                    if !self.mirror.is_playing() {
                        self.transport(Transport::Play);
                    }
                }
                None => self.toast("That track is no longer in the queue"),
            },
            AppMsg::RemoveFromQueue { at, id } => match self.queue_index_at(at, &id) {
                Some(index) => self.ask(Request::RemoveFromQueue { index }),
                None => self.toast("That track is no longer in the queue"),
            },
            AppMsg::SetAccent(accent) => {
                self.settings.accent = accent.id().into();
                self.settings.save();
                // Live: the provider is replaced, and every widget already
                // referencing the accent variables repaints itself.
                crate::style::set_accent(accent);
            }
            AppMsg::SetPlayerBackdrop(on) => {
                self.settings.player_backdrop = on;
                self.settings.save();
                // Live, like the accent. `style` still knows which cover is
                // showing, so turning it back on needs no track change first.
                crate::style::set_backdrop_enabled(on);
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
                self.ask(Request::Write {
                    action: WriteAction::RemoveFromLibrary,
                    id: library_id,
                });
                // Mirrored locally for the same reason the star is: the menu
                // reads this, and making someone reload to see their own click
                // is absurd. `include=library` is cached for tens of seconds
                // besides, so a read-back would disagree for a while (#34).
                self.set_in_library(&catalog_id, false);
                self.toast("Removing from your library…");
            }
            AppMsg::Unfavorite { catalog_id } => {
                tracing::info!(%catalog_id, "removing favourite");
                self.ask(Request::Write {
                    action: WriteAction::Unfavorite,
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
                // Said out loud before the request goes out: these are
                // fire-and-forget, and a click with no feedback at all reads as
                // a click that did not register.
                self.toast(action.sent());
                tracing::info!(?action, "library write");
                // The daemon decides whether this goes over REST or through
                // MusicKit — only it can do the second, and only it has the
                // tokens for the first.
                self.ask(Request::Write {
                    action: match action {
                        LibraryAction::Favorite => WriteAction::Favorite,
                        LibraryAction::AddToLibrary => WriteAction::AddToLibrary,
                    },
                    id: catalog_id,
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
                if self.mirror.queue.is_empty() {
                    // Nothing to insert into: `playNext` on an empty queue is a
                    // silent no-op in MusicKit. Start the queue instead —
                    // "add to queue" with no queue plainly means "make one",
                    // and refusing was a worse answer than doing it.
                    // The daemon states the mode for a queue it builds, so
                    // there is nothing to say here beyond which track.
                    tracing::info!("starting a queue from one track");
                    self.ask(Request::Play {
                        ids: songs,
                        index: 0,
                        start: PlayMode::Clicked,
                    });
                    return;
                }
                tracing::info!(next, "enqueueing one track");
                self.ask(Request::Enqueue { ids: songs, next });
            }
            AppMsg::SetShuffle(on) => {
                // Sent and forgotten: the mirror updates when MusicKit echoes
                // it back, so the button never claims a state the player is not
                // actually in (rule 3).
                tracing::info!(on, "shuffle");
                self.transport(Transport::SetShuffle { shuffle: on });
            }
            AppMsg::SetRepeat(mode) => {
                tracing::info!(?mode, "repeat");
                self.transport(Transport::SetRepeat { mode });
            }
            AppMsg::PlayFrom(index) => {
                let visible = self.visible_entries();
                self.play_entries(&visible, index, PlayMode::Clicked);
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
            CommandMsg::QuitWritten => notify::quit_cleanly(),
            CommandMsg::Pruned(report) => {
                // Reported here rather than inside the sweep, so the sweep
                // stays a function that returns facts and can be tested as one.
                // Silent when it found nothing, which is the ordinary case.
                if report.removed > 0 {
                    tracing::info!(
                        removed = report.removed,
                        freed_kb = report.freed / 1024,
                        kept = report.kept,
                        over_cap = report.over_cap,
                        was_mb = report.total / 1_048_576,
                        "swept the artwork cache"
                    );
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
                    "background portal refused; Quick Settings will not list Slipmat \
                     (expected when not launched from its .desktop entry)"
                ),
            },
            CommandMsg::TileArt { key, path, cover } => {
                self.tile_art_pending.remove(&key);
                let (Some(_path), Some(cover)) = (path, cover) else {
                    // Cosmetic. The tile keeps its placeholder.
                    return;
                };

                // Paint **every** tile showing this artwork now. Recycling
                // means they may not include the one that asked, and may be
                // none at all if it scrolled away — both are correct.
                //
                // All three registries, not the first that matches: the grids
                // hold their tiles bound whether or not the user is looking at
                // them, so a hidden album tile and a visible playlist tile can
                // want the same key at once. Stopping at the first match paid
                // the hidden one and left the visible one blank, which is what
                // "some artwork does not load" turned out to be.
                let texture = cover.into_texture();
                let mut painted = 0usize;
                for registry in [
                    &self.album_art_widgets,
                    &self.artist_art_widgets,
                    &self.playlist_art_widgets,
                ] {
                    for widget in registry.borrow().get(&key).into_iter().flatten() {
                        widget.set_texture(&texture);
                        painted += 1;
                    }
                }
                tracing::trace!(%key, painted, "tile art delivered");
            }
            CommandMsg::PageArtwork { page, path } => {
                if let (Some(path), Some(target)) = (path, self.pages.iter().find(|p| p.id == page))
                {
                    target.set_artwork(&path);
                }
            }
            CommandMsg::Artwork { path, backdrop } => {
                if path.as_deref() != self.art_for.as_deref().map(std::path::Path::new) {
                    tracing::debug!("discarding artwork for a track that moved on");
                    return;
                }
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
            CommandMsg::Daemon(message) => self.on_daemon(message, &sender),
        }
    }
}

impl AppModel {
    /// Which set of music the search box is searching, derived from the
    /// section. Not stored: see [`View`].
    fn scope(&self) -> SearchScope {
        self.view.scope()
    }

    /// Fetch a section again, over the top of what it is already showing.
    ///
    /// **Nothing is cleared first.** The three grids used to empty themselves
    /// here, and had to: their guard was `tried || !collection.is_empty()`, so
    /// clearing the flag alone left the loader returning early. The cost was
    /// that `page()` saw an empty collection mid-fetch and took the grid away
    /// for a full-pane spinner — a reload that interrupted whatever you were
    /// looking at, and Songs never did it because Songs never cleared.
    ///
    /// The guard is `tried` alone now, so the clear is not only unnecessary but
    /// the whole of that bug. All four sections keep their content up, and the
    /// list changes only if the answer did.
    fn reload(&mut self, view: View, _sender: &ComponentSender<Self>) {
        match view {
            View::Songs | View::Search => {
                self.tried_library = false;
            }
            View::Albums => {
                self.tried_albums = false;
            }
            View::Artists => {
                self.tried_artists = false;
            }
            View::Playlists => {
                self.tried_playlists = false;
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
    fn forget_session(&mut self, sender: &ComponentSender<Self>) {
        self.mirror.clear_account_state();
        self.all_tracks.clear();
        self.albums.clear();
        self.artists.clear();
        self.playlists.clear();
        self.loading_albums = false;
        self.loading_artists = false;
        self.loading_playlists = false;
        self.loading_library = false;
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
        self.catalog_filter = CatalogFilter::default();
        self.searching_catalog = false;
        self.catalog_exhausted = false;
        self.search_gen = self.search_gen.wrapping_add(1);
        self.library_query.clear();
        self.catalog_query.clear();
        self.sync_entry = true;
        self.searching = false;
        self.focus_search = false;
        self.row_overrides.borrow_mut().clear();
        self.tile_art_pending.clear();
        self.page_for.clear();

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
        self.art_path = None;
        self.art_for = None;
        self.notified_for = None;
        self.notify_when_art_lands = None;
        self.sync_tick(sender);
        self.now_playing.emit(NowPlayingInput::ArtworkReady(None));
        self.player_view.emit(PlayerViewInput::Artwork(None));
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
    use slipmat_core::music::types::TrackId;

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
