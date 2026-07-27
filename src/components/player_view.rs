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
    /// Enough width to put the artwork beside the controls rather than above.
    wide: bool,
    /// Whether the queue is showing inside the drawer.
    queue_shown: bool,
    /// The transport, built once and moved between two slots.
    transport: gtk::Box,
    /// The hand-built transport's refreshable pieces. See [`Bits`].
    bits: Option<Bits>,
    /// The containers `relayout` moves things between. Only available once the
    /// widgets exist, which is after `view_output!`.
    slots: Option<Slots>,
}

/// The four places something can live, plus the queue itself.
struct Slots {
    queue: adw::ToolbarView,
    queue_wide: gtk::Box,
    queue_compact: gtk::Box,
    transport_stacked: gtk::Box,
    transport_compact: gtk::Box,
}

/// How much of the window the open drawer claims.
const WINDOW_FRACTION: f64 = 0.7;

/// The shortest the drawer may ever be, in logical pixels. A floor low enough
/// that it is never what stops the window from shrinking.
const SHEET_MIN_H: i32 = 260;

/// Tie the drawer's height to the window's, at [`WINDOW_FRACTION`].
///
/// `AdwBottomSheet` sizes the sheet to its child's **natural height** and
/// offers no maximum, minimum or fraction of its own, so a drawer that should
/// fill most of the window has to be told how tall that is. There is nothing
/// to bind to; the number has to be computed and pushed down.
///
/// The basis is the toplevel `GdkSurface`'s height, which is the one size that
/// notifies on *every* resize — including tiling and maximising, which
/// `GtkWindow:default-height` deliberately does not track (it stores the size
/// to restore *to*, so it holds still exactly when the window is snapped).
///
/// Reading the surface rather than the sheet's own allocation also keeps this
/// acyclic: our request changes how tall the sheet is, and how tall the sheet
/// is never changes the surface. Measuring the sheet would be a loop.
///
/// The request is dropped entirely while the drawer is closed. A height
/// request is a *minimum*, and a minimum that tracks the current height would
/// fight the user as they drag the window shorter — so the app only carries it
/// when the drawer is actually open and asking to be tall.
///
/// `Rc` rather than `Arc` because every one of these callbacks is a GTK signal
/// handler: they all run on the main thread, and none of them crosses one.
pub fn fill_window(
    window: &adw::ApplicationWindow,
    sheet: &adw::BottomSheet,
    content: &gtk::Widget,
) {
    let apply: std::rc::Rc<dyn Fn()> = {
        let (window, sheet, content) = (window.clone(), sheet.clone(), content.clone());
        std::rc::Rc::new(move || {
            let height = window.surface().map_or(0, |surface| surface.height());
            let target = (f64::from(height) * WINDOW_FRACTION) as i32;
            content.set_height_request(if sheet.is_open() && height > 0 {
                target.max(SHEET_MIN_H)
            } else {
                -1
            });
        })
    };

    window.connect_realize({
        let apply = apply.clone();
        move |window| {
            let Some(surface) = window.surface() else {
                return;
            };
            apply();
            let apply = apply.clone();
            surface.connect_height_notify(move |_| apply());
        }
    });

    sheet.connect_open_notify(move |_| apply());
}

