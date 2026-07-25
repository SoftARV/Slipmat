// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Root component.
//!
//! Redux with a compiler: `AppMsg` are the actions, `update` is the sole
//! reducer, and the view is derived from `AppModel`. No I/O happens inline —
//! the sidecar's stdout is drained by a streaming relm4 `Command` so the GTK
//! main thread never blocks (CLAUDE.md rule 8).
//!
//! **M1, the handshake slice.** Spawn the sidecar, wait for Widevine and the
//! MusicKit hook, run Apple's sign-in once, harvest the tokens. The UI here is
//! deliberately a single `adw::StatusPage` reporting where we are in that
//! sequence — the Now Playing bar arrives in M2.

use relm4::adw::prelude::*;
use relm4::{Component, ComponentParts, ComponentSender, adw, gtk};

use crate::music::client::Client;
use crate::music::types::Track;
use crate::player::protocol::{Command, Event, Tokens};
use crate::player::{Incoming, PlayerState, sidecar};

/// What `PlayTestTrack` searches for. Override with `TONEARM_TEST_TERM` to try
/// something else without a rebuild.
const TEST_TERM: &str = "Yes Roundabout";

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
}

#[derive(Debug)]
pub enum AppMsg {
    SignIn,
    PlayPause,
    Next,
    Previous,
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

                            gtk::Button {
                                set_icon_name: "media-skip-backward-symbolic",
                                add_css_class: "circular",
                                #[watch]
                                set_visible: matches!(model.stage, Stage::Ready),
                                #[watch]
                                set_sensitive: model.player.has_previous(),
                                connect_clicked => AppMsg::Previous,
                            },
                            gtk::Button {
                                add_css_class: "circular",
                                add_css_class: "suggested-action",
                                #[watch]
                                set_icon_name: if model.player.state.is_playing() {
                                    "media-playback-pause-symbolic"
                                } else {
                                    "media-playback-start-symbolic"
                                },
                                #[watch]
                                set_visible: matches!(model.stage, Stage::Ready),
                                connect_clicked => AppMsg::PlayPause,
                            },
                            gtk::Button {
                                set_icon_name: "media-skip-forward-symbolic",
                                add_css_class: "circular",
                                #[watch]
                                set_visible: matches!(model.stage, Stage::Ready),
                                #[watch]
                                set_sensitive: model.player.has_next(),
                                connect_clicked => AppMsg::Next,
                            },
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
        let model = AppModel {
            stage: Stage::Starting,
            player: PlayerState::new(),
            tokens: None,
            sidecar: None,
            restarts: 0,
            toaster: adw::ToastOverlay::new(),
        };
        let toaster = &model.toaster;
        let widgets = view_output!();

        start_sidecar(&sender);

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            AppMsg::SignIn => self.send(Command::ShowLogin),
            AppMsg::PlayPause => self.send(Command::PlayPause),
            AppMsg::Next => self.send(Command::Next),
            AppMsg::Previous => self.send(Command::Previous),
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
            CommandMsg::Sidecar(Incoming::Event(event)) => self.on_event(event),
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
    fn send(&self, cmd: Command) {
        match &self.sidecar {
            Some(handle) => handle.send(cmd),
            None => tracing::debug!(?cmd, "dropped: no sidecar"),
        }
    }

    fn toast(&self, text: &str) {
        self.toaster.add_toast(adw::Toast::new(text));
    }

    fn on_event(&mut self, event: Event) {
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
            Event::CmdRecv { cmd } => tracing::info!(%cmd, "sidecar received command"),
            Event::CmdDone {
                cmd,
                state,
                queue_len,
            } => tracing::info!(%cmd, state, queue_len, "sidecar finished command"),
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
        self.player.apply(&event);
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
