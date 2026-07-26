// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Widgets. Nothing in here talks to the sidecar or to `reqwest` directly —
//! components receive plain data from `app.rs` and emit intent back (rule 9).

pub mod artwork;
pub mod cover;
pub mod detail_page;
pub mod grid_item;
pub mod now_playing;
pub mod queue_view;
pub mod track_row;

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

/// Count a recycled widget being built, and say so at `trace` level.
///
/// `setup` runs once per *widget*, not once per item, so this is the direct
/// measurement of whether a view is virtualised: scroll a 500-item list and
/// watch where the count stops. A few dozen means recycling; 500 means every
/// row is real and something upstream is asking the view for its full height.
///
/// `RUST_LOG=tonearm=trace` to see it.
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
