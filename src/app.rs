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
use relm4::factory::FactoryVecDeque;
use relm4::{
    Component, ComponentController, ComponentParts, ComponentSender, Controller, RelmWidgetExt,
    adw, gtk,
};

use crate::components::artwork::{self, ART_SIZE};
use crate::components::now_playing::{NowPlaying, NowPlayingInput, NowPlayingOutput, Snapshot};
use crate::components::track_row::{TrackRow, TrackRowInit, TrackRowInput, TrackRowOutput};
use crate::mpris::{Mpris, MprisState};
use crate::music::client::Client;
use crate::music::types::{Artwork, Track};
use crate::player::protocol::{Command, Event, Tokens};
use crate::player::{Incoming, PlayerState, sidecar};

/// How often the seek bar redraws while playing.
///
/// The sidecar's own position events are coarse and irregular, so
/// `PlayerState::interpolated_position_ms` fills the gaps; this timer just
/// drives the repaint. Removed entirely when not playing — a paused player
/// should cost nothing (the same discipline as Pitwall's suspend-gated poll).
const TICK_MS: u32 = 500;

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

/// Build a MusicKit queue from the visible rows, and translate a row index into
/// a queue position.
///
/// The **whole visible list** is enqueued, never just the clicked track — that
/// is the gapless rule (rule 3): MusicKit can only transition seamlessly
/// between items it already holds.
///
/// The translation is the subtle part. Unplayable tracks are shown as rows but
/// cannot go in the queue, so row index and queue position drift apart by the
/// number of unplayable rows above the click. Using the row index directly
/// would start the wrong track in any library containing an upload — silently,
/// and only for some rows.
fn queue_from(visible: &[&Track], row: usize) -> (Vec<String>, usize) {
    let songs = visible
        .iter()
        .filter_map(|t| t.catalog_id.clone())
        .collect::<Vec<_>>();
    let start = visible
        .iter()
        .take(row)
        .filter(|t| t.playable())
        .count()
        .min(songs.len().saturating_sub(1));
    (songs, start)
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
    /// The rows on screen — the filtered view.
    library: FactoryVecDeque<TrackRow>,
    /// The full library from the last load. The filter reads this, never the
    /// factory, so narrowing and then clearing a search is lossless.
    all_tracks: Vec<Track>,
    query: String,
    loading_library: bool,
    /// Catalog ids MusicKit has told us it cannot resolve. Remembered for the
    /// session so a delisted track only breaks one play attempt, not every one.
    dead_ids: std::collections::HashSet<String>,
    /// The last queue we tried, so a `NOT_FOUND` can be retried without the
    /// offenders instead of making the user click again.
    last_queue: Option<(Vec<String>, usize)>,
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
    ReloadLibrary,
}

#[derive(Debug)]
pub enum CommandMsg {
    /// Everything the sidecar pushed up, including its death.
    Sidecar(Incoming),
    /// The child started; here is the handle for talking to it.
    Spawned(sidecar::Handle),
    /// The user's library, or why it couldn't be read.
    Library(Result<Vec<Track>, String>),
    /// Cover art is on disk. `None` when the fetch failed — a missing cover is
    /// cosmetic and must not become a toast.
    Artwork(Option<PathBuf>),
}

#[relm4::component(pub)]
impl Component for AppModel {
    type Init = ();
    type Input = AppMsg;
    type Output = ();
    type CommandOutput = CommandMsg;

