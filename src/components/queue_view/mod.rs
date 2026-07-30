// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! What's playing next — MusicKit's queue, as a sidebar in the player drawer.
//!
//! **This is a projection** (CLAUDE.md rule 3). MusicKit owns the queue; every
//! act here sends a command and waits for the echo, and nothing on screen is
//! authored locally. The one exception is deliberate and lives in the app: a
//! drag moves the row optimistically, because a drop that springs back while a
//! command is in flight reads as a failure even when it worked.
//!
//! Three things that are not obvious, each of which has cost real time:
//!
//! * **The store holds only what is drawn.** No ids, no indices. Which track
//!   sits at a visible position is [`QueueView::rows`]'s question, rebuilt on
//!   every sync — see `row.rs` for why a row that remembers its own index
//!   eventually moves the wrong track.
//! * **Visible positions are not queue indices.** With the played tracks
//!   collapsed the list starts partway down, and the disclosure occupies a row
//!   of its own. Everything a row reports is a *visible* position, and
//!   [`QueueView::track_at`] is the only thing that turns one into a queue index.
//! * **The list is never rebuilt** (#6). See `reconcile.rs`.

mod menu;
mod reconcile;
mod row;
mod sync;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use relm4::adw::prelude::*;
use relm4::typed_view::list::TypedListView;
use relm4::{Component, ComponentParts, ComponentSender, adw, gtk};

use crate::components::now_playing::{Repeat, mode_opacity};
use reconcile::Key;
use row::{Bound, QueueItem, Shared};

/// One queue entry, flattened from the sidecar's view of MusicKit's queue.
#[derive(Debug, Clone, PartialEq)]
pub struct QueueEntry {
    /// Where this row sits in the queue the projection was built from.
    ///
    /// **An id alone cannot identify a row.** A queue may legitimately hold the
    /// same track twice — Play Next and Add to Queue are exactly what put it
    /// there — so resolving a click by id found the *first* copy and acted on
    /// that one instead (#88). The app resolves the pair against MusicKit's
    /// live queue: the position says which copy, the id says whether the queue
    /// has moved since.
    pub at: usize,
    /// MusicKit's id for this item. Not unique within a queue; see `at`.
    pub id: String,
    /// The catalog id, when Apple gave one — **not** `id`, which falls back to
    /// MusicKit's own when it did not. The two id spaces are not
    /// interchangeable, and this is the only one a catalog lookup accepts, so
    /// "Go to Album" is offered exactly when it is present rather than offered
    /// always and toasting a 404.
    pub catalog_id: Option<String>,
    pub title: String,
    pub artist: String,
    pub duration_ms: u64,
}

/// A row of the visible list — which is not the same list as the queue.
#[derive(Debug, Clone)]
enum Row {
    History { hidden: usize, expanded: bool },
    Track(QueueEntry),
}

impl Row {
    fn key(&self) -> Key {
        match self {
            Self::History { hidden, expanded } => Key::History {
                hidden: *hidden,
                expanded: *expanded,
            },
            Self::Track(entry) => Key::Track {
                id: entry.id.clone(),
                title: entry.title.clone(),
                artist: entry.artist.clone(),
                duration_ms: entry.duration_ms,
            },
        }
    }

    /// The store's copy: what this row draws, and nothing else.
    fn item(&self) -> QueueItem {
        match self {
            Self::History { hidden, expanded } => QueueItem::History {
                hidden: *hidden,
                expanded: *expanded,
            },
            Self::Track(entry) => QueueItem::Track {
                title: entry.title.clone(),
                artist: entry.artist.clone(),
                duration_ms: entry.duration_ms,
            },
        }
    }

    fn queue_index(&self) -> Option<usize> {
        match self {
            Self::Track(entry) => Some(entry.at),
            Self::History { .. } => None,
        }
    }
}

