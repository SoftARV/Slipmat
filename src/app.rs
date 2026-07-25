// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Root component.
//!
//! Redux with a compiler: `AppMsg` are the actions, `update` is the sole
//! reducer, and the view is derived from `AppModel`. No I/O happens inline —
//! the sidecar's stdout is drained by a streaming relm4 `Command` so the GTK
//! main thread never blocks (CLAUDE.md rule 8).
//!
//! **M2, the transport slice.** M1's handshake plus the persistent Now Playing
//! bar: artwork, labels, seek, transport and volume. The main surface is still
//! a `StatusPage` — the library lands in M5.

use std::path::PathBuf;

use relm4::adw::prelude::*;
use relm4::{
    Component, ComponentController, ComponentParts, ComponentSender, Controller, adw, gtk,
};

use crate::components::artwork::{self, ART_SIZE};
use crate::components::now_playing::{NowPlaying, NowPlayingInput, NowPlayingOutput, Snapshot};
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

/// What `PlayTestTrack` searches for. Override with `TONEARM_TEST_TERM` to try
/// something else without a rebuild.
///
/// It plays the first search hit, so this is the track you get rather than a
/// guaranteed one — remasters and compilations often outrank an original.
/// Fine for an acceptance test; it retires with the button in M5.
const TEST_TERM: &str = "Aitana SUPERSTRELLA";

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
    /// M1's acceptance test, as a button: search the catalog with the harvested
    /// developer token, then enqueue the first hit. Proves the token, the API
    /// client and the DRM path in one click. Retire it when M5 lands a real
    /// library — until then it is the only way to get audio out of the app.
    PlayTestTrack,
}

#[derive(Debug)]
pub enum CommandMsg {
    /// Everything the sidecar pushed up, including its death.
    Sidecar(Incoming),
    /// The child started; here is the handle for talking to it.
    Spawned(sidecar::Handle),
    /// Catalog search came back with something to enqueue (or an error).
    TestTrack(Result<Vec<Track>, String>),
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
                        #[wrap(Some)]
                        set_title_widget = &adw::WindowTitle {
                            set_title: "Tonearm",
                            #[watch]
                            set_subtitle: &model.subtitle(),
                        },
                    },

                    #[wrap(Some)]
                    set_content = &adw::StatusPage {
                        #[watch]
                        set_icon_name: Some(model.icon()),
                        #[watch]
                        set_title: &model.headline(),
                        #[watch]
                        set_description: Some(&model.detail()),

                        #[wrap(Some)]
                        set_child = &gtk::Box {
                            set_orientation: gtk::Orientation::Horizontal,
                            set_halign: gtk::Align::Center,
                            set_spacing: 12,

                            gtk::Button {
                                set_label: "Sign in to Apple Music",
                                add_css_class: "suggested-action",
                                add_css_class: "pill",
                                #[watch]
                                set_visible: matches!(model.stage, Stage::SignedOut),
                                connect_clicked => AppMsg::SignIn,
                            },

                            // M1's acceptance test. Goes away when M5 lands a
                            // real library to click on.
                            gtk::Button {
                                set_label: "Play a test track",
                                add_css_class: "pill",
                                #[watch]
                                set_visible: matches!(model.stage, Stage::Ready)
                                    && model.player.queue.is_empty(),
                                connect_clicked => AppMsg::PlayTestTrack,
                            },

                        },
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

        let model = AppModel {
            stage: Stage::Starting,
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
            AppMsg::PlayTestTrack => {
                let Some(tokens) = &self.tokens else {
                    self.toast("No tokens yet — wait for the sidecar to connect");
                    return;
                };
                // The client is built per request rather than cached: the
                // developer token is re-harvested and can be replaced mid-session
                // (rule 7), and a stale client would 401 in a way that looks
                // like a sign-in problem.
                let client = Client::new(
                    tokens.developer_token.clone(),
                    tokens.music_user_token.clone(),
                    tokens.storefront.clone(),
                );
                let term =
                    std::env::var("TONEARM_TEST_TERM").unwrap_or_else(|_| TEST_TERM.to_owned());
                tracing::info!(%term, "searching the catalog for a test track");
                sender.oneshot_command(async move {
                    CommandMsg::TestTrack(
                        client
                            .search_songs(&term, 1)
                            .await
                            .map_err(|err| format!("{err:#}")),
                    )
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
            CommandMsg::TestTrack(Ok(tracks)) => match tracks.first() {
                Some(track) => {
                    tracing::info!(
                        id = %track.id, title = %track.title, artist = %track.artist,
                        "enqueuing test track"
                    );
                    // One setQueue with the whole list — the gapless rule
                    // (rule 3), even for a list of one.
                    self.send(Command::SetQueue {
                        songs: tracks.iter().map(|t| t.id.0.clone()).collect(),
                        start_position: 0,
                    });
                }
                None => self.toast("Search returned no songs"),
            },
            CommandMsg::TestTrack(Err(err)) => {
                tracing::warn!(%err, "catalog search failed");
                self.toast(&format!("Search failed: {err}"));
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
                self.toast(detail);
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
        }
        self.sync_tick(sender);
        self.push_snapshot();
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
