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

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use relm4::adw::prelude::*;
use relm4::typed_view::list::TypedListView;
use relm4::{Component, ComponentParts, ComponentSender, adw, gtk};

use crate::components::now_playing::{Repeat, mode_opacity};
use reconcile::{Key, Plan};
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
            Self::Track(entry) => Key::Track(entry.id.clone()),
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
    /// A row was dropped onto another. `below` says which half of it, so the
    /// drop lands where the line was drawn rather than onto a row.
    Dropped {
        from: u32,
        over: u32,
        below: bool,
    },
    /// Move a row that is **already in the queue** to just after the current
    /// track. Not an insert; see [`reconcile::play_next_index`].
    PlayNextAt(u32),
    /// Open the album or artist the row's track belongs to.
    GoTo {
        at: u32,
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
        let bound: Bound = Rc::new(RefCell::new(Vec::new()));
        // Rows are built by GTK's factory, which has no component behind it.
        // This is what they reach for instead — see `row::Shared`.
        row::install(Rc::new(Shared {
            sender: sender.input_sender().clone(),
            playing: playing.clone(),
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
            QueueViewInput::Dropped { from, over, below } => {
                // Both ends resolved to queue indices before the arithmetic:
                // the visible list is not the queue when the history is
                // collapsed, and mixing the two is off-by-`hidden`.
                let Some(from) = self.track_at(from).map(|e| e.at) else {
                    return;
                };
                let Some(over) = self.track_at(over).map(|e| e.at) else {
                    return; // dropped on the disclosure
                };
                let to = reconcile::drop_index(from, over, below);
                if to != from {
                    let _ = sender.output(QueueViewOutput::Move { from, to });
                }
            }
            QueueViewInput::PlayNextAt(position) => {
                let Some(from) = self.track_at(position).map(|e| e.at) else {
                    return;
                };
                let Some(current) = self.current else { return };
                let to = reconcile::play_next_index(from, current);
                if to != from {
                    let _ = sender.output(QueueViewOutput::Move { from, to });
                }
            }
            QueueViewInput::GoTo { at, album } => {
                if let Some(id) = self.track_at(at).map(|e| e.id.clone()) {
                    let _ = sender.output(if album {
                        QueueViewOutput::GoToAlbum(id)
                    } else {
                        QueueViewOutput::GoToArtist(id)
                    });
                }
            }
            QueueViewInput::Menu { at, over, x, y } => {
                menu::show(sender.input_sender(), at, &over, x, y, self.movable(at));
            }
        }
        self.update_view(widgets, sender);
    }
}

impl QueueView {
    /// The track at a **visible** position, if that row is a track at all.
    fn track_at(&self, position: u32) -> Option<&QueueEntry> {
        match self.rows.get(position as usize)? {
            Row::Track(entry) => Some(entry),
            Row::History { .. } => None,
        }
    }

    /// Whether "Play Next" would do anything for this row. It would not for the
    /// track that is playing, nor for the one already next.
    fn movable(&self, position: u32) -> bool {
        let (Some(from), Some(current)) = (self.track_at(position).map(|e| e.at), self.current)
        else {
            return false;
        };
        from != current && reconcile::play_next_index(from, current) != from
    }

    /// The list as it should be, given the queue and whether history is showing.
    fn visible_rows(&self) -> Vec<Row> {
        let hidden = self.current.unwrap_or(0);
        let mut rows = Vec::with_capacity(self.entries.len() + 1);
        if hidden > 0 {
            rows.push(Row::History {
                hidden,
                expanded: self.history_expanded,
            });
        }
        let from = if self.history_expanded { 0 } else { hidden };
        rows.extend(
            self.entries
                .get(from..)
                .unwrap_or_default()
                .iter()
                .cloned()
                .map(Row::Track),
        );
        rows
    }

    /// Bring the rows in line with the queue, and the markers with the rows.
    fn refresh(&mut self) {
        let was: Vec<Key> = self.rows.iter().map(Row::key).collect();
        let rows = self.visible_rows();
        let now: Vec<Key> = rows.iter().map(Row::key).collect();
        let plan = reconcile::plan(&was, &now);
        self.rows = rows;

        self.playing.set(
            self.current
                .and_then(|at| self.rows.iter().position(|r| r.queue_index() == Some(at)))
                .map(|position| position as u32),
        );

        self.apply(plan);
        // **After the edit, unconditionally.** A move renumbers every row
        // between its ends without re-binding one of them, so their contents are
        // right and their markers are stale.
        row::repaint(&self.bound, self.playing.get());
    }

    /// Perform one plan against the store.
    fn apply(&mut self, plan: Plan) {
        if matches!(plan, Plan::Unchanged) {
            return;
        }
        // **The fix for #6, and the only one needed.** `GtkListView` throws the
        // scroll position away when the row holding keyboard focus is the one
        // being removed or moved — and clicking a row, or starting a drag on it,
        // is what gives it focus. Measured: identical edits keep the position
        // exactly when no row in the list is focused.
        crate::components::drop_focus(&self.list.view);

        match plan {
            Plan::Unchanged => {}
            Plan::Moved { from, to } => {
                tracing::debug!(from, to, "queue: moving one row");
                if let Some(item) = self.rows.get(to).map(Row::item) {
                    self.list.remove(from as u32);
                    self.list.insert(to as u32, item);
                }
            }
            Plan::Spliced { at, remove, insert } => {
                tracing::debug!(at, remove, insert, "queue: splicing rows");
                for _ in 0..remove {
                    self.list.remove(at as u32);
                }
                for offset in 0..insert {
                    if let Some(item) = self.rows.get(at + offset).map(Row::item) {
                        self.list.insert((at + offset) as u32, item);
                    }
                }
            }
        }
    }
}
