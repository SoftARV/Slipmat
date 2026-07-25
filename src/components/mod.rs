// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Widgets. Nothing in here talks to the sidecar or to `reqwest` directly —
//! components receive plain data from `app.rs` and emit intent back (rule 9).

pub mod artwork;
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
