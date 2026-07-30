// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! What a queue change costs in store edits, and the arithmetic a drag needs.
//!
//! Separate from the widgets because this is the half that can be tested
//! without a display — and because it is where issue #6 is actually won.
//!
//! **A rebuild always loses the scroll position. Every other edit keeps it.**
//! Measured on GTK 4.22, driving each edit programmatically with the viewport
//! parked 200 rows down and asking which row was on screen afterwards:
//!
//! ```text
//! remove one row        value 9553 ->  9505   one row's height    kept
//! move one row          value 9553 ->  9553   unchanged           kept
//! insert 30 at the head value 9553 -> 10993   30 rows' height     kept
//! remove 30 at the head value 9553 ->  8113   30 rows' height     kept
//! clear, then refill    value 9553 ->     0                       LOST
//! ```
//!
//! So [`plan`] exists to express *any* queue change as edits to the rows that
//! actually differ. Falling back to a refill is not a slower correct answer, it
//! is the bug. That is why the general case here is a splice over the changed
//! range rather than a rebuild: the ends of a queue are almost always untouched,
//! and trimming them is what keeps the edit small.
//!
//! The other half of #6 is not here, because it is not arithmetic: `GtkListView`
//! resets the scroll when the row holding **keyboard focus** is the one removed
//! or moved. See `components::drop_focus`.

/// The identity of a visible row, for diffing.
///
/// **Deliberately carries no position.** Keying a row by where it sits means a
/// move renumbers every row after it, so two lists that differ by one drag look
/// as though they differ everywhere — and the diff falls back to replacing the
/// lot. An id compares equal wherever it has moved to.
///
/// Ids are *not* unique: a queue may hold the same track twice, which is what
/// Play Next and Add to Queue are for (#88). That is fine here because the diff
/// is positional — a duplicate pair is only ever compared against whatever sits
/// at the same index in the other list.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Key {
    /// The history disclosure.
    ///
    /// Both fields are part of the identity so that a change to either — one
    /// more track played, or the user opening it — re-binds the row. It always
    /// sits at position 0, so the resulting splice is at the head, which is the
    /// cheapest place a splice can be.
    History {
        hidden: usize,
        expanded: bool,
    },
    Track(String),
}

/// The smallest set of store edits that turns one visible list into another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    /// Nothing structural. The marker may still have moved — that is repainted
    /// on the widgets, never through the store.
    Unchanged,
    /// One row went somewhere else. `to` is its index **after** the move, which
    /// is what a drop means and what `Array.splice` does.
    Moved { from: usize, to: usize },
    /// Replace `remove` rows at `at` with `insert` fresh ones.
    ///
    /// The general case, and always correct: a removal is `insert: 0`, an
    /// insertion is `remove: 0`, and opening the history is a splice at the
    /// head. Only the rows between the common prefix and the common suffix are
    /// touched.
    Spliced {
        at: usize,
        remove: usize,
        insert: usize,
    },
}

pub fn plan(old: &[Key], new: &[Key]) -> Plan {
    if old == new {
        return Plan::Unchanged;
    }

    // A single drag is the one change a splice describes badly: dropping a row
    // from the top of a 500-track queue to the bottom differs at every index in
    // between, so the splice would be the whole list. Check for it first.
    if old.len() == new.len()
        && let Some((from, to)) = single_move(old, new)
    {
        return Plan::Moved { from, to };
    }

    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    Plan::Spliced {
        at: prefix,
        remove: old.len() - prefix - suffix,
        insert: new.len() - prefix - suffix,
    }
}

/// The one item that moved, if a single move is what happened.
///
/// Reconstruct the move and check it explains the *whole* difference, rather
/// than trusting the first and last changed index — a swap across a gap changes
/// exactly two indices too, and is not one move.
fn single_move<T: Clone + PartialEq>(old: &[T], new: &[T]) -> Option<(usize, usize)> {
    if old.len() != new.len() || old == new {
        return None;
    }
    let first = old.iter().zip(new).position(|(a, b)| a != b)?;
    let last = old.iter().zip(new).rposition(|(a, b)| a != b)?;

    // Only two candidates: the item at the top of the changed range moved to
    // the bottom of it, or the other way round.
    for (from, to) in [(first, last), (last, first)] {
        let mut moved = old.to_vec();
        let item = moved.remove(from);
        moved.insert(to, item);
        if moved == new {
            return Some((from, to));
        }
    }
    None
}

/// Where a dragged row lands, in queue indices.
///
/// `over` is the row it was dropped on and `below` says which half of it the
/// pointer was in, so a drop reads as "between these two rows" rather than
/// "onto this one" — which is what the drop line drawn during the drag promises.
///
/// The subtraction is the whole of it: the insertion point is counted in the
/// queue as it stands, but the item is taken out *first*, so everything after
/// it slides up one. Getting this wrong is off-by-one in one direction only,
/// which is why dragging used to work downwards and not upwards.
pub fn drop_index(from: usize, over: usize, below: bool) -> usize {
    let insert_at = over + usize::from(below);
    insert_at - usize::from(from < insert_at)
}

