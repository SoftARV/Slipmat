// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The expanded player: the Now Playing bar opened out into a drawer.
//!
//! A separate component from [`super::now_playing`] rather than a second mode
//! of it, for two reasons. That file is already at its size budget and this is
//! not a small view; and the two are genuinely different shapes — the bar is a
//! strip that must survive being 400px wide, this is a page that assumes room.
//!
//! What they share is deliberate: the same [`Snapshot`] in, the same
//! [`NowPlayingOutput`] out. The transport here cannot drift from the
//! transport there, because they are the same messages handled by the same
//! reducer arms. Anything else would be two players disagreeing about one
//! MusicKit.

use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;
use relm4::prelude::*;

use super::cover::Cover;
use super::now_playing::{NowPlayingOutput, Repeat, Snapshot};
use crate::music::types::format_duration;

pub struct PlayerView {
    snap: Snapshot,
    cover: Cover,
    /// True while the user is dragging the scrubber, so incoming positions do
    /// not yank the handle out from under them — the same rule the bar follows.
    scrubbing: bool,
    /// Bumped per drag movement; only the newest commit is honoured.
    scrub_gen: u64,
    /// Room to put the transport beside the artwork rather than under it.
    wide: bool,
    /// Not enough room for the queue *and* the player side by side. The queue
    /// then takes the player's place rather than squeezing it, which is what
    /// every phone-sized music app does and the only thing that fits.
    cramped: bool,
    /// Whether the queue is showing inside the drawer.
    queue_shown: bool,
}

/// Below this the transport goes under the artwork instead of beside it.
const WIDE_PX: i32 = 980;
/// Below this the queue cannot share the width with the player.
const CRAMPED_PX: i32 = 700;
/// Artwork is the thing worth the space when there is space.
const ART_WIDE: i32 = 280;
const ART_NARROW: i32 = 180;

/// How long the scrubber waits after the last movement before seeking.
const SCRUB_COMMIT_MS: u64 = 250;

#[derive(Debug)]
pub enum PlayerViewInput {
    Sync(Box<Snapshot>),
    Artwork(Option<std::path::PathBuf>),
    Scrub(f64),
    /// Only the newest scrub commits — the same generation trick the bar's seek
    /// uses, and for the same reason: dragging emits continuously and every
    /// intermediate value would be a seek MusicKit has to service.
    ScrubDone(u64, f64),
    PlayPause,
    Next,
    Previous,
    Shuffle(bool),
    /// A breakpoint fired. The view watches these rather than the breakpoints
    /// setting properties directly, because two of the decisions here depend on
    /// the queue toggle as well as on the width.
    Layout {
        wide: bool,
    },
    Cramped(bool),
    SetQueueShown(bool),
    /// Cycle to the next repeat mode. No payload: the mirror says what is
    /// current, and this view must not have an opinion of its own (rule 3).
    RepeatCycle,
}

#[relm4::component(pub)]
impl SimpleComponent for PlayerView {
    type Init = ();
    type Input = PlayerViewInput;
    type Output = NowPlayingOutput;