pub struct QueueView {
    list: TypedListView<QueueItem, gtk::NoSelection>,
    /// MusicKit's queue, in its own order. Mirrored, never authored (rule 3).
    entries: Vec<QueueEntry>,
    /// The queue index the player is on, or `None` when nothing is loaded.
    ///
    /// A **position**, not an id, because a queue may hold the same track twice
    /// and an id would mark both copies.
    current: Option<usize>,
    /// Whether the tracks already played are showing.
    ///
    /// Collapsed by default: what a queue is for is what comes next, and a
    /// hundred played tracks above the current one is a hundred rows of scrolling
    /// between opening the drawer and seeing anything useful.
    history_expanded: bool,
    /// What the store is showing, in visible order. The only thing entitled to
    /// answer "which track is at visible position p".
    rows: Vec<Row>,
    /// Mirrored from the player, never authored here (rule 3). Both buttons are
    /// plain, so these only ever *display*: nothing here can report a change
    /// back and there is no binding to break.
    shuffle: bool,
    repeat: Repeat,
    /// The visible position of the playing row, shared with every row so the
    /// marker can move without the store being touched.
    playing: Rc<Cell<Option<u32>>>,
    /// Mirrors `!history_expanded` for the drop handlers, which are connected
    /// once and cannot be told at connect time.
    collapsed: Rc<Cell<bool>>,
    /// Which track is being dragged, remembered when the drag begins.
    ///
    /// A drag lasts seconds and the list moves underneath it — a track boundary
    /// alone renumbers every row once the played one folds away — so the
    /// dragged row is identified once, up front, and resolved again at drop.
    /// A cancelled drag leaves this set; the next drag overwrites it, and the
    /// only thing that reads it is a drop, which a drag always precedes.
    dragging: Option<(usize, String)>,
    bound: Bound,
}

#[derive(Debug)]
pub enum QueueViewInput {
    Sync {
        entries: Vec<QueueEntry>,
        /// The queue index the player is on.
        current: Option<usize>,
    },
    /// Bring the current track into view — on open, not on every update, or it
    /// would fight the user scrolling.
    ScrollToPlaying,
    /// A row was activated. A **visible** position, resolved immediately.
    Activated(u32),
    RemoveAt(u32),
    /// A drag began on a row. Carries a **visible** position, which is resolved
    /// to a track immediately — see [`QueueView::dragging`].
    DragStarted(u32),
    /// A row was dropped onto another. `below` says which half of it, so the
    /// drop lands where the line was drawn rather than onto a row. What was
    /// dragged is not in here; it was remembered when the drag began.
    Dropped {
        over: u32,
        below: bool,
    },
    /// Move a row that is **already in the queue** to just after the current
    /// track. Not an insert; see [`reconcile::play_next_index`].
    ///
    /// Carries the track's identity rather than its position, because it comes
    /// from a popover that waited on a person. See `menu::show`.
    PlayNext {
        at: usize,
        id: String,
    },
    /// As [`QueueViewInput::RemoveAt`], from the menu rather than the button.
    RemoveTrack {
        at: usize,
        id: String,
    },
    /// Open the album or artist the row's track belongs to.
    GoTo {
        id: String,
        album: bool,
    },
    Menu {
        at: u32,
        over: gtk::Widget,
        x: f64,
        y: f64,
    },
    ToggleHistory,
    /// Shuffle and repeat as the player currently has them.
    SetModes {
        shuffle: bool,
        repeat: Repeat,
    },
    /// No payload — the next value is derived from the mirrored one, so this
    /// view never invents one (rule 3).
    ShuffleClicked,
    RepeatClicked,
}

#[derive(Debug)]
pub enum QueueViewOutput {
    /// Which row to act on: where it sat, and what it was. See [`QueueEntry::at`].
    Jump {
        at: usize,
        id: String,
    },
    Remove {
        at: usize,
        id: String,
    },
    /// Empty the queue and stop.
    Clear,
    /// Reorder the queue MusicKit holds. `to` is the final index.
    Move {
        from: usize,
        to: usize,
    },
    /// Shuffle and repeat live here because they are properties of the queue,
    /// not of the transport. The player still owns the values (rule 3); these
    /// are requests.
    SetShuffle(bool),
    SetRepeat(Repeat),
    /// Open the album or artist a queue track belongs to, by the track's own
    /// catalog id — a queue item carries no album or artist id of its own, so
    /// the app has to ask Apple which ones they are.
    GoToAlbum(String),
    GoToArtist(String),
}

#[relm4::component(pub)]
impl Component for QueueView {
    type Init = ();
    type Input = QueueViewInput;
    type Output = QueueViewOutput;
    type CommandOutput = ();

