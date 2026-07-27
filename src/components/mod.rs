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

/// Put a list's scroll position back after `GtkListView` throws it away.
///
/// **Measured, after five attempts that were not** (issue #6). Across a
/// single-row removal the adjustment is fine at first — `value` unchanged, and
/// `upper` shrunk by exactly one row as it should — and then, somewhere between
/// 50ms and 100ms, `ListView` assigns `value = 0.0` while `upper` is still
/// healthy, and leaves it there:
///
/// ```text
/// before   value=26000  upper=29535  page=556
/// +50ms    value=26000  upper=29480
/// +100ms   value=0.0    upper=29480
/// ```
///
/// So there is no clamp, whatever #6 assumed. Nothing forces the value down; it
/// is overwritten. That is why restoring it in an idle, or once `upper` settled,
/// or calling `scroll_to` one tick after the edit all failed — every one of them
/// ran *before* the assignment and corrected a value that was still correct.
///
/// Hence: watch for the assignment rather than race it. On the first collapse to
/// zero *while there is still room above*, re-anchor and stop listening.
///
/// Two details that both matter:
///
/// * **`scroll_to`, not `set_value`.** Restoring the pixel offset alone moves the
///   viewport while `ListView`'s anchor stays on item 0, so it has materialised
///   no rows near the new position and the list renders blank until nudged.
/// * **Bounded.** The listener disconnects after 500ms either way, so a genuine
///   scroll to the top later is never fought over.
pub fn restore_scroll_after_edit(
    view: &relm4::gtk::ListView,
    adjustment: &relm4::gtk::Adjustment,
    item: u32,
) {
    use relm4::gtk::prelude::*;

    let was_at = adjustment.value();
    // Already at the top: nothing to protect, and no way to tell a real jump
    // from the bug.
    if was_at <= adjustment.page_size() {
        return;
    }

    // Assert it *now* as well as reactively. Setting the anchor before GTK
    // reassigns the value sometimes prevents the collapse outright, and when it
    // does the position never visibly moves — where undoing it afterwards is
    // always a visible flicker to the top and back.
    view.scroll_to(item, relm4::gtk::ListScrollFlags::NONE, None);

    // Then keep watching. Measured (#6): a single reactive attempt is not
    // enough — of three consecutive removals, two restored and one was left at
    // zero, because the assignment can land again after `scroll_to`. So this
    // re-asserts on every collapse rather than disconnecting after the first.
    let handler = std::rc::Rc::new(std::cell::RefCell::new(None));
    let view = view.clone();
    let id = adjustment.connect_value_changed(move |adj| {
        // Only the collapse, and only while it is unjustified.
        if adj.value() > 1.0 || was_at > adj.upper() - adj.page_size() {
            return;
        }
        // **Deferred out of the signal handler.** Calling `scroll_to` from
        // inside `value-changed` runs it in the middle of GTK's own adjustment
        // update: the viewport moves but no re-layout is scheduled, so the list
        // renders a single row and only fills in when something else — a hover,
        // a resize — forces it. Landing the call on an idle instead lets GTK
        // finish, then scroll properly.
        let view = view.clone();
        relm4::gtk::glib::idle_add_local_once(move || {
            view.scroll_to(item, relm4::gtk::ListScrollFlags::NONE, None);
        });
    });
    *handler.borrow_mut() = Some(id);

    // Bounded, so a genuine scroll to the top a moment later is never fought.
    let adj = adjustment.clone();
    relm4::gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(500), move || {
        if let Some(id) = handler.borrow_mut().take() {
            adj.disconnect(id);
        }
    });
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