    view! {
        #[name = "root"]
        adw::BreakpointBin {
            // The floor the *content* can actually reach, which is what makes
            // the breakpoints meaningful: a bin that cannot get narrow never
            // crosses its own thresholds.
            set_size_request: (340, 320),

            #[wrap(Some)]
            set_child = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                add_css_class: "np-sheet",
                set_vexpand: true,

                // The queue toggle lives **above** the two panes, not inside
                // the player. When the drawer is narrow the queue takes the
                // player's place, and a toggle that goes with it would be a
                // door that locks behind you.
                gtk::Box {
                    set_halign: gtk::Align::End,
                    set_margin_top: 8,
                    set_margin_end: 12,

                    gtk::ToggleButton {
                        set_icon_name: "view-list-symbolic",
                        set_tooltip_text: Some("Queue"),
                        add_css_class: "flat",
                        add_css_class: "circular",
                        #[watch]
                        set_active: model.queue_shown,
                        // `SetQueueShown`, not a flip. `#[watch] set_active`
                        // makes this a two-way binding, and GTK fires
                        // `toggled` for a programmatic set exactly as for a
                        // click — a flip would answer its own echo and the
                        // button would oscillate. The handler ignores a value
                        // it already holds.
                        connect_toggled[sender] => move |b| {
                            sender.input(PlayerViewInput::SetQueueShown(b.is_active()));
                        },
                    },
                },

                gtk::Box {
                    set_vexpand: true,

                    // The player. Hidden only when the queue has taken its
                    // place for want of width.
                    #[name = "player_side"]
                    gtk::Box {
                        set_hexpand: true,
                        set_halign: gtk::Align::Center,
                        set_valign: gtk::Align::Center,
                        set_spacing: 28,
                        set_margin_all: 24,
                        // Wide: artwork beside the controls. Narrow: under them.
                        #[watch]
                        set_orientation: if model.wide {
                            gtk::Orientation::Horizontal
                        } else {
                            gtk::Orientation::Vertical
                        },
                        #[watch]
                        set_visible: !(model.cramped && model.queue_shown),

                        #[name = "art_slot"]
                        gtk::Box {
                            set_halign: gtk::Align::Center,
                            set_valign: gtk::Align::Center,
                        },

                        gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_valign: gtk::Align::Center,
                            set_spacing: 20,

                            gtk::Box {
                                set_orientation: gtk::Orientation::Vertical,
                                set_spacing: 4,
                                #[watch]
                                set_halign: if model.wide {
                                    gtk::Align::Start
                                } else {
                                    gtk::Align::Center
                                },

                                gtk::Label {
                                    add_css_class: "title-1",
                                    set_ellipsize: gtk::pango::EllipsizeMode::End,
                                    set_max_width_chars: 30,
                                    set_use_markup: false,
                                    #[watch]
                                    set_xalign: if model.wide { 0.0 } else { 0.5 },
                                    #[watch]
                                    set_label: &model.snap.title,
                                },
                                gtk::Label {
                                    add_css_class: "title-4",
                                    add_css_class: "dim-label",
                                    set_ellipsize: gtk::pango::EllipsizeMode::End,
                                    set_max_width_chars: 36,
                                    set_use_markup: false,
                                    #[watch]
                                    set_xalign: if model.wide { 0.0 } else { 0.5 },
                                    #[watch]
                                    set_label: &model.subtitle(),
                                },
                            },

                            gtk::Box {
                                set_spacing: 10,
                                // No width request. A fixed one here made the
                                // column's minimum wider than the drawer's own
                                // minimum, which Adwaita reports as "GtkBox
                                // exceeds AdwBreakpointBin width" — the content
                                // cannot be allowed a floor the container does
                                // not have. The scale expands instead.
                                set_hexpand: true,

                                gtk::Label {
                                    add_css_class: "numeric",
                                    add_css_class: "caption",
                                    #[watch]
                                    set_label: &format_duration(model.snap.position_ms),
                                },

                                #[name = "scrubber"]
                                gtk::Scale {
                                    set_hexpand: true,
                                    set_draw_value: false,
                                    #[watch]
                                    set_range: (0.0, model.snap.duration_ms.max(1) as f64),
                                    #[watch]
                                    set_sensitive: model.snap.duration_ms > 0,
                                    connect_change_value[sender] => move |_, _, v| {
                                        sender.input(PlayerViewInput::Scrub(v));
                                        gtk::glib::Propagation::Proceed
                                    },
                                },

                                gtk::Label {
                                    add_css_class: "numeric",
                                    add_css_class: "caption",
                                    #[watch]
                                    set_label: &format_duration(
                                        model.snap.duration_ms.saturating_sub(model.snap.position_ms),
                                    ),
                                },
                            },

                            gtk::Box {
                                set_spacing: 12,
                                #[watch]
                                set_halign: if model.wide {
                                    gtk::Align::Start
                                } else {
                                    gtk::Align::Center
                                },

                                gtk::ToggleButton {
                                    set_icon_name: "media-playlist-shuffle-symbolic",
                                    set_tooltip_text: Some("Shuffle"),
                                    add_css_class: "flat",
                                    add_css_class: "circular",
                                    #[watch]
                                    set_active: model.snap.shuffle,
                                    connect_toggled[sender] => move |b| {
                                        sender.input(PlayerViewInput::Shuffle(b.is_active()));
                                    },
                                },

                                gtk::Button {
                                    set_icon_name: "media-skip-backward-symbolic",
                                    add_css_class: "flat",
                                    add_css_class: "circular",
                                    #[watch]
                                    set_sensitive: model.snap.has_previous,
                                    connect_clicked[sender] => move |_| {
                                        sender.input(PlayerViewInput::Previous);
                                    },
                                },

                                gtk::Button {
                                    add_css_class: "suggested-action",
                                    add_css_class: "circular",
                                    set_width_request: 56,
                                    set_height_request: 56,
                                    #[watch]
                                    set_icon_name: if model.snap.playing {
                                        "media-playback-pause-symbolic"
                                    } else {
                                        "media-playback-start-symbolic"
                                    },
                                    #[watch]
                                    set_sensitive: model.snap.active,
                                    connect_clicked[sender] => move |_| {
                                        sender.input(PlayerViewInput::PlayPause);
                                    },
                                },

                                gtk::Button {
                                    set_icon_name: "media-skip-forward-symbolic",
                                    add_css_class: "flat",
                                    add_css_class: "circular",
                                    #[watch]
                                    set_sensitive: model.snap.has_next,
                                    connect_clicked[sender] => move |_| {
                                        sender.input(PlayerViewInput::Next);
                                    },
                                },

                                gtk::Button {
                                    add_css_class: "flat",
                                    add_css_class: "circular",
                                    set_tooltip_text: Some("Repeat"),
                                    #[watch]
                                    set_icon_name: match model.snap.repeat {
                                        Repeat::One => "media-playlist-repeat-song-symbolic",
                                        _ => "media-playlist-repeat-symbolic",
                                    },
                                    #[watch]
                                    set_opacity: if matches!(model.snap.repeat, Repeat::Off) {
                                        0.5
                                    } else {
                                        1.0
                                    },
                                    connect_clicked[sender] => move |_| {
                                        sender.input(PlayerViewInput::RepeatCycle);
                                    },
                                },
                            },
                        },
                    },

                    #[name = "queue_separator"]
                    gtk::Separator {
                        set_orientation: gtk::Orientation::Vertical,
                        #[watch]
                        set_visible: model.queue_shown && !model.cramped,
                    },

                    #[local_ref]
                    queue -> adw::ToolbarView {
                        #[watch]
                        set_visible: model.queue_shown,
                        #[watch]
                        set_hexpand: model.cramped,
                        #[watch]
                        set_width_request: if model.cramped { -1 } else { 340 },
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
        let model = PlayerView {
            snap: Snapshot::default(),
            cover: Cover::new(240),
            scrubbing: false,
            scrub_gen: 0,
            wide: true,
            cramped: false,
            queue_shown: false,
        };
        let queue = QUEUE_SLOT
            .with(|q| q.borrow().clone())
            .expect("the queue widget must be handed over before the player view is built");
        let widgets = view_output!();
        model.cover.attach_first(&widgets.art_slot);
        model.cover.square("audio-x-generic-symbolic");
        model.cover.resize(ART_WIDE);

        // Two breakpoints, reported to the model rather than setting properties
        // themselves. Two of the decisions here — whether the player is visible
        // at all, and how wide the queue is — depend on the queue toggle as
        // well as on the width, and a setter cannot see that.
        add_breakpoint(&widgets.root, &format!("max-width: {}px", WIDE_PX - 1), {
            let sender = sender.clone();
            move |narrow| sender.input(PlayerViewInput::Layout { wide: !narrow })
        });
        add_breakpoint(
            &widgets.root,
            &format!("max-width: {}px", CRAMPED_PX - 1),
            {
                let sender = sender.clone();
                move |cramped| sender.input(PlayerViewInput::Cramped(cramped))
            },
        );

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            PlayerViewInput::Sync(snap) => {
                // While dragging, the position is the user's, not the player's.
                let position = if self.scrubbing {
                    self.snap.position_ms
                } else {
                    snap.position_ms
                };
                self.snap = *snap;
                self.snap.position_ms = position;
            }
            PlayerViewInput::Artwork(path) => match path {
                Some(path) => self.cover.set_file(&path),
                None => self.cover.square("audio-x-generic-symbolic"),
            },
            PlayerViewInput::Scrub(v) => {
                self.scrubbing = true;
                self.snap.position_ms = v as u64;
                // Debounced: a drag emits on every motion event, and seeking on
                // each one would have MusicKit re-buffering continuously.
                self.scrub_gen = self.scrub_gen.wrapping_add(1);
                let generation = self.scrub_gen;
                let sender = sender.clone();
                gtk::glib::timeout_add_local_once(
                    std::time::Duration::from_millis(SCRUB_COMMIT_MS),
                    move || sender.input(PlayerViewInput::ScrubDone(generation, v)),
                );
            }
            PlayerViewInput::ScrubDone(generation, v) => {
                // A later drag supersedes this one.
                if generation != self.scrub_gen {
                    return;
                }
                self.scrubbing = false;
                let _ = sender.output(NowPlayingOutput::Seek(v as u64));
            }
            PlayerViewInput::PlayPause => {
                let _ = sender.output(NowPlayingOutput::PlayPause);
            }
            PlayerViewInput::Next => {
                let _ = sender.output(NowPlayingOutput::Next);
            }
            PlayerViewInput::Previous => {
                let _ = sender.output(NowPlayingOutput::Previous);
            }
            PlayerViewInput::Layout { wide } => {
                if self.wide != wide {
                    self.wide = wide;
                    // The artwork is the thing worth the space when there is
                    // space, and the thing to give up first when there is not.
                    self.cover.resize(if wide { ART_WIDE } else { ART_NARROW });
                }
            }
            PlayerViewInput::Cramped(cramped) => self.cramped = cramped,
            PlayerViewInput::SetQueueShown(shown) => {
                if self.queue_shown == shown {
                    return; // our own echo
                }
                self.queue_shown = shown;
            }
            PlayerViewInput::Shuffle(on) => {
                let _ = sender.output(NowPlayingOutput::SetShuffle(on));
            }
            PlayerViewInput::RepeatCycle => {
                // Cycles through the three modes; the mirror decides what is
                // next, exactly as the bar's button does.
                let next = match self.snap.repeat {
                    Repeat::Off => Repeat::All,
                    Repeat::All => Repeat::One,
                    Repeat::One => Repeat::Off,
                };
                let _ = sender.output(NowPlayingOutput::SetRepeat(next));
            }
        }
    }
}

/// Attach one breakpoint and report both edges of it.
///
/// `AdwBreakpoint` fires `apply` when its condition starts holding and
/// `unapply` when it stops, so a single bool needs both wired or the layout
/// only ever changes in one direction.
fn add_breakpoint(bin: &adw::BreakpointBin, condition: &str, on: impl Fn(bool) + Clone + 'static) {
    let Ok(condition) = adw::BreakpointCondition::parse(condition) else {
        tracing::warn!(condition, "unparsable breakpoint; layout will not adapt");
        return;
    };
    let breakpoint = adw::Breakpoint::new(condition);
    let applied = on.clone();
    breakpoint.connect_apply(move |_| applied(true));
    breakpoint.connect_unapply(move |_| on(false));
    bin.add_breakpoint(breakpoint);
}

thread_local! {
    /// Where the queue widget is left for `init` to collect.
    ///
    /// relm4's `view!` builds the widget tree before the model exists, and the
    /// queue is a sibling component owned by the app — there is no init payload
    /// that can carry a `&Widget` through. Handing it over on this cell keeps
    /// the queue a *moved* component rather than a second implementation, which
    /// is what issue #18 asked for.
    static QUEUE_SLOT: std::cell::RefCell<Option<adw::ToolbarView>> =
        const { std::cell::RefCell::new(None) };
}

/// Lend the queue widget to the player view being built next.
pub fn hand_over_queue(queue: adw::ToolbarView) {
    QUEUE_SLOT.with(|q| *q.borrow_mut() = Some(queue));
}

impl PlayerView {
    fn subtitle(&self) -> String {
        match (self.snap.artist.is_empty(), self.snap.album.is_empty()) {
            (false, false) => format!("{} — {}", self.snap.artist, self.snap.album),
            (false, true) => self.snap.artist.clone(),
            (true, false) => self.snap.album.clone(),
            (true, true) => String::new(),
        }
    }
}
