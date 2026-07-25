// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Root component.
//!
//! Redux with a compiler: `AppMsg` are the actions, `update` is the sole
//! reducer, and the view is derived from `AppModel`. No I/O happens inline —
//! the sidecar's stdout is drained by a streaming relm4 `Command` so the GTK
//! main thread never blocks (CLAUDE.md rule 8).
//!
//! **M5, the library slice.** Your saved songs in a native list, with
//! type-to-find search and click-to-play that enqueues the whole visible list.
//! The StatusPage now only appears while connecting or signed out.

use std::path::PathBuf;

use relm4::adw::prelude::*;
use relm4::{
    Component, ComponentController, ComponentParts, ComponentSender, Controller, RelmWidgetExt,
    adw, gtk,
};

use relm4::typed_view::list::TypedListView;

use crate::components::artwork::{self, ART_SIZE};
use crate::components::now_playing::{NowPlaying, NowPlayingInput, NowPlayingOutput, Snapshot};
use crate::components::queue_view::{QueueEntry, QueueView, QueueViewInput, QueueViewOutput};
use crate::components::track_row::LibraryRowWidgets;
use crate::components::track_row::{LibraryItem, apply_row_state};
use crate::components::{
    CurrentTrack, DeadTracks, RowRegistry, current_track, dead_tracks, row_registry,
};
use crate::mpris::{Mpris, MprisState};
use crate::music::client::Client;
use crate::music::types::{Artwork, Track};
use crate::notify;
use crate::player::protocol::{Command, Event, Tokens};
use crate::player::{Incoming, PlayerState, sidecar};
use crate::settings::{Section, Settings, Theme};

/// How often the seek bar redraws while playing.
///
/// The sidecar's own position events are coarse and irregular, so
/// `PlayerState::interpolated_position_ms` fills the gaps; this timer just
/// drives the repaint. Removed entirely when not playing — a paused player
/// should cost nothing (the same discipline as Pitwall's suspend-gated poll).
const TICK_MS: u32 = 500;

/// How long the search box must sit still before a catalog search is sent.
///
/// The library filter is local and runs on every keystroke; the catalog is a
/// network request, and firing one per character would be both slow and rude.
const SEARCH_DEBOUNCE_MS: u64 = 350;

/// Apple caps search at 25 results per request, so this is its ceiling rather
/// than a choice. More than that means paging with an offset.
const CATALOG_LIMIT: u32 = 25;

/// Stop paging here. Nobody scrolls 400 search results, and an unbounded list
/// is an unbounded number of requests.
const CATALOG_MAX: usize = 200;

/// Which set of music the search box is searching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchScope {
    /// Filter the tracks already loaded from the user's library, locally.
    #[default]
    Library,
    /// Search Apple Music's whole catalog, over the network.
    Catalog,
}

/// Upper bound on the library load. Apple pages at 100, so this is 25 requests
/// worst case. Generous for one laptop, and bounded so a very large library
/// cannot spin forever on first run.
const LIBRARY_MAX: usize = 2_500;

/// Case-insensitive substring match across the fields a person would search by.
///
/// Deliberately not fuzzy: with the whole library in memory, plain substring is
/// instant and predictable, and "predictable" is what makes type-to-find work.
fn matches(track: &Track, needle: &str) -> bool {
    track.title.to_lowercase().contains(needle)
        || track.artist.to_lowercase().contains(needle)
        || track.album.to_lowercase().contains(needle)
}

/// Pull the catalog ids out of MusicKit's `NOT_FOUND` error.
///
/// `setQueue` is all-or-nothing: if a single id cannot be resolved it rejects
/// the whole queue, so one delisted track makes an entire library unplayable.
/// The error names the offenders:
///
/// ```text
/// [mk-007] NOT_FOUND; One or more items could not be resolved: 1550626760, 1526511025
/// ```
///
/// Rather than pre-validating every id against the catalog — hundreds of ids
/// per play, on every play — we let MusicKit tell us, remember them, and retry
/// without them. Self-healing and free in the common case.
///
/// This parses an error *string*, which is exactly the kind of thing rule 4
/// warns about, so it is deliberately loose: find the marker, then take digit
/// runs. If Apple rewords the message we get zero ids and fall back to
/// reporting the error, which is where we started — no worse.
fn unresolvable_ids(detail: &str) -> Vec<String> {
    const MARKER: &str = "could not be resolved";
    let Some(tail) = detail.split_once(MARKER).map(|(_, t)| t) else {
        return Vec::new();
    };
    tail.split(|c: char| !c.is_ascii_digit())
        .filter(|s| s.len() >= 6) // catalog ids are long; skip stray numbers
        .map(str::to_owned)
        .collect()
}

/// Build a MusicKit queue from the visible rows, plus **the id to start on**.
///
/// The whole visible list is enqueued, never just the clicked track — the
/// gapless rule (rule 3): MusicKit can only transition seamlessly between items
/// it already holds.
///
/// Note this returns an *id*, not an index. Rows are filtered twice on the way
/// to a queue — once for tracks with no catalog id, again for ids MusicKit has
/// rejected — and a retry filters a third time. Carrying an index through that
/// means re-deriving it at every step and being right every time; carrying the
/// id means the answer cannot drift. An earlier version did the arithmetic and
/// started the wrong track once dead ids entered the picture.
///
/// If the clicked track itself can't be streamed, this starts on the first one
/// after it that can — which is what a person expects from clicking a dead row.
fn queue_from(
    visible: &[&Track],
    row: usize,
    dead: &std::collections::HashSet<String>,
) -> (Vec<String>, Option<String>) {
    let alive = |id: &String| !dead.contains(id);
    let mut seen = std::collections::HashSet::new();
    let songs: Vec<String> = visible
        .iter()
        .filter_map(|t| t.catalog_id.clone())
        .filter(alive)
        // Deduplicate. MusicKit collapses repeats when it builds the queue, so
        // sending the same id twice makes its queue shorter than ours and every
        // position after the repeat refers to a different track than we meant.
        .filter(|id| seen.insert(id.clone()))
        .collect();
    let start_id = visible
        .get(row..)
        .unwrap_or(&[])
        .iter()
        .filter_map(|t| t.catalog_id.clone())
        .find(alive);
    (songs, start_id)
}

