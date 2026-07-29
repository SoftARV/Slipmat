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
use relm4::gtk::{gdk, glib};
use relm4::prelude::*;

use self::transport::{Bits, build_transport};
use super::cover::{Cover, SWAP_MS};
use super::now_playing::{NowPlayingOutput, Snapshot};

mod transport;

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
    /// The artwork's live pixel size. In a `Cell` behind an `Rc` because the
    /// animation callback owns a copy and outlives any one `relayout` — and
    /// reading it back is what lets an interrupted transition resume from
    /// where it actually is rather than snapping to where it started.
    art_px: std::rc::Rc<std::cell::Cell<i32>>,
    /// Drives the artwork between its two sizes. `None` until `init` has a
    /// widget to hang it on: an animation needs a frame clock, and a frame
    /// clock comes from a widget.
    art_anim: Option<adw::TimedAnimation>,
}

/// The four places something can live, plus the queue itself.
struct Slots {
    queue: adw::ToolbarView,
    queue_wide_rev: gtk::Revealer,
    queue_compact_rev: gtk::Revealer,
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
/// `AdwBottomSheet` sizes the sheet to its child's **natural height** and offers
/// no maximum or fraction of its own, so the number has to be computed and
/// pushed down.
///
/// The basis is the toplevel `GdkSurface`'s height, for two reasons. It is the
/// one size that notifies on *every* resize, including tiling and maximising —
/// `GtkWindow:default-height` deliberately does not track those, because it
/// stores the size to restore *to*. And reading the surface keeps this acyclic:
/// our request changes the sheet's height, and the sheet never changes the
/// surface.
///
/// While closed this falls back to [`SHEET_MIN_H`] — **not to `-1`**. The
/// request has to come off, or it fights the user dragging the window shorter.
/// But `-1` does not restore the floor `view!` declared with `set_size_request`;
/// it *clears* it, because they are the same property, leaving the
/// `AdwBreakpointBin` with no minimum height and libadwaita warning by name:
///
/// ```text
/// AdwBreakpointBin does not have a minimum height, set the 'height-request'
/// property to specify it
/// ```
///
/// `Rc` rather than `Arc`: these are GTK signal handlers, all on the main
/// thread.
pub fn fill_window(
    window: &adw::ApplicationWindow,
    sheet: &adw::BottomSheet,
    content: &gtk::Widget,
) {
    let apply: std::rc::Rc<dyn Fn()> = {
        let (window, sheet, content) = (window.clone(), sheet.clone(), content.clone());
        std::rc::Rc::new(move || {
            let height = window.surface().map_or(0, |surface| surface.height());
            let target = if sheet.is_open() && height > 0 {
                (f64::from(height) * WINDOW_FRACTION) as i32
            } else {
                SHEET_MIN_H
            };
            content.set_height_request(target.max(SHEET_MIN_H));
        })
    };

    // One handler at a time, not one per realize.
    //
    // Hiding the window unrealizes it and `Ctrl`+`W` makes that routine (#32),
    // so `realize` fires more than once per session and each firing sees a new
    // `GdkSurface`. Connecting without disconnecting leaves a handler on every
    // surface the window has ever had. They are harmless — a dead surface never
    // notifies, and `set_size_request` no-ops on an unchanged value — but the
    // list only grows, which is the kind of thing that is free until it is not.
    let connected: std::rc::Rc<std::cell::RefCell<Option<(gdk::Surface, glib::SignalHandlerId)>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    window.connect_realize({
        let apply = apply.clone();
        move |window| {
            let Some(surface) = window.surface() else {
                return;
            };
            if let Some((old, id)) = connected.borrow_mut().take() {
                old.disconnect(id);
            }
            apply();
            let id = surface.connect_height_notify({
                let apply = apply.clone();
                move |_| apply()
            });
            *connected.borrow_mut() = Some((surface, id));
        }
    });

    sheet.connect_open_notify(move |_| apply());
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

/// How long the queue takes to slide in or out, and the artwork to follow it.
/// One number for both: they are one movement and must not finish apart.
const QUEUE_ANIM_MS: u32 = 250;

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
    /// The width breakpoint crossed.
    Wide(bool),
    SetQueueShown(bool),
    /// Flip shuffle. No payload: the value is derived from the mirrored one,
    /// so this view never invents one (rule 3).
    ShuffleClicked,
    /// Cycle repeat, for the same reason.
    RepeatClicked,
    VolumeChanged(f64),
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
                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_hexpand: true,
                        set_spacing: 16,
                        set_margin_start: 24,
                        set_margin_end: 24,
                        // Not on `top`: the queue shares that box and wants to
                        // stay flush. The drawer's drag handle is drawn over
                        // the top edge, so without this the artwork starts
                        // under it and, in the compact layout, the title is
                        // written straight through it.
                        set_margin_top: 24,
                        set_margin_bottom: 24,
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

                                // Crossfaded, not flipped.
                                //
                                // These are two readings of one state, and a
                                // queue emptying used to cut between them: the
                                // title vanishing and the grey bars appearing
                                // in the same frame. A `GtkStack` dissolves
                                // between its pages instead, and the page is
                                // chosen in `post_view` rather than by a
                                // `#[watch]`, because a transition is an
                                // animation and animated properties are
                                // written on an edge.
                                #[name = "meta_stack"]
                                gtk::Stack {
                                    set_transition_type: gtk::StackTransitionType::Crossfade,
                                    set_transition_duration: SWAP_MS,

                                    // They differ in **length**, not in weight:
                                    // a title runs long and an artist is
                                    // usually a name. Each carries its own
                                    // `halign` because a vertical `GtkBox`
                                    // fills its children across and
                                    // `set_size_request` is only a minimum, so
                                    // without it both drew the same length.
                                    add_named[Some("empty")] = &gtk::Box {
                                        set_orientation: gtk::Orientation::Vertical,
                                        set_spacing: 10,
                                        set_margin_top: 4,
                                        set_margin_bottom: 4,
                                        set_valign: gtk::Align::Center,

                                        gtk::Box {
                                            #[watch]
                                            set_halign: if model.centred_text() {
                                                gtk::Align::Center
                                            } else {
                                                gtk::Align::Start
                                            },
                                            set_size_request: (240, 16),
                                            add_css_class: "np-skeleton",
                                        },
                                        gtk::Box {
                                            #[watch]
                                            set_halign: if model.centred_text() {
                                                gtk::Align::Center
                                            } else {
                                                gtk::Align::Start
                                            },
                                            set_size_request: (120, 16),
                                            add_css_class: "np-skeleton",
                                        },
                                    },

                                    add_named[Some("track")] = &gtk::Box {
                                        set_orientation: gtk::Orientation::Vertical,
                                        set_spacing: 2,
                                        set_valign: gtk::Align::Center,

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
                            },
                        },

                        // Where the transport lives whenever the artwork is
                        // above it, which is every layout but one.
                        #[name = "transport_stacked"]
                        gtk::Box { set_orientation: gtk::Orientation::Vertical },
                    },