/// The earliest index a dragged row may land on and still be seen.
///
/// **A row dropped above the current track files itself into the past**, and
/// while the history is collapsed the past is exactly what is hidden — so the
/// drag reads as a delete. That is not hypothetical: any drop above the current
/// track creates history, whether or not there was any before.
///
/// So a collapsed queue has a floor, and it is the slot immediately after the
/// current track — which is `current` rather than `current + 1` when the row
/// came from above it, for the same reason [`play_next_index`] shifts.
pub fn earliest_visible_slot(from: usize, current: usize) -> usize {
    play_next_index(from, current)
}

/// Whether two queues have a track in common.
///
/// Used to decide whether an opened history belongs to the queue still on
/// screen. Overlap rather than length or first-id, because Play Next *grows* a
/// queue and must not fold it, while a fresh album shares nothing with the last
/// one and should start folded.
pub fn shares_a_track<'a>(
    old: impl IntoIterator<Item = &'a str>,
    new: impl IntoIterator<Item = &'a str>,
) -> bool {
    let seen: std::collections::HashSet<&str> = old.into_iter().collect();
    new.into_iter().any(|id| seen.contains(id))
}

/// Where "Play Next" puts an item **already in the queue**.
///
/// Not [`crate::player::protocol::Command::PlayNext`], which inserts songs that
/// are not in the queue yet: this is a move, so it keeps the gapless buffer and
/// cannot duplicate the track.
///
/// Same off-by-one as [`drop_index`]: taking the item out from above the current
/// track slides the current track up one, so the slot after it is `current`
/// rather than `current + 1`.
pub fn play_next_index(from: usize, current: usize) -> usize {
    if from < current { current } else { current + 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(v: &[&str]) -> Vec<Key> {
        v.iter().map(|s| Key::Track((*s).to_owned())).collect()
    }

    #[test]
    fn an_unchanged_queue_is_no_work_at_all() {
        assert_eq!(
            plan(&keys(&["a", "b"]), &keys(&["a", "b"])),
            Plan::Unchanged
        );
    }

    #[test]
    fn removing_one_row_touches_only_that_row() {
        // The common case, and the one that has to stay cheap: a 500-track
        // queue losing one row must not re-bind 499 others.
        assert_eq!(
            plan(&keys(&["a", "b", "c"]), &keys(&["a", "c"])),
            Plan::Spliced {
                at: 1,
                remove: 1,
                insert: 0
            }
        );
    }

    #[test]
    fn removing_from_either_end_is_still_one_row() {
        assert_eq!(
            plan(&keys(&["a", "b", "c"]), &keys(&["b", "c"])),
            Plan::Spliced {
                at: 0,
                remove: 1,
                insert: 0
            }
        );
        assert_eq!(
            plan(&keys(&["a", "b", "c"]), &keys(&["a", "b"])),
            Plan::Spliced {
                at: 2,
                remove: 1,
                insert: 0
            }
        );
    }

    #[test]
    fn a_drag_is_a_move_in_both_directions() {
        // Not a splice: a drag from one end of a long queue to the other
        // differs at every index between them, and replacing that range is the
        // rebuild this whole module exists to avoid.
        assert_eq!(
            plan(
                &keys(&["a", "b", "c", "d", "e"]),
                &keys(&["b", "c", "d", "a", "e"])
            ),
            Plan::Moved { from: 0, to: 3 }
        );
        assert_eq!(
            plan(
                &keys(&["a", "b", "c", "d", "e"]),
                &keys(&["a", "d", "b", "c", "e"])
            ),
            Plan::Moved { from: 3, to: 1 }
        );
    }

    #[test]
    fn a_swap_across_a_gap_is_not_a_move() {
        // Two changed indices, like a move — but reconstructing the move does
        // not reproduce it, which is why `single_move` checks the whole list.
        assert_eq!(
            plan(&keys(&["a", "b", "c", "d"]), &keys(&["d", "b", "c", "a"])),
            Plan::Spliced {
                at: 0,
                remove: 4,
                insert: 4
            }
        );
        // Adjacent, though, genuinely is one move.
        assert_eq!(
            plan(&keys(&["a", "b", "c", "d"]), &keys(&["a", "c", "b", "d"])),
            Plan::Moved { from: 1, to: 2 }
        );
    }

    #[test]
    fn opening_the_history_is_a_splice_at_the_head() {
        // The measurement in this module's header: a head splice keeps every
        // row that was on screen exactly where it was, so the current track
        // does not move under the pointer that opened the disclosure.
        let collapsed = [
            vec![Key::History {
                hidden: 2,
                expanded: false,
            }],
            keys(&["c", "d"]),
        ]
        .concat();
        let expanded = [
            vec![Key::History {
                hidden: 2,
                expanded: true,
            }],
            keys(&["a", "b", "c", "d"]),
        ]
        .concat();
        assert_eq!(
            plan(&collapsed, &expanded),
            Plan::Spliced {
                at: 0,
                remove: 1,
                insert: 3
            },
            "the disclosure re-binds and the two played tracks appear above"
        );
        assert_eq!(
            plan(&expanded, &collapsed),
            Plan::Spliced {
                at: 0,
                remove: 3,
                insert: 1
            }
        );
    }

    #[test]
    fn one_more_track_played_only_re_binds_the_disclosure() {
        // A natural gapless advance while collapsed: the disclosure's count
        // goes up and the track that started playing leaves the head.
        let before = [
            vec![Key::History {
                hidden: 1,
                expanded: false,
            }],
            keys(&["b", "c"]),
        ]
        .concat();
        let after = [
            vec![Key::History {
                hidden: 2,
                expanded: false,
            }],
            keys(&["c"]),
        ]
        .concat();
        assert_eq!(
            plan(&before, &after),
            Plan::Spliced {
                at: 0,
                remove: 2,
                insert: 1
            }
        );
    }

    #[test]
    fn a_whole_new_queue_is_a_splice_and_never_a_refill() {
        // Even with nothing in common, this is one splice over the whole list
        // rather than `clear` — because `clear` is the one edit measured to
        // lose the scroll position.
        assert_eq!(
            plan(&keys(&["a", "b"]), &keys(&["x", "y", "z"])),
            Plan::Spliced {
                at: 0,
                remove: 2,
                insert: 3
            }
        );
    }

    #[test]
    fn clearing_the_queue_removes_every_row() {
        assert_eq!(
            plan(&keys(&["a", "b"]), &[]),
            Plan::Spliced {
                at: 0,
                remove: 2,
                insert: 0
            }
        );
    }

    #[test]
    fn a_drop_lands_between_the_two_rows_the_line_was_drawn_between() {
        // Queue [a b c d]. Dragging `a` onto the lower half of `c` means
        // "between c and d", which is index 2 once `a` itself is gone.
        assert_eq!(drop_index(0, 2, true), 2);
        // And onto the upper half of `c` means "between b and c" — index 1.
        assert_eq!(drop_index(0, 2, false), 1);
    }

    #[test]
    fn dragging_upwards_does_not_lose_a_place() {
        // Queue [a b c d]. Dragging `d` onto the upper half of `b` is
        // "between a and b": index 1, and nothing slid up because `d` was
        // below the insertion point.
        assert_eq!(drop_index(3, 1, false), 1);
        // Onto the lower half of `b` is "between b and c": index 2.
        assert_eq!(drop_index(3, 1, true), 2);
    }

    #[test]
    fn a_drop_that_changes_nothing_reports_where_the_row_already_is() {
        // Dropping a row onto its own edges is a no-op, and must report the
        // index it is already at rather than one either side of it — the
        // caller refuses a move to where the row already is.
        assert_eq!(drop_index(2, 2, false), 2);
        assert_eq!(drop_index(2, 2, true), 2);
        // The row immediately below, upper half, is the same no-op.
        assert_eq!(drop_index(2, 3, false), 2);
    }

    #[test]
    fn play_next_puts_a_later_track_directly_after_the_current_one() {
        // Queue [a b c d], `b` playing at 1. Play Next on `d`.
        assert_eq!(play_next_index(3, 1), 2);
    }

    #[test]
    fn a_drop_above_the_current_track_is_floored_to_where_it_stays_visible() {
        // Queue [a b c d] with b playing at 1 and the history collapsed. The
        // reported bug: dragging d onto the upper half of b gives index 1, `b`
        // slides to 2, and d is then behind the disclosure — the drag reads as
        // a delete. The floor puts it immediately after b instead.
        assert_eq!(drop_index(3, 1, false), 1, "what the drop alone says");
        assert_eq!(earliest_visible_slot(3, 1), 2, "and where it may land");

        // Dragging from above the current track shifts the same way
        // `play_next_index` does: removing it slides the current track up one.
        assert_eq!(earliest_visible_slot(0, 2), 2);
    }

    #[test]
    fn a_queue_that_shares_a_track_is_the_same_queue() {
        // Play Next grows a queue; an opened history must survive that.
        assert!(shares_a_track(["a", "b"], ["a", "b", "c"]));
        // A different album shares nothing, so its history starts folded.
        assert!(!shares_a_track(["a", "b"], ["x", "y"]));
        // And a cleared queue is not the queue it replaced.
        assert!(!shares_a_track(["a", "b"], []));
        assert!(!shares_a_track([], ["a"]));
    }

    #[test]
    fn play_next_on_an_earlier_track_accounts_for_the_gap_it_leaves() {
        // Queue [a b c d], `c` playing at 2. Play Next on `a`: removing `a`
        // slides `c` up to 1, so the slot after it is 2 — not 3.
        assert_eq!(play_next_index(0, 2), 2);
    }
}