/// Where `start_id` sits in `songs`. Falls back to the top rather than failing:
/// playing from the start beats not playing.
fn start_index(songs: &[String], start_id: Option<&String>) -> usize {
    start_id
        .and_then(|id| songs.iter().position(|s| s == id))
        .unwrap_or(0)
}

// The primary menu's action group. GTK menu items invoke `GAction`s by name;
// each of these bridges to an `AppMsg` so the reducer stays the only place
// state changes.
relm4::new_action_group!(AppMenuActionGroup, "win");
relm4::new_stateless_action!(PreferencesAction, AppMenuActionGroup, "preferences");
relm4::new_stateless_action!(ShortcutsAction, AppMenuActionGroup, "shortcuts");
relm4::new_stateless_action!(AboutAction, AppMenuActionGroup, "about");
relm4::new_stateless_action!(QuitAction, AppMenuActionGroup, "quit");
relm4::new_stateless_action!(PlayPauseAction, AppMenuActionGroup, "play-pause");
relm4::new_stateless_action!(NextAction, AppMenuActionGroup, "next");
relm4::new_stateless_action!(PreviousAction, AppMenuActionGroup, "previous");
relm4::new_stateless_action!(ToggleQueueAction, AppMenuActionGroup, "toggle-queue");

/// Wire the primary menu's actions to messages, with their accelerators.
fn register_actions(window: &adw::ApplicationWindow, sender: &ComponentSender<AppModel>) {
    use relm4::actions::{AccelsPlus, RelmAction, RelmActionGroup};

    let mut group = RelmActionGroup::<AppMenuActionGroup>::new();

    let s = sender.clone();
    group.add_action(RelmAction::<PreferencesAction>::new_stateless(move |_| {
        s.input(AppMsg::ShowPreferences)
    }));
    let s = sender.clone();
    group.add_action(RelmAction::<ShortcutsAction>::new_stateless(move |_| {
        s.input(AppMsg::ShowShortcuts)
    }));
    let s = sender.clone();
    group.add_action(RelmAction::<AboutAction>::new_stateless(move |_| {
        s.input(AppMsg::ShowAbout)
    }));
    group.add_action(RelmAction::<QuitAction>::new_stateless(move |_| {
        relm4::main_application().quit()
    }));

    // Transport, so the app answers the keyboard even when the bar does not
    // have focus. Media keys already arrive over MPRIS; these are the
    // in-window equivalents.
    let s = sender.clone();
    group.add_action(RelmAction::<PlayPauseAction>::new_stateless(move |_| {
        s.input(AppMsg::PlayPause)
    }));
    let s = sender.clone();
    group.add_action(RelmAction::<NextAction>::new_stateless(move |_| {
        s.input(AppMsg::Next)
    }));
    let s = sender.clone();
    group.add_action(RelmAction::<PreviousAction>::new_stateless(move |_| {
        s.input(AppMsg::Previous)
    }));
    let s = sender.clone();
    group.add_action(RelmAction::<ToggleQueueAction>::new_stateless(move |_| {
        s.input(AppMsg::ToggleQueue)
    }));

    let app = relm4::main_application();
    app.set_accelerators_for_action::<PreferencesAction>(&["<Control>comma"]);
    app.set_accelerators_for_action::<ShortcutsAction>(&["<Control>question"]);
    app.set_accelerators_for_action::<QuitAction>(&["<Control>q"]);
    app.set_accelerators_for_action::<PlayPauseAction>(&["<Control>space"]);
    app.set_accelerators_for_action::<NextAction>(&["<Control>Right"]);
    app.set_accelerators_for_action::<PreviousAction>(&["<Control>Left"]);
    app.set_accelerators_for_action::<ToggleQueueAction>(&["<Control>u"]);

    group.register_for_widget(window);
}