/// Build the scrubber and the transport row into `into`.
///
/// By hand rather than in `view!` because this block **moves** between two
/// containers depending on the layout, and the macro's tree is fixed. Building
/// it once and reparenting is what keeps one set of buttons driving one player
/// — the alternative is two transports that have to be kept in step.
///
/// The labels and the scale are handed back through `Bits` so `update` can
/// refresh them; `#[watch]` cannot reach a widget the macro does not own.
fn build_transport(into: &gtk::Box, sender: &ComponentSender<PlayerView>) -> Bits {
    let elapsed = gtk::Label::builder()
        .css_classes(["numeric", "caption"])
        .build();
    let remaining = gtk::Label::builder()
        .css_classes(["numeric", "caption"])
        .build();
    let scale = gtk::Scale::builder()
        .hexpand(true)
        .draw_value(false)
        .build();
    {
        let sender = sender.clone();
        scale.connect_change_value(move |_, _, v| {
            sender.input(PlayerViewInput::Scrub(v));
            gtk::glib::Propagation::Proceed
        });
    }

    let scrubber_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    scrubber_row.append(&elapsed);
    scrubber_row.append(&scale);
    scrubber_row.append(&remaining);
    // Clamped rather than filling. A scrubber that spans a maximised window is
    // both hard to aim — one pixel is most of a second — and unlike anything
    // else in the drawer, which is a centred column. `AdwClamp` is the widget
    // for "up to this wide, then centre", and using it keeps the ceiling out of
    // the widget's own minimum, so the compact layout still shrinks freely.
    into.append(
        &adw::Clamp::builder()
            .maximum_size(SCRUB_MAX_W)
            .child(&scrubber_row)
            .build(),
    );

    let buttons = gtk::Box::builder()
        .spacing(10)
        .halign(gtk::Align::Center)
        .build();

    let button = |icon: &str, classes: &[&str]| {
        gtk::Button::builder()
            .icon_name(icon)
            .css_classes(classes.iter().map(|c| c.to_string()).collect::<Vec<_>>())
            .build()
    };

    let shuffle = gtk::ToggleButton::builder()
        .icon_name("media-playlist-shuffle-symbolic")
        .tooltip_text("Shuffle")
        .css_classes(["flat", "circular"])
        .build();
    {
        let sender = sender.clone();
        shuffle.connect_toggled(move |b| sender.input(PlayerViewInput::Shuffle(b.is_active())));
    }
    let previous = button("media-skip-backward-symbolic", &["flat", "circular"]);
    let play = button(
        "media-playback-start-symbolic",
        &["suggested-action", "circular"],
    );
    play.set_width_request(56);
    play.set_height_request(56);
    let next = button("media-skip-forward-symbolic", &["flat", "circular"]);
    let repeat = button("media-playlist-repeat-symbolic", &["flat", "circular"]);
    repeat.set_tooltip_text(Some("Repeat"));
    // Only the way *in*. Closing belongs to the queue's own header, so this
    // hides once the queue is showing rather than becoming a second control
    // for the same thing.
    let queue = button("view-list-symbolic", &["flat", "circular"]);
    queue.set_tooltip_text(Some("Queue"));

    for (widget, msg) in [
        (&previous, PlayerViewInput::Previous),
        (&play, PlayerViewInput::PlayPause),
        (&next, PlayerViewInput::Next),
        (&repeat, PlayerViewInput::RepeatCycle),
        (&queue, PlayerViewInput::SetQueueShown(true)),
    ] {
        let sender = sender.clone();
        widget.connect_clicked(move |_| sender.input(msg.clone()));
    }

    for w in [
        shuffle.upcast_ref::<gtk::Widget>(),
        previous.upcast_ref(),
        play.upcast_ref(),
        next.upcast_ref(),
        repeat.upcast_ref(),
        queue.upcast_ref(),
    ] {
        buttons.append(w);
    }
    into.append(&buttons);

    Bits {
        elapsed,
        remaining,
        scale,
        shuffle,
        play,
        previous,
        next,
        repeat,
        queue,
    }
}

/// The pieces of the hand-built transport that `update` has to refresh.
struct Bits {
    elapsed: gtk::Label,
    remaining: gtk::Label,
    scale: gtk::Scale,
    shuffle: gtk::ToggleButton,
    play: gtk::Button,
    previous: gtk::Button,
    next: gtk::Button,
    repeat: gtk::Button,
    queue: gtk::Button,
}

/// Move a widget to a new parent, if it is not already there.
fn reparent(child: &impl IsA<gtk::Widget>, new_parent: &gtk::Box) {
    let child = child.as_ref();
    if child.parent().as_ref() == Some(new_parent.upcast_ref::<gtk::Widget>()) {
        return;
    }
    if let Some(old) = child.parent() {
        if let Some(old) = old.downcast_ref::<gtk::Box>() {
            old.remove(child);
        } else {
            child.unparent();
        }
    }
    new_parent.append(child);
}