                    // Where the queue lives when it can be a column of its own.
                    //
                    // Wrapped in a `GtkRevealer` so it slides in from the edge
                    // rather than appearing. That also animates everything
                    // beside it for free: the revealer grows its own width
                    // over the transition, so the player column is squeezed
                    // continuously instead of jumping to its new size.
                    #[name = "queue_wide_rev"]
                    gtk::Revealer {
                        set_transition_type: gtk::RevealerTransitionType::SlideLeft,
                        set_transition_duration: QUEUE_ANIM_MS,

                        #[wrap(Some)]
                        #[name = "queue_wide"]
                        set_child = &gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            #[watch]
                            // Only when it is a column of its own; in the
                            // compact layout the queue is the full width and
                            // must not carry a floor.
                            set_width_request: if model.wide { 320 } else { -1 },
                        },
                    },
                },

                // ...and where each goes when it cannot. Upwards here, because
                // in the compact layout the queue rises from the foot of the
                // drawer rather than in from the side.
                #[name = "queue_compact_rev"]
                gtk::Revealer {
                    set_transition_type: gtk::RevealerTransitionType::SlideUp,
                    set_transition_duration: QUEUE_ANIM_MS,
                    // **Only while it is actually showing.** A collapsed
                    // revealer draws nothing but still claims its share of the
                    // expansion, so leaving this on meant this and `top` split
                    // the drawer's height between them — and the player,
                    // centred inside its half, sat in the upper part of the
                    // drawer with the rest of it empty below.
                    #[watch]
                    set_vexpand: model.queue_shown && !model.wide,

                    #[wrap(Some)]
                    #[name = "queue_compact"]
                    set_child = &gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_vexpand: true,
                    },
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
            cover: Cover::new(ART_LARGE),
            scrubbing: false,
            scrub_gen: 0,
            wide: true,
            queue_shown: false,
            transport: gtk::Box::new(gtk::Orientation::Vertical, 12),
            slots: None,
            bits: None,
            art_px: std::rc::Rc::new(std::cell::Cell::new(ART_LARGE)),
            art_anim: None,
        };
        // Rule 5: no `.expect()` here. A missing handover is a construction
        // order mistake rather than a runtime condition, so it should never
        // happen — but "should never happen" is exactly what the rule is about,
        // and a drawer with an empty queue pane is a far better failure than a
        // player that will not start. It is loud in the log and silent to the
        // user, who cannot act on it either way.
        let queue = QUEUE_SLOT.with(|q| q.borrow().clone()).unwrap_or_else(|| {
            tracing::error!("no queue widget was handed over; the drawer's queue will be empty");
            adw::ToolbarView::new()
        });
        let widgets = view_output!();
        model.cover.attach_first(&widgets.art_slot);
        model.cover.empty_sleeve(ART_LARGE);

        model.bits = Some(build_transport(&model.transport, &sender));

        // The artwork has no widget that will animate a size request for it,
        // so this is the one place the drawer drives a value by hand. The
        // callback is deliberately idempotent — `AdwTimedAnimation` can hand
        // back the same rounded pixel twice on consecutive frames, and
        // re-setting the size would queue a resize for no change.
        let px = model.art_px.clone();
        let cover = model.cover.clone();
        let anim = adw::TimedAnimation::new(
            &widgets.art_slot,
            f64::from(ART_LARGE),
            f64::from(ART_LARGE),
            QUEUE_ANIM_MS,
            adw::CallbackAnimationTarget::new(move |value| {
                let size = value.round() as i32;
                if px.replace(size) != size {
                    cover.resize(size);
                }
            }),
        );
        anim.set_easing(adw::Easing::EaseOutCubic);
        model.art_anim = Some(anim);
        model.slots = Some(Slots {
            queue,
            queue_wide_rev: widgets.queue_wide_rev.clone(),
            queue_compact_rev: widgets.queue_compact_rev.clone(),
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

    /// Which face the metadata shows.
    ///
    /// Here rather than as a `#[watch] set_visible_child_name`, and guarded,
    /// because a stack with a transition is an animation: writing it is asking
    /// for a cross-fade, and a `#[watch]` would ask on every message. Same
    /// rule as the app's animated properties, for the same reason.
    fn post_view(&self, widgets: &mut Self::Widgets) {
        let want = if self.snap.active { "track" } else { "empty" };
        if widgets.meta_stack.visible_child_name().as_deref() != Some(want) {
            widgets.meta_stack.set_visible_child_name(want);
        }
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
                // The same empty case the bar draws, at the drawer's size —
                // a place the artwork goes, rather than a bare glyph adrift in
                // a 260px square.
                None => self.cover.empty_sleeve(self.art_px.get()),
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
            PlayerViewInput::ShuffleClicked => {
                let _ = sender.output(NowPlayingOutput::SetShuffle(!self.snap.shuffle));
            }
            PlayerViewInput::RepeatClicked => {
                let _ = sender.output(NowPlayingOutput::SetRepeat(self.snap.repeat.next()));
            }
            PlayerViewInput::VolumeChanged(v) => {
                // Same guard as the bar's: `set_value` and a drag are
                // indistinguishable to GTK, so a value equal to the one held is
                // the echo of our own write and must not go back out.
                if crate::components::now_playing::volume_is_new(v, self.snap.volume) {
                    self.snap.volume = v;
                    let _ = sender.output(NowPlayingOutput::SetVolume(v));
                }
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
        // a thumbnail once the queue needs the room — and it travels between
        // the two rather than cutting, so it reads as the same picture moving.
        self.resize_cover(if self.stacked() { ART_LARGE } else { ART_THUMB });

        // One control at a time: the transport's button opens the queue, the
        // queue's own header closes it. Two buttons, but never both on screen,
        // which is what keeps it from reading as a duplicate.
        if let Some(bits) = self.bits.as_ref() {
            bits.set_secondary_visible(!self.queue_shown);
        }

        // The revealers decide what is on screen now, so the queue itself
        // stays visible: hiding it would pre-empt the very transition the
        // revealer is there to play, and the close would be a cut.
        slots.queue.set_visible(true);
        slots
            .queue_wide_rev
            .set_reveal_child(self.queue_shown && self.wide);
        slots
            .queue_compact_rev
            .set_reveal_child(self.queue_shown && !self.wide);
    }

    /// Send the artwork to `target`, animating unless there is nothing to
    /// animate with.
    ///
    /// Interrupting is the case that matters — toggling the queue twice
    /// quickly — so the new run starts from the size on screen *now*, which is
    /// what `art_px` holds, rather than from the size it was meant to be.
    fn resize_cover(&self, target: i32) {
        if self.art_px.get() == target {
            return;
        }
        let Some(anim) = self.art_anim.as_ref() else {
            self.art_px.set(target);
            self.cover.resize(target);
            return;
        };
        anim.pause();
        anim.set_value_from(f64::from(self.art_px.get()));
        anim.set_value_to(f64::from(target));
        anim.play();
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
