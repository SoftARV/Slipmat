// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Keeping the visible list in step with the queue.
//!
//! A sibling rather than more of `mod.rs`, which keeps the three things that
//! have to be in one place: the model, the messages, and the `Component` impl
//! holding `view!` and the reducer.
//!
//! What lives here is the one concern between them — **the visible list is not
//! the queue**. It gains a disclosure row, it loses everything before the
//! current track while that disclosure is folded, and it is the only thing
//! entitled to say which track sits at a given visible position. Every
//! translation between the two happens in this file.

use super::reconcile::{self, Key, Plan};
use super::{QueueEntry, QueueView, Row, row};

impl QueueView {
    /// The track at a **visible** position, if that row is a track at all.
    pub(super) fn track_at(&self, position: u32) -> Option<&QueueEntry> {
        match self.rows.get(position as usize)? {
            Row::Track(entry) => Some(entry),
            Row::History { .. } => None,
        }
    }
    /// Where a remembered row is in the queue **now**.
    ///
    /// The position is the key and the id is the check, exactly as
    /// `app::queue::index_at` does it against MusicKit's own queue: a queue may
    /// hold the same track twice (#88), so an id alone finds the wrong copy —
    /// and if the queue moved since, searching by id is the better wrong answer.
    ///
    /// This is what anything that waited on a person has to go through: a
    /// popover, or a drag.
    pub(super) fn entry_index(&self, at: usize, id: &str) -> Option<usize> {
        match self.entries.get(at) {
            Some(entry) if entry.id == id => Some(at),
            _ => self.entries.iter().position(|entry| entry.id == id),
        }
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
    pub(super) fn refresh(&mut self) {
        let was: Vec<Key> = self.rows.iter().map(Row::key).collect();
        let rows = self.visible_rows();
        let now: Vec<Key> = rows.iter().map(Row::key).collect();
        let plan = reconcile::plan(&was, &now);
        self.rows = rows;
        self.collapsed.set(!self.history_expanded);
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
        //
        // **Only when that row is in the range being edited.** Rows after an
        // edit are renumbered but not removed, and those keep the scroll on
        // their own — so dropping focus for them would cost a keyboard user
        // their place once per track boundary, the queue folding away a played
        // track being a structural edit like any other.
        let edited = match plan {
            Plan::Unchanged => 0..0,
            Plan::Moved { from, .. } => from..from + 1,
            Plan::Spliced { at, remove, .. } => at..at + remove,
        };
        if row::focused_row(&self.list.view, &self.bound)
            .is_some_and(|position| edited.contains(&(position as usize)))
        {
            crate::components::drop_focus(&self.list.view);
        }
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