/// Below this the artwork goes above the controls instead of beside them.
const WIDE_PX: i32 = 860;
/// Artwork sizes: generous when it is the subject, a thumbnail when the queue
/// has taken the space and it is only there to say which record this is.
const ART_LARGE: i32 = 260;
const ART_THUMB: i32 = 72;

/// The widest the scrubber may get before it stops growing and centres.
const SCRUB_MAX_W: i32 = 520;

/// How long the scrubber waits after the last movement before seeking.
const SCRUB_COMMIT_MS: u64 = 250;

#[derive(Debug, Clone)]
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
    /// The width breakpoint crossed.
    Wide(bool),
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
            // Low enough that the drawer never becomes the window's floor.
            // The whole point of the compact layout is that the app can be
            // tiled to half a screen, and a minimum here would undo that.
            //
            // How tall it actually opens is [`fill_window`]'s business, not
            // this number's: this is the floor it may never go under, that is
            // the share of the window it asks for.
            set_size_request: (300, SHEET_MIN_H),

            #[wrap(Some)]
            set_child = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                add_css_class: "np-sheet",

                // The player column and, when there is width for it, the queue
                // beside it. Horizontal always: what changes is whether the
                // queue column next to it is showing.
                #[name = "top"]
                gtk::Box {
                    // No padding and no spacing **here**: the queue is one of
                    // this box's two children and it wants to sit flush against
                    // the drawer's edge, the way it did as a sidebar. Padding
                    // is the player column's own business, below.
                    set_spacing: 0,
                    // Only claims the height when it is the thing worth
                    // looking at. In the compact layout with the queue open
                    // the queue is, and this shrinks to the thumbnail and the
                    // title above it.
                    #[watch]
                    set_vexpand: model.stacked(),

                    // The player, as one column: artwork, then what is
                    // playing, then the transport **under** it. The transport
                    // used to sit in the metadata column, which put it beside
                    // the artwork rather than below it in every layout wide
                    // enough to have a choice.
                    #[name = "player_col"]
                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_hexpand: true,
                        set_spacing: 16,
                        set_margin_start: 24,
                        set_margin_end: 24,
                        // Centred in the drawer when it is a column, pinned to
                        // the top when the queue is below it and wants the rest.
                        #[watch]
                        set_valign: if model.stacked() {
                            gtk::Align::Center
                        } else {
                            gtk::Align::Start
                        },

                        // Artwork above the metadata, except in the one layout
                        // with no room for it — compact, queue open — where the
                        // artwork shrinks to a thumbnail and sits beside it.
                        #[name = "head"]
                        gtk::Box {
                            set_spacing: 16,
                            #[watch]
                            set_orientation: if model.stacked() {
                                gtk::Orientation::Vertical
                            } else {
                                gtk::Orientation::Horizontal
                            },

                            #[name = "art_slot"]
                            gtk::Box {
                                #[watch]
                                set_halign: if model.stacked() {
                                    gtk::Align::Center
                                } else {
                                    gtk::Align::Start
                                },
                                #[watch]
                                set_valign: if model.stacked() {
                                    gtk::Align::End
                                } else {
                                    gtk::Align::Center
                                },
                            },

                            gtk::Box {
                                set_orientation: gtk::Orientation::Vertical,
                                set_hexpand: true,
                                set_valign: gtk::Align::Center,
                                set_spacing: 2,
                                #[watch]
                                set_halign: if model.centred_text() {
                                    gtk::Align::Center
                                } else {
                                    gtk::Align::Start
                                },

                                gtk::Label {
                                    add_css_class: "title-1",
                                    set_ellipsize: gtk::pango::EllipsizeMode::End,
                                    set_max_width_chars: 28,
                                    set_use_markup: false,
                                    #[watch]
                                    set_xalign: if model.centred_text() { 0.5 } else { 0.0 },
                                    #[watch]
                                    set_label: &model.snap.title,
                                },
                                gtk::Label {
                                    add_css_class: "title-4",
                                    add_css_class: "dim-label",
                                    set_ellipsize: gtk::pango::EllipsizeMode::End,
                                    set_max_width_chars: 34,
                                    set_use_markup: false,
                                    #[watch]
                                    set_xalign: if model.centred_text() { 0.5 } else { 0.0 },
                                    #[watch]
                                    set_label: &model.subtitle(),
                                },
                            },
                        },

                        // Where the transport lives whenever the artwork is
                        // above it, which is every layout but one.
                        #[name = "transport_stacked"]
                        gtk::Box { set_orientation: gtk::Orientation::Vertical },
                    },

                    // Where the queue lives when it can be a column of its own.
                    #[name = "queue_wide"]
                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        #[watch]
                        // Only when it is a column of its own; in the compact
                        // layout the queue is the full width and must not carry
                        // a floor.
                        set_width_request: if model.wide { 320 } else { -1 },
                    },
                },

                // ...and where each goes when it cannot.
                #[name = "queue_compact"]
                gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_vexpand: true,
                },

                #[name = "transport_compact"]
                gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_margin_start: 24,
                    set_margin_end: 24,
                    set_margin_bottom: 18,
                },
            },
        }
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let mut model = PlayerView {
            snap: Snapshot::default(),
            cover: Cover::new(240),
            scrubbing: false,
            scrub_gen: 0,
            wide: true,
            queue_shown: false,
            transport: gtk::Box::new(gtk::Orientation::Vertical, 12),
            slots: None,
            bits: None,
        };
        let queue = QUEUE_SLOT
            .with(|q| q.borrow().clone())
            .expect("the queue widget must be handed over before the player view is built");
        let widgets = view_output!();
        model.cover.attach_first(&widgets.art_slot);
        model.cover.square("audio-x-generic-symbolic");

        model.bits = Some(build_transport(&model.transport, &sender));
        model.slots = Some(Slots {
            queue,
            queue_wide: widgets.queue_wide.clone(),
            queue_compact: widgets.queue_compact.clone(),
            transport_stacked: widgets.transport_stacked.clone(),
            transport_compact: widgets.transport_compact.clone(),
        });
        model.relayout();

        // One breakpoint. The other decision — whether the queue is showing —
        // is the user's, and combining the two is what `relayout` is for.
        if let Ok(condition) =
            adw::BreakpointCondition::parse(&format!("max-width: {}px", WIDE_PX - 1))
        {
            let breakpoint = adw::Breakpoint::new(condition);
            let narrowed = sender.clone();
            breakpoint.connect_apply(move |_| narrowed.input(PlayerViewInput::Wide(false)));
            let widened = sender.clone();
            breakpoint.connect_unapply(move |_| widened.input(PlayerViewInput::Wide(true)));
            widgets.root.add_breakpoint(breakpoint);
        } else {
            tracing::warn!("unparsable breakpoint; the player will not adapt");
        }

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
                self.refresh_transport();
            }
            PlayerViewInput::Artwork(path) => match path {
                Some(path) => self.cover.set_file(&path),
                None => self.cover.square("audio-x-generic-symbolic"),
            },
            PlayerViewInput::Scrub(v) => {
                self.scrubbing = true;
                self.snap.position_ms = v as u64;
                self.refresh_transport();
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
            PlayerViewInput::Wide(wide) => {
                if self.wide != wide {
                    self.wide = wide;
                    self.relayout();
                }
            }
            PlayerViewInput::SetQueueShown(shown) => {
                if self.queue_shown == shown {
                    return; // our own echo
                }
                self.queue_shown = shown;
                self.relayout();
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
    /// Whether the artwork sits **above** the rest of the player rather than
    /// beside it.
    ///
    /// One question, asked in four places, and the reason it is a method: this
    /// is the layout. Every layout stacks — artwork, then what is playing, then
    /// the transport under it — except the single case with no vertical room to
    /// spare, compact with the queue open, where the artwork becomes a
    /// thumbnail beside the title and the queue takes the height.
    fn stacked(&self) -> bool {
        self.wide || !self.queue_shown
    }

    /// Text is centred only when the artwork is above it — a column reads as a
    /// column. Beside a thumbnail, it aligns left.
    fn centred_text(&self) -> bool {
        self.stacked()
    }

    /// Push the current snapshot into the hand-built transport.
    ///
    /// The macro's `#[watch]` cannot reach these — they are built outside its
    /// tree so they can move between layouts — so this is the equivalent, and
    /// it has the same obligation: set **every** property it cares about, since
    /// the last track left its own values behind.
    fn refresh_transport(&self) {
        let Some(bits) = self.bits.as_ref() else {
            return;
        };
        bits.elapsed
            .set_label(&format_duration(self.snap.position_ms));
        bits.remaining.set_label(&format_duration(
            self.snap.duration_ms.saturating_sub(self.snap.position_ms),
        ));
        bits.scale
            .set_range(0.0, self.snap.duration_ms.max(1) as f64);
        bits.scale.set_sensitive(self.snap.duration_ms > 0);
        // Only when it is not the user's: a drag must not be argued with.
        if !self.scrubbing {
            bits.scale.set_value(self.snap.position_ms as f64);
        }
        bits.play.set_icon_name(if self.snap.playing {
            "media-playback-pause-symbolic"
        } else {
            "media-playback-start-symbolic"
        });
        bits.play.set_sensitive(self.snap.active);
        bits.previous.set_sensitive(self.snap.has_previous);
        bits.next.set_sensitive(self.snap.has_next);
        // `set_active` fires `toggled`, and the handler would send the value
        // straight back — so only touch it when it actually differs.
        if bits.shuffle.is_active() != self.snap.shuffle {
            bits.shuffle.set_active(self.snap.shuffle);
        }
        bits.repeat.set_icon_name(match self.snap.repeat {
            Repeat::One => "media-playlist-repeat-song-symbolic",
            _ => "media-playlist-repeat-symbolic",
        });
        bits.repeat
            .set_opacity(if matches!(self.snap.repeat, Repeat::Off) {
                0.5
            } else {
                1.0
            });
    }

    /// Put the transport and the queue where this layout wants them.
    ///
    /// They are **moved, not duplicated**. The transport is one widget with one
    /// set of signal handlers, and the queue is the app's own `QueueView` — a
    /// second copy of either would be two things claiming to be the same
    /// player, which is the failure this whole component is arranged to avoid.
    ///
    /// Called on a breakpoint or a toggle, so a handful of times a session
    /// rather than per frame.
    fn relayout(&self) {
        let Some(slots) = self.slots.as_ref() else {
            return;
        };
        // The transport goes under the artwork wherever the artwork is above
        // it, which is everywhere but the compact layout with the queue open —
        // there it drops to the foot of the drawer, under the queue, because
        // that is still below the artwork and it must stay visible.
        //
        // The queue's home is a different question, and asking it separately is
        // the point: it depends on width alone, the transport's on the layout.
        let transport_home = if self.stacked() {
            &slots.transport_stacked
        } else {
            &slots.transport_compact
        };
        let queue_home = if self.wide {
            &slots.queue_wide
        } else {
            &slots.queue_compact
        };
        reparent(&self.transport, transport_home);
        reparent(&slots.queue, queue_home);

        // The artwork is the elastic element: large when it is the subject,
        // a thumbnail once the queue needs the room.
        self.cover
            .resize(if self.stacked() { ART_LARGE } else { ART_THUMB });

        // One control at a time: the transport's button opens the queue, the
        // queue's own header closes it. Two buttons, but never both on screen,
        // which is what keeps it from reading as a duplicate.
        if let Some(bits) = self.bits.as_ref() {
            bits.queue.set_visible(!self.queue_shown);
        }

        slots.queue.set_visible(self.queue_shown);
        slots.queue_wide.set_visible(self.queue_shown && self.wide);
        slots
            .queue_compact
            .set_visible(self.queue_shown && !self.wide);
    }

    fn subtitle(&self) -> String {
        match (self.snap.artist.is_empty(), self.snap.album.is_empty()) {
            (false, false) => format!("{} — {}", self.snap.artist, self.snap.album),
            (false, true) => self.snap.artist.clone(),
            (true, false) => self.snap.album.clone(),
            (true, true) => String::new(),
        }
    }
}