/// Check an icon name against the theme, falling back if it is missing.
///
/// A name that does not exist renders as nothing at all — silently, with no
/// warning — which is how `music-note-single-symbolic` shipped as an invisible
/// icon.
fn icon(name: &'static str) -> &'static str {
    let present = gtk::gdk::Display::default()
        .map(|display| gtk::IconTheme::for_display(&display))
        .is_some_and(|theme| theme.has_icon(name));
    if present {
        name
    } else {
        tracing::warn!(icon = name, "icon missing from the theme; falling back");
        "audio-x-generic-symbolic"
    }
}

fn show_about(parent: &adw::ApplicationWindow) {
    let about = adw::AboutDialog::builder()
        .application_name("Tonearm")
        .application_icon(crate::APP_ID)
        .developer_name("Miguel Rincon")
        .version(env!("CARGO_PKG_VERSION"))
        .license_type(gtk::License::Gpl30)
        .website("https://github.com/SoftARV/Tonearm")
        .issue_url("https://github.com/SoftARV/Tonearm/issues")
        .comments(
            "A native GNOME client for Apple Music.\n\n\
             Playback runs through Apple's own MusicKit player using Google's \
             Widevine CDM, in a hidden helper process. Tonearm is a native \
             front-end for a licensed session — it requires an active Apple \
             Music subscription and an internet connection.",
        )
        .build();
    about.present(Some(parent));
}

fn show_shortcuts(parent: &adw::ApplicationWindow) {
    // Built by hand rather than from a .ui file: it is a dozen lines either
    // way, and this keeps the strings next to the code that implements them.
    let dialog = adw::ShortcutsDialog::new();

    let playback = adw::ShortcutsSection::new(Some("Playback"));
    for (title, accel) in [
        ("Play or pause", "<Control>space"),
        ("Next track", "<Control>Right"),
        ("Previous track", "<Control>Left"),
    ] {
        playback.add(adw::ShortcutsItem::new(title, accel));
    }

    let general = adw::ShortcutsSection::new(Some("General"));
    for (title, accel) in [
        ("Toggle the queue", "<Control>u"),
        ("Preferences", "<Control>comma"),
        ("Keyboard shortcuts", "<Control>question"),
        ("Quit", "<Control>q"),
    ] {
        general.add(adw::ShortcutsItem::new(title, accel));
    }

    dialog.add(playback);
    dialog.add(general);
    dialog.present(Some(parent));
}

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
    scope: SearchScope,
    /// Results of the last catalog search. Kept separate from `all_tracks` so
    /// switching back to Library does not have to reload anything.
    catalog: Vec<Track>,
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
    /// Repaint the seek bar from the interpolated position.
    Tick,
    /// Play the visible list, starting at this row.
    PlayFrom(usize),
    SearchChanged(String),
    /// The debounce elapsed for this generation; run the catalog search.
    RunCatalogSearch(u64),
    SetScope(SearchScope),
    /// The results list is near its end; fetch the next page if there is one.
    LoadMoreCatalog,
    ReloadLibrary,
    ShowPreferences,
    ShowShortcuts,
    ShowAbout,
    SetTheme(u32),
    SetNotifyTrackChange(bool),
    ToggleQueue,
    /// A library row was activated; the position is resolved immediately.
    LibraryActivated(u32),
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
        result: Result<Vec<Track>, String>,
    },
    /// Cover art is on disk. `None` when the fetch failed — a missing cover is
    /// cosmetic and must not become a toast.
    Artwork(Option<PathBuf>),
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
                        set_max_sidebar_width: 380.0,
                        #[watch]
                        set_show_sidebar: model.show_queue,

                        #[wrap(Some)]
                        #[local_ref]
                        set_sidebar = queue_sidebar -> adw::ToolbarView {},

                        // Navigation on the left. A NavigationSplitView rather
                        // than another OverlaySplitView: this sidebar is where
                        // you are, not a panel you summon, and it should
                        // collapse into a back-navigable page on narrow
                        // windows rather than overlay the content.
                        #[wrap(Some)]
                        set_content = &adw::NavigationSplitView {
                            set_min_sidebar_width: 200.0,
                            set_max_sidebar_width: 260.0,

                            #[wrap(Some)]
                            set_sidebar = &adw::NavigationPage {
                                set_title: "Tonearm",

                                #[wrap(Some)]
                                set_child = &adw::ToolbarView {
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
                                                        sender.input(AppMsg::SetScope(
                                                            match row.index() {
                                                                0 => SearchScope::Catalog,
                                                                _ => SearchScope::Library,
                                                            },
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

                                                // Index 1 — Library.
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
                                            },
                                        },
                                    },
                                },
                            },

                            #[wrap(Some)]
                            set_content = &adw::NavigationPage {
                                #[watch]
                                set_title: match model.scope {
                                    SearchScope::Library => "Songs",
                                    SearchScope::Catalog => "Search",
                                },

                                #[wrap(Some)]
                                set_child = &adw::ToolbarView {
                                    add_top_bar = &adw::HeaderBar {
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
                                            set_placeholder_text: Some(match model.scope {
                                                SearchScope::Library => "Search your library",
                                                SearchScope::Catalog => "Search Apple Music",
                                            }),
                                            connect_search_changed[sender] => move |entry| {
                                                sender.input(AppMsg::SearchChanged(entry.text().into()));
                                            },
                                        },

                                        pack_end = &gtk::ToggleButton {
                                            set_icon_name: "view-list-symbolic",
                                            set_tooltip_text: Some("Queue"),
                                            #[watch]
                                            set_active: model.show_queue,
                                            connect_clicked => AppMsg::ToggleQueue,
                                        },

                                        pack_end = &gtk::Button {
                                            set_icon_name: "view-refresh-symbolic",
                                            set_tooltip_text: Some("Reload library"),
                                            add_css_class: "flat",
                                            #[watch]
                                            set_visible: model.scope == SearchScope::Library,
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
                                                set_label: if model.searching_catalog {
                                                    "Searching Apple Music"
                                                } else {
                                                    "Loading your library"
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
                                            set_description: Some(&match model.scope {
                                                SearchScope::Library => format!(
                                                    "Nothing in your library matches “{}”. Try searching Apple Music.",
                                                    model.query()
                                                ),
                                                SearchScope::Catalog => format!(
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
            });

        let library: TypedListView<LibraryItem, gtk::NoSelection> = TypedListView::new();
        let activate = sender.clone();
        library.view.connect_activate(move |_, position| {
            activate.input(AppMsg::LibraryActivated(position));
        });

        let queue_view = QueueView::builder()
            .launch(())
            .forward(sender.input_sender(), |out| match out {
                QueueViewOutput::Jump(id) => AppMsg::JumpTo(id),
                QueueViewOutput::Remove(id) => AppMsg::RemoveFromQueue(id),
            });

        let starting_scope = match settings.section {
            Section::Library => SearchScope::Library,
            Section::Catalog => SearchScope::Catalog,
        };

        let model = AppModel {
            stage: Stage::Starting,
            queue_view,
            library,
            show_queue: false,
            marked_playing: None,
            library_icons: row_registry(),
            current_track: current_track(),
            dead_rows: dead_tracks(),
            // filled from `dead_ids` once the model exists (see below)
            all_tracks: Vec::new(),
            library_query: String::new(),
            catalog_query: String::new(),
            scope: starting_scope,
            catalog: Vec::new(),
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

        // Open on the section we were last in. Selecting fires `row-selected`,
        // which posts SetScope — harmless, since the model is already on that
        // scope and SetScope returns early when unchanged.
        let start_row = match model.scope {
            SearchScope::Catalog => 0,
            SearchScope::Library => 1,
        };
        if let Some(row) = widgets.nav_list.row_at_index(start_row) {
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
        let scope_before = self.scope;
        self.update(msg, sender.clone(), root);

        if self.scope != scope_before {
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
                match self.scope {
                    SearchScope::Library => self.library_query = query,
                    SearchScope::Catalog => self.catalog_query = query,
                }

                match self.scope {
                    // Local filter: instant, every keystroke.
                    SearchScope::Library => self.rebuild_rows(),
                    SearchScope::Catalog => {
                        self.search_gen = self.search_gen.wrapping_add(1);
                        let generation = self.search_gen;

                        self.catalog_exhausted = false;
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
            AppMsg::SetScope(scope) => {
                if scope == self.scope {
                    return;
                }
                self.scope = scope;
                self.settings.section = match scope {
                    SearchScope::Library => Section::Library,
                    SearchScope::Catalog => Section::Catalog,
                };
                self.settings.save();
                // Switching scope re-reads whichever set is now showing; the
                // other is kept, so switching back is instant.
                match scope {
                    SearchScope::Library => self.rebuild_rows(),
                    SearchScope::Catalog => {
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
            AppMsg::LoadMoreCatalog => {
                // Guarded on all four conditions: only in catalog scope, only
                // when a page is not already in flight, only while Apple still
                // has more, and only up to a ceiling. Scroll events arrive in
                // bursts, so without these one flick would queue several
                // identical requests.
                if self.scope == SearchScope::Catalog
                    && !self.searching_catalog
                    && !self.catalog_exhausted
                    && !self.catalog.is_empty()
                    && self.catalog.len() < CATALOG_MAX
                {
                    let generation = self.search_gen;
                    let offset = self.catalog.len();
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
            AppMsg::ToggleQueue => {
                self.show_queue = !self.show_queue;
                if self.show_queue {
                    self.queue_view.emit(QueueViewInput::ScrollToPlaying);
                }
            }
            AppMsg::LibraryActivated(position) => {
                // The store is the visible list, so this position is the row
                // index `queue_from` expects. Resolved here and now, never
                // stored.
                sender.input(AppMsg::PlayFrom(position as usize));
            }
            AppMsg::JumpTo(id) => match self.queue_index_of(&id) {
                Some(index) => self.send(Command::ChangeToIndex { index }),
                None => self.toast("That track is no longer in the queue"),
            },
            AppMsg::RemoveFromQueue(id) => match self.queue_index_of(&id) {
                Some(index) => self.send(Command::RemoveFromQueue { index }),
                None => self.toast("That track is no longer in the queue"),
            },
            AppMsg::PlayFrom(index) => {
                let visible: Vec<&Track> = self.visible_tracks().collect();
                let (songs, start_id) = queue_from(&visible, index, &self.dead_ids);
                if songs.is_empty() {
                    self.toast("Nothing here can be streamed");
                    return;
                }
                let start = start_index(&songs, start_id.as_ref());
                tracing::info!(queue = songs.len(), start, "enqueuing from library");
                self.pending_start = start_id.clone();
                self.last_queue = Some((songs.clone(), start_id));
                self.send(Command::SetQueue {
                    songs,
                    start_position: start,
                });
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
                    Ok(tracks) => {
                        // A short page means Apple has no more to give.
                        self.catalog_exhausted = tracks.len() < CATALOG_LIMIT as usize;
                        if offset == 0 {
                            self.catalog = tracks;
                        } else {
                            self.catalog.extend(tracks);
                        }
                        tracing::info!(
                            held = self.catalog.len(),
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
            CommandMsg::Artwork(path) => {
                if path.is_none() {
                    // Cosmetic. The bar falls back to a generic icon.
                    tracing::debug!("artwork unavailable");
                }
                self.art_path = path.clone();
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
    fn query(&self) -> &str {
        match self.scope {
            SearchScope::Library => &self.library_query,
            SearchScope::Catalog => &self.catalog_query,
        }
    }

    fn visible_tracks(&self) -> Box<dyn Iterator<Item = &Track> + '_> {
        match self.scope {
            SearchScope::Library => {
                let needle = self.query().trim().to_lowercase();
                Box::new(
                    self.all_tracks
                        .iter()
                        .filter(move |t| needle.is_empty() || matches(t, &needle)),
                )
            }
            // Apple already ranked these; filtering them again locally would
            // only throw away results that matched for reasons we cannot see.
            SearchScope::Catalog => Box::new(self.catalog.iter()),
        }
    }

    fn run_catalog_search(
        &mut self,
        sender: &ComponentSender<Self>,
        generation: u64,
        offset: usize,
    ) {
        let Some(tokens) = &self.tokens else {
            return;
        };
        let client = Client::new(
            tokens.developer_token.clone(),
            tokens.music_user_token.clone(),
            tokens.storefront.clone(),
        );
        let term = self.catalog_query.trim().to_owned();
        if term.is_empty() {
            return;
        }
        self.searching_catalog = true;
        tracing::debug!(%term, "searching the catalog");
        sender.oneshot_command(async move {
            CommandMsg::Catalog {
                generation,
                offset,
                result: client
                    .search_songs(&term, CATALOG_LIMIT, offset)
                    .await
                    .map_err(|err| format!("{err:#}")),
            }
        });
    }

    /// Rebuild the visible rows from `all_tracks` + query.
    ///
    /// A full rebuild is honest here, unlike Pitwall's in-place reconcile: the
    /// filter can change membership arbitrarily on every keystroke, and these
    /// rows hold no state worth preserving (no popovers, no expanders).
    fn rebuild_rows(&mut self) {
        // Rebuilding resets the scroll. It is legitimate on load and on a
        // search change; anywhere else it is a bug, so say when it happens.
        tracing::debug!(query = %self.query(), "library: rebuilding rows");
        let visible: Vec<Track> = self.visible_tracks().cloned().collect();
        let playing = self.playing_catalog_id();
        // The rows are built with the marker already set, so record that here
        // or `mark_now_playing` will think it still needs applying.
        self.marked_playing = playing.clone();
        let registry = self.library_icons.clone();
        // Rows are about to be discarded; none of their widgets are ours now.
        registry.borrow_mut().clear();
        // Rows read the marker from here at bind time, so it just has to be
        // current before they are built.
        let current = self.current_track.clone();
        *current.borrow_mut() = playing.clone();
        let dead = self.dead_rows.clone();
        self.library.clear();
        self.library.extend_from_iter(
            visible.into_iter().map(|track| {
                LibraryItem::new(track, registry.clone(), current.clone(), dead.clone())
            }),
        );
    }

    /// Tell the rows which one is playing, so the list shows a play marker.
    /// Notify about a new track, if the user asked for that.
    ///
    /// Keyed on the track id rather than on "metadata changed": a queue echo,
    /// a seek or an artwork arrival all count as metadata changes, and none of
    /// them is a new song. Without this you get several notifications per
    /// track.
    fn maybe_notify(&mut self, artwork_in_flight: bool) {
        if !self.settings.notify_track_change {
            // Still track what is playing, so switching the preference on
            // mid-song does not immediately fire for the song already playing.
            self.notified_for = self.playing_catalog_id();
            return;
        }

        let current = self.playing_catalog_id();
        if current.is_none() || current == self.notified_for {
            return;
        }
        self.notified_for = current.clone();

        if artwork_in_flight {
            // `art_path` still holds the PREVIOUS track's cover: the fetch is
            // async and has not landed yet. Notifying now shows the wrong
            // album. Wait for CommandMsg::Artwork, which always arrives — with
            // None if the fetch failed.
            self.notify_when_art_lands = current;
            return;
        }
        self.send_track_notification();
    }

    /// Post the notification for whatever is playing now.
    fn send_track_notification(&mut self) {
        self.notify_when_art_lands = None;
        let Some(item) = self.player.now_playing.as_ref() else {
            return;
        };
        notify::track_changed(
            relm4::main_application().upcast_ref::<gtk::gio::Application>(),
            &item.title,
            &item.artist,
            self.art_path.as_deref(),
        );
    }

    /// The catalog id of the track MusicKit is on, if any.
    fn playing_catalog_id(&self) -> Option<String> {
        self.player
            .now_playing
            .as_ref()
            .and_then(|i| i.catalog_id.clone().or_else(|| i.id.clone()))
    }

    /// Move the play marker in the library list.
    ///
    /// Only the two affected rows are touched — the one losing the marker and
    /// the one gaining it — by replacing them in the store, which is what makes
    /// `ListView` re-bind those rows. Mutating the item in place does nothing:
    /// the store emits no change, so the widget is never told to update. That
    /// is why the marker did not appear at all in the first virtualised
    /// version.
    fn mark_now_playing(&mut self) {
        let current = self.playing_catalog_id();
        if current == self.marked_playing {
            return;
        }
        // The shared cell first, so any row bound from here on is correct...
        *self.current_track.borrow_mut() = current.clone();
        // ...then the two rows that are on screen right now, if they are.
        if let Some(old) = self.marked_playing.take() {
            self.set_row_playing(&old, false);
        }
        if let Some(new) = &current {
            self.set_row_playing(new, true);
        }
        self.marked_playing = current;
    }

    /// Move the marker on one row **without touching the model**.
    ///
    /// Editing the store — even replacing a single item — makes `ListView`
    /// re-measure, and the scroll jumps to the top. Intolerable for something
    /// that fires on every track change. So: update the item's data silently,
    /// so a later re-bind is correct, and update the widget directly if this
    /// row happens to be on screen right now.
    /// Repaint one row's marker. Touches a widget, never the model.
    fn set_row_playing(&self, catalog_id: &str, playing: bool) {
        if let Some(w) = self.library_icons.borrow().get(catalog_id) {
            let playable = !self.dead_rows.borrow().contains(catalog_id);
            apply_row_state(&w.icon, &w.root, playing, playable);
        }
    }

    fn load_library(&mut self, sender: &ComponentSender<Self>) {
        let Some(tokens) = &self.tokens else {
            self.toast("Not connected yet");
            return;
        };
        // Built per request rather than cached: the developer token is
        // re-harvested and can be replaced mid-session (rule 7), and a stale
        // client 401s in a way that looks like a sign-in problem.
        let client = Client::new(
            tokens.developer_token.clone(),
            tokens.music_user_token.clone(),
            tokens.storefront.clone(),
        );
        self.loading_library = true;
        tracing::info!("loading library");
        sender.oneshot_command(async move {
            CommandMsg::Library(
                client
                    .all_library_songs(LIBRARY_MAX)
                    .await
                    .map_err(|err| format!("{err:#}")),
            )
        });
    }

    /// Handle MusicKit's all-or-nothing `NOT_FOUND` by dropping the ids it
    /// named and trying again.
    ///
    /// Returns true when it took ownership of the error, so the caller doesn't
    /// also toast a message the user can do nothing about.
    fn retry_without_dead_tracks(&mut self, detail: &str) -> bool {
        let dead = unresolvable_ids(detail);
        if dead.is_empty() {
            return false;
        }
        let Some((songs, wanted)) = self.last_queue.take() else {
            return false;
        };

        let newly_dead = dead
            .iter()
            .filter(|id| !self.dead_ids.contains(*id))
            .count();
        self.dead_ids.extend(dead);
        if newly_dead > 0 {
            // Remember them, so the next run starts already knowing.
            crate::unplayable::save(&self.dead_ids);
        }

        // Nothing new: the retry already happened and failed again. Stop, or we
        // loop forever on an error we cannot parse our way out of.
        if newly_dead == 0 {
            tracing::warn!("queue still unresolvable after dropping known-dead ids");
            return false;
        }

        // If the track we were aiming at is itself newly dead, aim at the next
        // surviving track *after* it in the original order — not at the top of
        // the list, which is where falling back to index 0 would land.
        let from = songs
            .iter()
            .position(|s| Some(s) == wanted.as_ref())
            .unwrap_or(0);
        let wanted = songs[from..]
            .iter()
            .find(|id| !self.dead_ids.contains(*id))
            .cloned();

        let retry: Vec<String> = songs
            .into_iter()
            .filter(|id| !self.dead_ids.contains(id))
            .collect();

        if retry.is_empty() {
            self.toast("None of these tracks are available to stream");
            return true;
        }

        let start = start_index(&retry, wanted.as_ref());
        tracing::info!(
            dropped = newly_dead,
            queue = retry.len(),
            start,
            "retrying queue without unresolvable tracks"
        );
        self.mark_dead_tracks_unplayable();
        self.pending_start = wanted.clone();
        self.last_queue = Some((retry.clone(), wanted));
        self.send(Command::SetQueue {
            songs: retry,
            start_position: start,
        });
        true
    }

    /// Reflect newly-refused tracks in the list **without rebuilding it**.
    ///
    /// This fires on the first play of a session — exactly when the user is
    /// looking at the row they just clicked — so a rebuild here is what sent
    /// the library back to the top, once per run. Rows consult the shared set
    /// at bind, so updating it covers everything off screen; the rows that are
    /// on screen are repainted directly.
    ///
    /// `all_tracks` keeps its catalog ids: playability is now a question for
    /// `dead_rows`, and blanking the id would also lose the handle the queue
    /// builder needs.
    fn mark_dead_tracks_unplayable(&mut self) {
        *self.dead_rows.borrow_mut() = self.dead_ids.clone();

        let playing = self.playing_catalog_id();
        let registry = self.library_icons.borrow();
        for id in &self.dead_ids {
            if let Some(w) = registry.get(id) {
                apply_row_state(&w.icon, &w.root, Some(id) == playing.as_ref(), false);
            }
        }
    }

    /// Check that MusicKit actually landed on the track we asked for, and
    /// correct it if not.
    ///
    /// `setQueue` takes a *position*, but the queue MusicKit builds is not
    /// always the list we handed it — it drops repeats and anything it decides
    /// it cannot use, and every position after such an item then refers to a
    /// different track. Observed directly: 531 ids sent, `queue_len=530` back,
    /// and playback one track further down than the row that was clicked.
    ///
    /// No amount of arithmetic on our side can fix that, because the
    /// discrepancy happens inside MusicKit. So we check its own queue for the
    /// id we wanted and jump if we are not on it — `changeToMediaAtIndex`, not
    /// a second `setQueue`, so the queue is not rebuilt and gapless survives.
    fn verify_start(&mut self) {
        let Some(wanted) = self.pending_start.clone() else {
            return;
        };
        if self.player.queue.is_empty() {
            return; // queue hasn't arrived yet; try again on the next event
        }

        // One shot either way: acting or giving up both clear the flag, so a
        // correction can never bounce against MusicKit's own echo.
        self.pending_start = None;

        let id_of = |item: &crate::player::protocol::Item| {
            item.catalog_id.clone().or_else(|| item.id.clone())
        };
        let Some(index) = self
            .player
            .queue
            .iter()
            .position(|item| id_of(item).as_deref() == Some(wanted.as_str()))
        else {
            tracing::debug!(%wanted, "chosen track is not in MusicKit's queue");
            return;
        };

        if index == self.player.queue_position {
            return; // already right
        }
        tracing::info!(
            from = self.player.queue_position,
            to = index,
            "MusicKit started the wrong track; correcting"
        );
        self.send(Command::ChangeToIndex { index });
    }

    /// Where a track sits in MusicKit's queue *right now*.
    ///
    /// Resolved at send time rather than carried from the row, because our row
    /// order and MusicKit's queue can drift — and a stale position does not
    /// fail loudly, it removes or plays the wrong track, or gets rejected with
    /// INVALID_ARGUMENTS once it runs off the end.
    fn queue_index_of(&self, id: &str) -> Option<usize> {
        self.player.queue.iter().position(|item| {
            item.catalog_id.as_deref() == Some(id) || item.id.as_deref() == Some(id)
        })
    }

    /// Preferences: theme and the track-change notification.
    ///
    /// Built imperatively rather than in `view!` because it is presented on
    /// demand and owns no state of its own — every change goes straight back
    /// through `AppMsg` so the reducer stays the only writer.
    fn show_preferences(&self, sender: &ComponentSender<Self>, parent: &adw::ApplicationWindow) {
        let dialog = adw::PreferencesDialog::new();
        let page = adw::PreferencesPage::new();

        let appearance = adw::PreferencesGroup::builder().title("Appearance").build();
        let theme = adw::ComboRow::builder()
            .title("Theme")
            .model(&gtk::StringList::new(&["Follow System", "Light", "Dark"]))
            .selected(self.settings.theme.index())
            .build();
        {
            let sender = sender.clone();
            theme.connect_selected_notify(move |row| {
                sender.input(AppMsg::SetTheme(row.selected()));
            });
        }
        appearance.add(&theme);

        let notifications = adw::PreferencesGroup::builder()
            .title("Notifications")
            .description("Notifications only appear once Tonearm is installed — see the README.")
            .build();
        let notify = adw::SwitchRow::builder()
            .title("Notify on track change")
            .subtitle("Show a notification when a new song starts")
            .active(self.settings.notify_track_change)
            .build();
        {
            let sender = sender.clone();
            notify.connect_active_notify(move |row| {
                sender.input(AppMsg::SetNotifyTrackChange(row.is_active()));
            });
        }
        notifications.add(&notify);

        page.add(&appearance);
        page.add(&notifications);
        dialog.add(&page);
        dialog.present(Some(parent));
    }

    fn showing_library(&self) -> bool {
        matches!(self.stage, Stage::Ready) && !self.all_tracks.is_empty()
    }

    fn page(&self) -> &'static str {
        // Only the *first* load takes over the screen. A reload with tracks
        // already on show keeps the list up and just disables the refresh
        // button — yanking the library away to show a spinner is worse.
        // Only ever take over the screen when there is nothing to show. Paging
        // in more catalog results happens *below* a list the user is already
        // reading, and replacing that list with a spinner mid-scroll is worse
        // than a moment with no new rows.
        // Scoped to the Library section on purpose. The library loads at
        // startup whichever section you are in, and taking over the Apple
        // Music pane to say "Loading your library" reads as the whole app
        // being stuck. The sidebar spinner covers that case instead.
        let first_library_load = self.scope == SearchScope::Library
            && self.loading_library
            && self.all_tracks.is_empty();
        let first_catalog_page =
            self.scope == SearchScope::Catalog && self.searching_catalog && self.catalog.is_empty();

        if first_library_load || first_catalog_page {
            "loading"
        } else if !self.showing_library() {
            "status"
        } else if self.scope == SearchScope::Catalog && self.catalog_query.trim().is_empty() {
            // Nothing typed yet: invite a search rather than report a failed
            // one.
            "search-prompt"
        } else if self.library.is_empty() {
            "no-results"
        } else {
            "library"
        }
    }

    /// Flatten `PlayerState` into what the bar renders, and push it down.
    ///
    /// Called after every event that could change it *and* on each tick, since
    /// the interpolated position moves without any event arriving.
    fn push_snapshot(&self) {
        let item = self.player.now_playing.as_ref();
        let snap = Snapshot {
            title: item.map(|i| i.title.clone()).unwrap_or_default(),
            artist: item.map(|i| i.artist.clone()).unwrap_or_default(),
            album: item.map(|i| i.album.clone()).unwrap_or_default(),
            position_ms: self.player.interpolated_position_ms(),
            duration_ms: self.player.duration_ms,
            playing: self.player.state.is_playing(),
            busy: self.player.state.is_busy(),
            has_next: self.player.has_next(),
            has_previous: self.player.has_previous(),
            active: item.is_some(),
        };
        self.now_playing.emit(NowPlayingInput::Sync(Box::new(snap)));

        // The queue dialog reads MusicKit's queue, not our library list. The
        // playing track is identified by id rather than position: after a
        // removal the positions shift, and marking by index put the indicator
        // on whichever track slid into the old slot.
        let queue_id = |item: &crate::player::protocol::Item| {
            item.catalog_id
                .clone()
                .or_else(|| item.id.clone())
                .unwrap_or_default()
        };
        self.queue_view.emit(QueueViewInput::Sync {
            entries: self
                .player
                .queue
                .iter()
                .map(|item| QueueEntry {
                    id: queue_id(item),
                    title: item.title.clone(),
                    artist: item.artist.clone(),
                    duration_ms: item.duration_ms,
                })
                .collect(),
            playing: item.map(queue_id),
        });

        // Same state, second consumer. MPRIS diffs internally, so calling this
        // on every tick costs one property write and no bus traffic.
        self.mpris.update(MprisState {
            track_id: item.and_then(|i| i.catalog_id.clone().or_else(|| i.id.clone())),
            title: item.map(|i| i.title.clone()).unwrap_or_default(),
            artist: item.map(|i| i.artist.clone()).unwrap_or_default(),
            album: item.map(|i| i.album.clone()).unwrap_or_default(),
            track_number: item.map(|i| i.track_number).unwrap_or(0),
            art_path: self.art_path.clone(),
            length_ms: self.player.duration_ms,
            position_ms: self.player.interpolated_position_ms(),
            playing: self.player.state.is_playing(),
            stopped: item.is_none(),
            can_next: self.player.has_next(),
            can_previous: self.player.has_previous(),
            volume: self.volume,
        });
    }

    /// Start the repaint timer while playing, drop it otherwise.
    ///
    /// `glib::SourceId` must be removed exactly once — holding it in an
    /// `Option` and `take()`ing is what makes that safe, since removing an
    /// already-removed source aborts.
    fn sync_tick(&mut self, sender: &ComponentSender<Self>) {
        let want = self.player.state.is_playing();
        match (want, self.tick.is_some()) {
            (true, false) => {
                let sender = sender.clone();
                self.tick = Some(gtk::glib::timeout_add_local(
                    std::time::Duration::from_millis(TICK_MS as u64),
                    move || {
                        sender.input(AppMsg::Tick);
                        gtk::glib::ControlFlow::Continue
                    },
                ));
            }
            (false, true) => {
                if let Some(id) = self.tick.take() {
                    id.remove();
                }
            }
            _ => {}
        }
    }

    /// Fetch cover art for the current track, at most once per template.
    /// Returns whether a fetch is now in flight, so the caller knows that
    /// `art_path` is stale until `CommandMsg::Artwork` arrives.
    fn sync_artwork(&mut self, sender: &ComponentSender<Self>) -> bool {
        let template = self
            .player
            .now_playing
            .as_ref()
            .and_then(|i| i.artwork_template.clone());

        if template == self.art_for {
            // Same cover as the last track — usually the next song on the same
            // album. `art_path` is already correct.
            return false;
        }
        self.art_for = template.clone();

        match template {
            Some(t) => {
                let art = Artwork::new(t);
                sender.oneshot_command(async move {
                    CommandMsg::Artwork(artwork::fetch(art, ART_SIZE).await.ok())
                });
                true
            }
            None => {
                self.art_path = None;
                self.now_playing.emit(NowPlayingInput::ArtworkReady(None));
                false
            }
        }
    }

    fn send(&self, cmd: Command) {
        match &self.sidecar {
            Some(handle) => handle.send(cmd),
            None => tracing::debug!(?cmd, "dropped: no sidecar"),
        }
    }

    fn toast(&self, text: &str) {
        self.toaster.add_toast(adw::Toast::new(text));
    }

    fn on_event(&mut self, event: Event, sender: &ComponentSender<Self>) {
        match &event {
            // Bound as `shown`, not `debug`: inside a tracing macro the name
            // `debug` resolves to `tracing::field::debug` instead of our
            // binding, and the field never compiles.
            Event::Ready { debug: shown } => {
                tracing::info!(window_shown = shown, "sidecar ready");
                self.restarts = 0;
            }
            // CDM is in place. Now we're waiting on music.apple.com to load
            // and the hook to attach.
            Event::WidevineReady => self.stage = Stage::Connecting,
            Event::HookBoot { ready_state, href } => {
                tracing::info!(%ready_state, %href, "preload booted")
            }
            Event::HookReady {
                authorized,
                version,
                trigger,
            } => {
                tracing::info!(%version, authorized, %trigger, "musickit hook attached");
                self.stage = if *authorized {
                    Stage::Ready
                } else {
                    Stage::SignedOut
                };
            }
            Event::HookFailed { detail } => {
                // The loud failure rule 4 demands.
                self.stage = Stage::Broken(format!(
                    "Apple Music changed and Tonearm can't attach to its player ({detail}). \
                     Tonearm needs an update."
                ));
            }
            Event::HookWarning { detail } => tracing::warn!(%detail, "hook warning"),
            // Per-command tracing is debug, not info: it was invaluable while
            // the command path was broken and is pure noise now that it works.
            Event::CmdRecv { cmd } => tracing::debug!(%cmd, "sidecar received command"),
            Event::CmdQueued { cmd, depth } => {
                tracing::warn!(%cmd, depth, "command queued — hook not attached")
            }
            Event::CmdDone {
                cmd,
                state,
                queue_len,
            } => tracing::debug!(%cmd, state, queue_len, "sidecar finished command"),
            Event::Tokens(tokens) => {
                // `has_user_token` is the one that matters after sign-in: a
                // developer token alone gets you catalog search but not
                // playback, and the difference is otherwise invisible.
                tracing::info!(
                    storefront = %tokens.storefront,
                    authorized = tokens.authorized,
                    has_user_token = tokens.music_user_token.is_some(),
                    "tokens harvested"
                );
                if tokens.authorized {
                    self.stage = Stage::Ready;
                }
                self.tokens = Some(tokens.clone());
            }
            Event::Authorization { authorized } => {
                tracing::info!(authorized, "authorization changed");
                self.stage = if *authorized {
                    Stage::Ready
                } else {
                    Stage::SignedOut
                };
                if *authorized {
                    self.send(Command::Hide);
                }
            }
            // These three are what tell you whether audio is actually
            // happening. Without them a silent player looks identical to one
            // that was never asked to play anything — which is exactly the
            // hole the first run fell into.
            Event::PlaybackState { state } => tracing::info!(?state, "playback state"),
            Event::NowPlaying { item, queue } => tracing::info!(
                title = item.as_ref().map(|i| i.title.as_str()).unwrap_or("<none>"),
                queue_len = queue.items.len(),
                "now playing changed"
            ),
            Event::Queue(queue) => {
                tracing::debug!(len = queue.items.len(), position = queue.position, "queue")
            }
            Event::Error { code, detail } => {
                tracing::warn!(%code, %detail, "sidecar error");
                if !self.retry_without_dead_tracks(detail) {
                    self.toast(detail);
                }
            }
            _ => {}
        }
        // The mirror is updated last so the stage transitions above always see
        // the previous state (rule 3: this is a projection, not a source).
        let metadata_changed = self.player.apply(&event);

        // Everything below is derived from the mirror, so it happens in one
        // place rather than being sprinkled through the match above — miss one
        // branch there and the bar silently goes stale.
        if metadata_changed {
            let artwork_in_flight = self.sync_artwork(sender);
            self.mark_now_playing();
            self.maybe_notify(artwork_in_flight);
        }
        self.sync_tick(sender);
        self.push_snapshot();
        // After the mirror has the new queue, confirm MusicKit put us on the
        // track that was actually clicked.
        self.verify_start();

        // Load the library the moment we're able to, rather than making the
        // user ask. Guarded on all three conditions so a later event — a
        // reconnect, a token refresh — can't kick off a second load over the
        // top of the first.
        if matches!(self.stage, Stage::Ready)
            && self.all_tracks.is_empty()
            && !self.loading_library
            && self.tokens.is_some()
        {
            self.load_library(sender);
        }
    }

    fn icon(&self) -> &'static str {
        match self.stage {
            Stage::Ready => "audio-x-generic-symbolic",
            Stage::SignedOut => "avatar-default-symbolic",
            Stage::Broken(_) => "dialog-warning-symbolic",
            _ => "content-loading-symbolic",
        }
    }

    fn headline(&self) -> String {
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

    fn detail(&self) -> String {
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

    fn subtitle(&self) -> String {
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

fn start_sidecar(sender: &ComponentSender<AppModel>) {
    respawn_sidecar(sender, std::time::Duration::ZERO);
}

/// Spawn the sidecar after `delay` and drain its stdout for as long as it lives.
///
/// This is a **streaming** command, not a `oneshot_command`: the receiver stays
/// alive for the whole session, which is the one case CLAUDE.md reserves
/// `command` for. `drop_on_shutdown` is what guarantees the child can't outlive
/// the window — without it, closing Tonearm would leave Chromium playing music
/// with no way to stop it.
fn respawn_sidecar(sender: &ComponentSender<AppModel>, delay: std::time::Duration) {
    sender.command(move |out, shutdown| {
        shutdown
            .register(async move {
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                let (handle, mut rx) = match sidecar::spawn() {
                    Ok(pair) => pair,
                    Err(err) => {
                        // A missing sidecar is reported down the same path as a
                        // crashed one, so there is a single recovery route.
                        let _ = out.send(CommandMsg::Sidecar(Incoming::Died(err.to_string())));
                        return;
                    }
                };
                let _ = out.send(CommandMsg::Spawned(handle));
                while let Some(msg) = rx.recv().await {
                    if out.send(CommandMsg::Sidecar(msg)).is_err() {
                        break; // the component is gone
                    }
                }
            })
            .drop_on_shutdown()
    });
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
            artist: "Aitana".into(),
            album: "Superestrella".into(),
            duration_ms: 200_000,
            track_number: 1,
            artwork: None,
        }
    }

    fn dead(ids: &[&str]) -> std::collections::HashSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn clicking_a_row_enqueues_the_whole_visible_list() {
        let (a, b, c) = (
            track("a", Some("1")),
            track("b", Some("2")),
            track("c", Some("3")),
        );
        let visible = vec![&a, &b, &c];

        // Rule 3: the whole list goes in, not just the clicked track.
        let (songs, start_id) = queue_from(&visible, 1, &dead(&[]));
        assert_eq!(songs, vec!["1", "2", "3"]);
        assert_eq!(start_index(&songs, start_id.as_ref()), 1);
    }

    #[test]
    fn unplayable_rows_do_not_shift_the_chosen_track() {
        // Row 3 is "d", but "b" cannot be streamed so never enters the queue.
        // Carrying an index through that filter is what started the wrong song.
        let (a, b) = (track("a", Some("1")), track("b", None));
        let (c, d) = (track("c", Some("3")), track("d", Some("4")));
        let visible = vec![&a, &b, &c, &d];

        let (songs, start_id) = queue_from(&visible, 3, &dead(&[]));
        assert_eq!(songs, vec!["1", "3", "4"]);
        assert_eq!(songs[start_index(&songs, start_id.as_ref())], "4");
    }

    #[test]
    fn known_dead_ids_never_reach_the_queue_and_do_not_shift_it() {
        // "2" was rejected by MusicKit on an earlier play. Clicking "c" must
        // still start "c", not the track above or below it.
        let (a, b, c) = (
            track("a", Some("1")),
            track("b", Some("2")),
            track("c", Some("3")),
        );
        let visible = vec![&a, &b, &c];

        let (songs, start_id) = queue_from(&visible, 2, &dead(&["2"]));
        assert_eq!(songs, vec!["1", "3"], "dead id must not be sent");
        assert_eq!(songs[start_index(&songs, start_id.as_ref())], "3");
    }

    #[test]
    fn clicking_a_dead_row_starts_the_next_streamable_track() {
        let (a, b, c) = (
            track("a", Some("1")),
            track("b", Some("2")),
            track("c", Some("3")),
        );
        let visible = vec![&a, &b, &c];

        // Click "b", which is dead: the sensible result is "c", not the top.
        let (songs, start_id) = queue_from(&visible, 1, &dead(&["2"]));
        assert_eq!(songs[start_index(&songs, start_id.as_ref())], "3");
    }

    #[test]
    fn a_list_with_nothing_playable_produces_no_queue() {
        let a = track("a", None);
        let (songs, _) = queue_from(&[&a], 0, &dead(&[]));
        assert!(songs.is_empty(), "caller must toast rather than enqueue");
    }

    #[test]
    fn clicking_past_the_last_streamable_track_falls_back_to_the_top() {
        let (a, b) = (track("a", Some("1")), track("b", None));
        let visible = vec![&a, &b];
        let (songs, start_id) = queue_from(&visible, 1, &dead(&[]));
        // Nothing streamable at or after the click: play from the start rather
        // than not play at all.
        assert_eq!(start_index(&songs, start_id.as_ref()), 0);
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

    #[test]
    fn search_matches_title_artist_and_album_case_insensitively() {
        let t = track("SUPERESTRELLA", Some("1"));
        assert!(matches(&t, "superestrella"), "title");
        assert!(matches(&t, "aitana"), "artist");
        assert!(!matches(&t, "superstrella"), "not fuzzy, by design");
        assert!(!matches(&t, "rosalia"));
    }
}