    view! {
        adw::ToolbarView {

            add_top_bar = &adw::HeaderBar {
                // No window controls: inside the drawer they would be a second
                // close button laid over the real one.
                set_show_end_title_buttons: false,

                #[wrap(Some)]
                set_title_widget = &adw::WindowTitle {
                    set_title: "Queue",
                    #[watch]
                    set_subtitle: &match model.entries.len() {
                        0 => String::new(),
                        1 => "1 track".to_owned(),
                        n => format!("{n} tracks"),
                    },
                },

                // Only when there is something to clear. A destructive action
                // that does nothing is worse than no button.
                pack_start = &gtk::Button {
                    set_icon_name: "user-trash-symbolic",
                    set_tooltip_text: Some("Clear the queue"),
                    add_css_class: "flat",
                    #[watch]
                    set_visible: !model.entries.is_empty(),
                    connect_clicked[sender] => move |_| {
                        let _ = sender.output(QueueViewOutput::Clear);
                    },
                },

                // Packed end-first, so left to right these read shuffle,
                // repeat. Both plain buttons weighted by `mode_opacity` rather
                // than filled — the same reading as the bar and the drawer.
                //
                // That is not only a look. A `GtkToggleButton` here would be a
                // control that both reports *and* displays state in a component
                // that does not own the value, which is the shape that froze a
                // desktop: relm4 re-runs the view after every message and GTK
                // reports a programmatic `set_active` identically to a click. A
                // button only ever reports clicks, so there is nothing to echo
                // and no guard to get wrong.
                pack_end = &gtk::Button {
                    set_icon_name: "media-playlist-shuffle-symbolic",
                    set_tooltip_text: Some("Shuffle"),
                    add_css_class: "flat",
                    #[watch]
                    set_opacity: mode_opacity(model.shuffle),
                    connect_clicked[sender] => move |_| {
                        sender.input(QueueViewInput::ShuffleClicked);
                    },
                },

                pack_end = &gtk::Button {
                    add_css_class: "flat",
                    set_tooltip_text: Some("Repeat"),
                    #[watch]
                    set_icon_name: match model.repeat {
                        Repeat::One => "media-playlist-repeat-song-symbolic",
                        _ => "media-playlist-repeat-symbolic",
                    },
                    #[watch]
                    set_opacity: mode_opacity(!matches!(model.repeat, Repeat::Off)),
                    connect_clicked[sender] => move |_| {
                        sender.input(QueueViewInput::RepeatClicked);
                    },
                },
            },

            // A `GtkBox` with one child hidden, **not** a `GtkStack`. A stack
            // measures its hidden children on both axes, so `AdwStatusPage`'s
            // minimum width would become the whole pane's minimum width — and
            // the pane has to fit 206px, which is what a 460px window gives it.
            #[wrap(Some)]
            set_content = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,

                // An empty queue is a state, not an empty list: a blank panel
                // reads as broken. In its own scroller so a narrow pane can
                // scroll the status page rather than be widened by it.
                gtk::ScrolledWindow {
                    set_vexpand: true,
                    set_hscrollbar_policy: gtk::PolicyType::Never,
                    #[watch]
                    set_visible: model.entries.is_empty(),

                    adw::StatusPage {
                        set_icon_name: Some("view-list-symbolic"),
                        set_title: "Nothing queued",
                        set_description: Some("Play something and it will show up here."),
                    },
                },

                gtk::ScrolledWindow {
                    set_vexpand: true,
                    #[watch]
                    set_visible: !model.entries.is_empty(),
                    // **Load-bearing.** On the default policy the scroller
                    // allocates the list its *natural* width — the width of the
                    // longest title — and grows a horizontal scrollbar instead
                    // of letting the labels ellipsize, which is what made rows
                    // render wrongly in a narrow drawer.
                    set_hscrollbar_policy: gtk::PolicyType::Never,

                    #[local_ref]
                    queue_list -> gtk::ListView {
                        set_single_click_activate: true,
                        add_css_class: "navigation-sidebar",
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
        let list: TypedListView<QueueItem, gtk::NoSelection> = TypedListView::new();

        let playing = Rc::new(Cell::new(None));
        let collapsed = Rc::new(Cell::new(true));
        let bound: Bound = Rc::new(RefCell::new(Vec::new()));
        // Rows are built by GTK's factory, which has no component behind it.
        // This is what they reach for instead — see `row::Shared`.
        row::install(Rc::new(Shared {
            sender: sender.input_sender().clone(),
            playing: playing.clone(),
            collapsed: collapsed.clone(),
            bound: bound.clone(),
        }));

        let activate = sender.clone();
        list.view.connect_activate(move |_, position| {
            activate.input(QueueViewInput::Activated(position));
        });

        let model = QueueView {
            list,
            entries: Vec::new(),
            current: None,
            history_expanded: false,
            rows: Vec::new(),
            shuffle: false,
            repeat: Repeat::default(),
            playing,
            collapsed,
            dragging: None,
            bound,
        };
        let queue_list = &model.list.view;
        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        msg: Self::Input,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match msg {
            QueueViewInput::Sync { entries, current } => {
                // No scroll to save and none to restore. See `reconcile.rs` and
                // `components::drop_focus` for why that is now true.
                if entries != self.entries || current != self.current {
                    // A different queue is a different history, and an opened
                    // disclosure belongs to the queue it was opened on — left
                    // alone it unfolds the next album's first track the moment
                    // the second one starts.
                    if self.history_expanded
                        && !reconcile::shares_a_track(
                            self.entries.iter().map(|e| e.id.as_str()),
                            entries.iter().map(|e| e.id.as_str()),
                        )
                    {
                        self.history_expanded = false;
                    }
                    self.entries = entries;
                    self.current = current;
                    self.refresh();
                }
            }
            QueueViewInput::ToggleHistory => {
                self.history_expanded = !self.history_expanded;
                self.refresh();
            }
            QueueViewInput::SetModes { shuffle, repeat } => {
                self.shuffle = shuffle;
                self.repeat = repeat;
            }
            QueueViewInput::ShuffleClicked => {
                let _ = sender.output(QueueViewOutput::SetShuffle(!self.shuffle));
            }
            QueueViewInput::RepeatClicked => {
                let _ = sender.output(QueueViewOutput::SetRepeat(self.repeat.next()));
            }
            QueueViewInput::ScrollToPlaying => {
                if let Some(position) = self.playing.get() {
                    widgets
                        .queue_list
                        .scroll_to(position, gtk::ListScrollFlags::NONE, None);
                }
            }
            QueueViewInput::Activated(position) => match self.rows.get(position as usize) {
                // Activating the disclosure is what opens it: it is a row, so
                // clicking it is how anyone will try.
                Some(Row::History { .. }) => sender.input(QueueViewInput::ToggleHistory),
                Some(Row::Track(entry)) => {
                    let _ = sender.output(QueueViewOutput::Jump {
                        at: entry.at,
                        id: entry.id.clone(),
                    });
                }
                None => {}
            },
            QueueViewInput::RemoveAt(position) => {
                if let Some(entry) = self.track_at(position) {
                    let _ = sender.output(QueueViewOutput::Remove {
                        at: entry.at,
                        id: entry.id.clone(),
                    });
                }
            }
            QueueViewInput::DragStarted(position) => {
                self.dragging = self.track_at(position).map(|e| (e.at, e.id.clone()));
            }
            QueueViewInput::Dropped { over, below } => {
                // Both ends resolved to queue indices before the arithmetic:
                // the visible list is not the queue when the history is
                // collapsed, and mixing the two is off-by-`hidden`.
                let Some((at, id)) = self.dragging.take() else {
                    return;
                };
                let Some(from) = self.entry_index(at, &id) else {
                    return; // it left the queue mid-drag
                };
                let Some(over) = self.track_at(over).map(|e| e.at) else {
                    return; // dropped on the disclosure
                };
                let mut to = reconcile::drop_index(from, over, below);
                // A drop above the current track files the row into the past,
                // which the collapsed history then hides — so the drag would
                // read as a delete. `drop_below` keeps the line honest; this
                // refuses the move the line cannot promise.
                if !self.history_expanded
                    && let Some(current) = self.current
                {
                    to = to.max(reconcile::earliest_visible_slot(from, current));
                }
                if to != from {
                    let _ = sender.output(QueueViewOutput::Move { from, to });
                }
            }
            QueueViewInput::PlayNext { at, id } => {
                let Some(from) = self.entry_index(at, &id) else {
                    return;
                };
                let Some(current) = self.current else { return };
                let to = reconcile::play_next_index(from, current);
                if to != from {
                    let _ = sender.output(QueueViewOutput::Move { from, to });
                }
            }
            QueueViewInput::RemoveTrack { at, id } => {
                // Re-resolved rather than passed through: the app checks the
                // pair against MusicKit's live queue anyway, but handing it a
                // position that is already known to be stale is one more thing
                // for the fallback to have to rescue.
                let at = self.entry_index(at, &id).unwrap_or(at);
                let _ = sender.output(QueueViewOutput::Remove { at, id });
            }
            QueueViewInput::GoTo { id, album } => {
                let _ = sender.output(if album {
                    QueueViewOutput::GoToAlbum(id)
                } else {
                    QueueViewOutput::GoToArtist(id)
                });
            }
            QueueViewInput::Menu { at, over, x, y } => {
                let Some(entry) = self.track_at(at) else {
                    return;
                };
                let (at, id) = (entry.at, entry.id.clone());
                let catalog_id = entry.catalog_id.clone();
                // The playing track is neither: its own remove button is
                // insensitive, and "play next" for what is already playing
                // means nothing.
                let removable = self.current != Some(at);
                let movable = removable
                    && self
                        .current
                        .is_some_and(|c| reconcile::play_next_index(at, c) != at);
                menu::show(
                    sender.input_sender(),
                    menu::Target {
                        at,
                        id: &id,
                        catalog_id: catalog_id.as_deref(),
                        movable,
                        removable,
                    },
                    &over,
                    x,
                    y,
                );
            }
        }
        self.update_view(widgets, sender);
    }
}