    view! {
        adw::ApplicationWindow {
            set_title: Some("Tonearm"),
            set_default_width: 900,
            set_default_height: 640,

            #[local_ref]
            toaster -> adw::ToastOverlay {
                adw::ToolbarView {
                    add_top_bar = &adw::HeaderBar {
                        // Title until there is a library, then the search box.
                        // A bare hidden SearchEntry would leave the header
                        // blank while connecting.
                        #[wrap(Some)]
                        set_title_widget = &gtk::Stack {
                            add_named[Some("title")] = &adw::WindowTitle {
                                set_title: "Tonearm",
                                #[watch]
                                set_subtitle: &model.subtitle(),
                            },

                            add_named[Some("search")] = &gtk::SearchEntry {
                                set_placeholder_text: Some("Search your library"),
                                set_width_request: 320,
                                connect_search_changed[sender] => move |entry| {
                                    sender.input(AppMsg::SearchChanged(entry.text().into()));
                                },
                            },

                            // AFTER the children, deliberately. relm4 applies
                            // these in source order, and setting a visible child
                            // by name before that child has been added warns and
                            // does nothing.
                            #[watch]
                            set_visible_child_name: if model.showing_library() {
                                "search"
                            } else {
                                "title"
                            },
                        },

                        pack_end = &gtk::Button {
                            set_icon_name: "view-refresh-symbolic",
                            set_tooltip_text: Some("Reload library"),
                            add_css_class: "flat",
                            #[watch]
                            set_visible: model.showing_library(),
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

                        add_named[Some("library")] = &gtk::ScrolledWindow {
                            set_vexpand: true,

                            #[wrap(Some)]
                            set_child = &adw::Clamp {
                                set_maximum_size: 800,

                                #[local_ref]
                                library_list -> gtk::ListBox {
                                    set_selection_mode: gtk::SelectionMode::None,
                                    set_valign: gtk::Align::Start,
                                    set_margin_all: 12,
                                    add_css_class: "boxed-list",
                                },
                            },
                        },

                        // Distinct from "status": an empty library and a
                        // search with no matches are different problems and
                        // must not share a message.
                        add_named[Some("no-results")] = &adw::StatusPage {
                            set_icon_name: Some("system-search-symbolic"),
                            set_title: "No matches",
                            #[watch]
                            set_description: Some(&format!("Nothing in your library matches “{}”.", model.query)),
                        },

                        // After the children — see the note on the title stack.
                        #[watch]
                        set_visible_child_name: model.page(),
                    },

                    // The bar is present on every screen — it is the app.
                    // Wrapped in a Box so the visibility watch has somewhere to
                    // live: the bar itself is a child component, and `#[watch]`
                    // can only drive widgets this macro owns.
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
        _init: Self::Init,
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

        let library = FactoryVecDeque::builder()
            .launch(gtk::ListBox::default())
            .forward(sender.input_sender(), |out| match out {
                TrackRowOutput::Activated(index) => AppMsg::PlayFrom(index),
            });

        let model = AppModel {
            stage: Stage::Starting,
            library,
            all_tracks: Vec::new(),
            query: String::new(),
            loading_library: false,
            dead_ids: std::collections::HashSet::new(),
            last_queue: None,
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
        };
        let toaster = &model.toaster;
        let now_playing_bar = model.now_playing.widget();
        let library_list = model.library.widget();
        let widgets = view_output!();

        start_sidecar(&sender);

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
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
                if query != self.query {
                    self.query = query;
                    self.rebuild_rows();
                }
            }
            AppMsg::ReloadLibrary => self.load_library(&sender),
            AppMsg::PlayFrom(index) => {
                let visible: Vec<&Track> = self.visible_tracks().collect();
                let (mut songs, mut start) = queue_from(&visible, index);
                // Drop ids already known to be unresolvable, keeping the start
                // position pointing at the same track.
                if !self.dead_ids.is_empty() {
                    let before = songs.len();
                    let dropped_before_start = songs[..start]
                        .iter()
                        .filter(|id| self.dead_ids.contains(*id))
                        .count();
                    songs.retain(|id| !self.dead_ids.contains(id));
                    start = start.saturating_sub(dropped_before_start);
                    tracing::debug!(dropped = before - songs.len(), "excluded known-dead ids");
                }
                if songs.is_empty() {
                    self.toast("Nothing here can be streamed");
                    return;
                }
                start = start.min(songs.len().saturating_sub(1));
                tracing::info!(queue = songs.len(), start, "enqueuing from library");
                self.last_queue = Some((songs.clone(), start));
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
    fn visible_tracks(&self) -> impl Iterator<Item = &Track> {
        let needle = self.query.trim().to_lowercase();
        self.all_tracks
            .iter()
            .filter(move |t| needle.is_empty() || matches(t, &needle))
    }

    /// Rebuild the visible rows from `all_tracks` + query.
    ///
    /// A full rebuild is honest here, unlike Pitwall's in-place reconcile: the
    /// filter can change membership arbitrarily on every keystroke, and these
    /// rows hold no state worth preserving (no popovers, no expanders).
    fn rebuild_rows(&mut self) {
        let visible: Vec<Track> = self.visible_tracks().cloned().collect();
        let mut rows = self.library.guard();
        rows.clear();
        for (index, track) in visible.into_iter().enumerate() {
            rows.push_back(TrackRowInit { track, index });
        }
    }

    /// Tell the rows which one is playing, so the list shows a play marker.
    fn mark_now_playing(&self) {
        let current = self
            .player
            .now_playing
            .as_ref()
            .and_then(|i| i.catalog_id.clone().or_else(|| i.id.clone()));
        self.library.broadcast(TrackRowInput::NowPlaying(current));
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
        let Some((songs, start)) = self.last_queue.take() else {
            return false;
        };

        let newly_dead = dead
            .iter()
            .filter(|id| !self.dead_ids.contains(*id))
            .count();
        self.dead_ids.extend(dead);

        // Nothing new: the retry already happened and failed again. Stop, or we
        // loop forever on an error we cannot parse our way out of.
        if newly_dead == 0 {
            tracing::warn!("queue still unresolvable after dropping known-dead ids");
            return false;
        }

        let dropped_before_start = songs[..start.min(songs.len())]
            .iter()
            .filter(|id| self.dead_ids.contains(*id))
            .count();
        let retry: Vec<String> = songs
            .into_iter()
            .filter(|id| !self.dead_ids.contains(id))
            .collect();

        if retry.is_empty() {
            self.toast("None of these tracks are available to stream");
            return true;
        }

        let start = start
            .saturating_sub(dropped_before_start)
            .min(retry.len() - 1);
        tracing::info!(
            dropped = newly_dead,
            queue = retry.len(),
            "retrying queue without unresolvable tracks"
        );
        self.mark_dead_tracks_unplayable();
        self.last_queue = Some((retry.clone(), start));
        self.send(Command::SetQueue {
            songs: retry,
            start_position: start,
        });
        true
    }

    /// Reflect newly-discovered dead ids in the list, so the affected rows dim
    /// instead of looking playable and doing nothing.
    fn mark_dead_tracks_unplayable(&mut self) {
        let mut changed = false;
        for track in &mut self.all_tracks {
            if let Some(id) = &track.catalog_id
                && self.dead_ids.contains(id)
            {
                track.catalog_id = None;
                changed = true;
            }
        }
        if changed {
            self.rebuild_rows();
        }
    }

    fn showing_library(&self) -> bool {
        matches!(self.stage, Stage::Ready) && !self.all_tracks.is_empty()
    }

    fn page(&self) -> &'static str {
        if !self.showing_library() {
            "status"
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
    fn sync_artwork(&mut self, sender: &ComponentSender<Self>) {
        let template = self
            .player
            .now_playing
            .as_ref()
            .and_then(|i| i.artwork_template.clone());

        if template == self.art_for {
            return;
        }
        self.art_for = template.clone();

        match template {
            Some(t) => {
                let art = Artwork::new(t);
                sender.oneshot_command(async move {
                    CommandMsg::Artwork(artwork::fetch(art, ART_SIZE).await.ok())
                });
            }
            None => self.now_playing.emit(NowPlayingInput::ArtworkReady(None)),
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
            self.sync_artwork(sender);
            self.mark_now_playing();
        }
        self.sync_tick(sender);
        self.push_snapshot();

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
                .map(|i| format!("{} — {}", i.artist, i.album))
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

    #[test]
    fn clicking_a_row_enqueues_the_whole_visible_list() {
        let a = track("a", Some("1"));
        let b = track("b", Some("2"));
        let c = track("c", Some("3"));
        let visible = vec![&a, &b, &c];

        // Rule 3: the whole list goes in, not just the clicked track — that is
        // what lets MusicKit transition gaplessly.
        let (songs, start) = queue_from(&visible, 1);
        assert_eq!(songs, vec!["1", "2", "3"]);
        assert_eq!(start, 1);
    }

    #[test]
    fn unplayable_rows_shift_the_queue_position() {
        // Row 3 is "d", but "b" cannot be streamed and so never enters the
        // queue. Using the row index directly would start "c" instead.
        let a = track("a", Some("1"));
        let b = track("b", None);
        let c = track("c", Some("3"));
        let d = track("d", Some("4"));
        let visible = vec![&a, &b, &c, &d];

        let (songs, start) = queue_from(&visible, 3);
        assert_eq!(songs, vec!["1", "3", "4"]);
        assert_eq!(songs[start], "4", "must start the track that was clicked");
    }

    #[test]
    fn a_list_with_nothing_playable_produces_no_queue() {
        let a = track("a", None);
        let visible = vec![&a];
        let (songs, _) = queue_from(&visible, 0);
        assert!(songs.is_empty(), "caller must toast rather than enqueue");
    }

    #[test]
    fn a_row_past_the_last_playable_track_still_lands_in_range() {
        // Clicking the last row when everything below it is unplayable must not
        // produce a start position past the end of the queue.
        let a = track("a", Some("1"));
        let b = track("b", None);
        let visible = vec![&a, &b];
        let (songs, start) = queue_from(&visible, 1);
        assert!(
            start < songs.len(),
            "start {start} out of range for {songs:?}"
        );
    }

    #[test]
    fn unresolvable_ids_are_parsed_out_of_musickits_error() {
        // setQueue is all-or-nothing: one delisted track kills the whole queue.
        // The error names the offenders, which is how we recover.
        let detail = "setQueue: [mk-007] NOT_FOUND; One or more items could not \
be resolved: 1550626760, 1526511025, 1550626763";
        let ids = unresolvable_ids(detail);
        assert_eq!(ids, vec!["1550626760", "1526511025", "1550626763"]);
    }

    #[test]
    fn a_reworded_error_yields_nothing_rather_than_garbage() {
        // Rule 4: this parses a string Apple controls. If the wording changes we
        // must degrade to reporting the error, not invent ids.
        assert!(unresolvable_ids("[mk-007] NOT_FOUND; something else entirely").is_empty());
        assert!(unresolvable_ids("").is_empty());
    }

    #[test]
    fn short_numbers_in_the_error_are_not_mistaken_for_ids() {
        // "mk-007" precedes the marker, but a stray small number after it must
        // not be treated as a catalog id either.
        let ids = unresolvable_ids("could not be resolved: 42, 1550626760");
        assert_eq!(ids, vec!["1550626760"]);
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
