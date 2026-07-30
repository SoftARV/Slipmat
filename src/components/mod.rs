// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Widgets. Nothing in here talks to the sidecar or to `reqwest` directly —
//! components receive plain data from `app.rs` and emit intent back (rule 9).

pub mod artwork;
pub mod cover;
pub mod detail_page;
pub mod grid_item;
pub mod mosaic;
pub mod now_playing;
pub mod player_view;
pub mod prune;
pub mod queue_view;
pub mod track_row;

/// Take keyboard focus out of a list before its model is edited.
///
/// **This is issue #6, and its cause rather than its symptom.** `GtkListView`
/// throws the scroll position away when the row holding keyboard focus is the
/// one removed or moved — and clicking a row, or starting a drag on it, is what
/// gives it focus. Measured on GTK 4.22, driving identical edits with the
/// viewport parked 200 rows down:
///
/// ```text
///                              value          the row on screen
/// remove a row, nothing focused  9553 ->  9505   unchanged
/// remove a focused row           9553 ->     0   lost, ~50ms later
/// remove a focused row, focus dropped first
///                                9553 ->  9360   unchanged
/// ```
///
/// The delay is what made this so hard to see: for the first frame the
/// adjustment is *correct*, so every attempt to restore the value ran before the
/// collapse and corrected a number that was still right. And it is why
/// `set_focus_on_click(false)` on a row's own buttons never helped — the focus
/// that matters belongs to the `GtkListItemWidget`, which is GTK's, not ours.
///
/// The row is not re-focused afterwards. Restoring focus to a row is the very
/// thing that loses the position, and the act that got here was a click or a
/// drop, so the pointer is where the user's attention is.
pub fn drop_focus(view: &relm4::gtk::ListView) {
    use relm4::gtk::prelude::*;

    let Some(window) = view.root().and_downcast::<relm4::gtk::Window>() else {
        return;
    };
    let Some(focused) = relm4::gtk::prelude::GtkWindowExt::focus(&window) else {
        return;
    };
    // Only when it is ours to drop: clearing the window's focus because some
    // other list was edited would steal it from whatever the user is typing in.
    let list: relm4::gtk::Widget = view.clone().upcast();
    if focused == list || focused.is_ancestor(&list) {
        relm4::gtk::prelude::GtkWindowExt::set_focus(&window, None::<&relm4::gtk::Widget>);
    }
}

/// Widgets that are **currently bound and on screen**, keyed by track id.
///
/// `ListView` recycles rows, so most items have no widget at any given moment.
/// Moving a play marker by editing the model works, but `items-changed` makes
/// the list re-measure and the scroll position jumps — which is unacceptable
/// for something that happens on every track change.
///
/// So the marker is applied two ways instead: the item's data is updated
/// silently, so a future re-bind is correct, and the widget is updated directly
/// if it happens to be on screen. Neither touches the model.
pub type RowRegistry<W> = std::rc::Rc<std::cell::RefCell<std::collections::HashMap<String, W>>>;

pub fn row_registry<W>() -> RowRegistry<W> {
    std::rc::Rc::new(std::cell::RefCell::new(std::collections::HashMap::new()))
}

/// The id of the track currently playing, shared with every row.
///
/// Rows read this in `bind` rather than carrying a `playing` flag of their own.
/// Carrying the flag means changing it in the model, and **any** model edit
/// makes `ListView` re-measure and lose the scroll position — even replacing a
/// single item. Since the marker moves on every track change, that made both
/// lists jump to the top roughly once a minute.
pub type CurrentTrack = std::rc::Rc<std::cell::RefCell<Option<String>>>;

pub fn current_track() -> CurrentTrack {
    std::rc::Rc::new(std::cell::RefCell::new(None))
}

/// Catalog ids MusicKit has refused, shared with every row.
///
/// Same reasoning as [`CurrentTrack`]: discovering a track is unplayable
/// happens mid-session, and rebuilding the list to reflect it costs the scroll
/// position. Rows consult this at `bind` instead of trusting a flag baked into
/// their copy of the track.
pub type DeadTracks = std::rc::Rc<std::cell::RefCell<std::collections::HashSet<String>>>;

pub fn dead_tracks() -> DeadTracks {
    std::rc::Rc::new(std::cell::RefCell::new(std::collections::HashSet::new()))
}

/// What has changed about a track since it was fetched, shared with every row.
///
/// Third of its kind, and for the third time the same reason: a row must not
/// carry state that can change while it is off screen. `CurrentTrack` and
/// `DeadTracks` already work this way; favourites and library membership did
/// not, and paid for it three times in one evening — the star cleared while the
/// menu still offered to un-star, "Add to Library" kept being offered for a
/// song already added, and un-favouriting came undone the moment the row
/// scrolled out and back.
///
/// Each of those was one copy of the truth left behind. `Track` is what Apple
/// said when we asked; this is what has happened since; the row combines them
/// at bind time. Nothing else needs updating, which is the point — there is no
/// list store clone to patch, because the clone is no longer authoritative.
///
/// Cleared when a section reloads: fresh data supersedes anything remembered
/// here.
pub type TrackOverrides =
    std::rc::Rc<std::cell::RefCell<std::collections::HashMap<String, TrackOverride>>>;

/// `None` on a field means "nothing has happened to it" — which is different
/// from `Some(false)`, and is why these are options rather than bools.
#[derive(Debug, Clone, Copy, Default)]
pub struct TrackOverride {
    pub favorite: Option<bool>,
    pub in_library: Option<bool>,
}

pub fn track_overrides() -> TrackOverrides {
    std::rc::Rc::new(std::cell::RefCell::new(std::collections::HashMap::new()))
}

/// Apply whatever has happened to a track on top of what was fetched.
pub fn overridden(
    overrides: &TrackOverrides,
    catalog_id: Option<&str>,
    fetched_favorite: bool,
    fetched_in_library: bool,
) -> (bool, bool) {
    let Some(id) = catalog_id else {
        return (fetched_favorite, fetched_in_library);
    };
    let map = overrides.borrow();
    let Some(over) = map.get(id) else {
        return (fetched_favorite, fetched_in_library);
    };
    (
        over.favorite.unwrap_or(fetched_favorite),
        over.in_library.unwrap_or(fetched_in_library),
    )
}

/// Count a recycled widget being built, and say so at `trace` level.
///
/// `setup` runs once per *widget*, not once per item, so this is the direct
/// measurement of whether a view is virtualised: scroll a 500-item list and
/// watch where the count stops. A few dozen means recycling; 500 means every
/// row is real and something upstream is asking the view for its full height.
///
/// `RUST_LOG=slipmat=trace` to see it.
pub fn count_widget(kind: &'static str) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    // One counter per kind, kept in a small table rather than a static per
    // call site — there are three of these and there will not be thirty.
    static COUNTS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<&'static str, AtomicUsize>>,
    > = std::sync::OnceLock::new();
    let table = COUNTS.get_or_init(Default::default);
    let Ok(mut table) = table.lock() else { return };
    let count = table
        .entry(kind)
        .or_default()
        .fetch_add(1, Ordering::Relaxed)
        + 1;
    tracing::trace!(kind, widgets = count, "list widget built");
}
