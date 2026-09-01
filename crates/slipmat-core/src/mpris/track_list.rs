// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

use std::ops::Range;

use mpris_server::TrackId;

use crate::player::protocol::Item;

const LIMIT: usize = 21;
const RADIUS: usize = 10;

#[derive(Debug)]
struct Occurrence {
    source_id: String,
    track_id: TrackId,
    item: Item,
    index: usize,
}

#[derive(Debug, Default, PartialEq)]
pub(crate) struct Change {
    pub(crate) queue: bool,
    pub(crate) window: bool,
    pub(crate) metadata: Vec<TrackId>,
}

#[derive(Debug)]
pub(crate) struct Projection {
    occurrences: Vec<Occurrence>,
    window: Range<usize>,
    current: Option<TrackId>,
    next_id: u64,
}

impl Default for Projection {
    fn default() -> Self {
        Self {
            occurrences: Vec::new(),
            window: 0..0,
            current: None,
            next_id: 1,
        }
    }
}

impl Projection {
    pub(crate) fn reconcile(
        &mut self,
        queue: &[Item],
        queue_position: usize,
        current: Option<&Item>,
    ) -> Change {
        let previous_queue: Vec<_> = self
            .occurrences
            .iter()
            .map(|entry| entry.track_id.clone())
            .collect();
        let previous_window = self.tracks();
        let previous_items: Vec<_> = self
            .occurrences
            .iter()
            .map(|entry| (entry.track_id.clone(), entry.item.clone()))
            .collect();
        let mut previous = std::mem::take(&mut self.occurrences);
        self.occurrences = queue
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let track_id = (!item.occurrence_id.is_empty())
                    .then(|| {
                        previous
                            .iter()
                            .position(|old| old.source_id == item.occurrence_id)
                    })
                    .flatten()
                    .map(|at| previous.remove(at).track_id)
                    .unwrap_or_else(|| self.allocate());
                Occurrence {
                    source_id: item.occurrence_id.clone(),
                    track_id,
                    item: item.clone(),
                    index,
                }
            })
            .collect();

        let current_index = current
            .filter(|item| !item.occurrence_id.is_empty())
            .and_then(|item| {
                self.occurrences
                    .iter()
                    .position(|entry| entry.source_id == item.occurrence_id)
            });
        self.current = current_index.map(|index| self.occurrences[index].track_id.clone());
        self.window = context_window(queue.len(), current_index.unwrap_or(queue_position));

        let current_queue: Vec<_> = self
            .occurrences
            .iter()
            .map(|entry| entry.track_id.clone())
            .collect();
        let current_window = self.tracks();
        let metadata = self.occurrences[self.window.clone()]
            .iter()
            .filter_map(|entry| {
                previous_items
                    .iter()
                    .find(|(id, _)| id == &entry.track_id)
                    .filter(|(_, old)| !same_metadata(old, &entry.item))
                    .map(|_| entry.track_id.clone())
            })
            .collect();
        Change {
            queue: previous_queue != current_queue,
            window: previous_window != current_window,
            metadata,
        }
    }

    pub(crate) fn tracks(&self) -> Vec<TrackId> {
        self.occurrences[self.window.clone()]
            .iter()
            .map(|entry| entry.track_id.clone())
            .collect()
    }

    pub(crate) fn current(&self) -> Option<TrackId> {
        self.current.clone()
    }

    pub(crate) fn index(&self, id: &TrackId) -> Option<usize> {
        self.exposed(id).map(|entry| entry.index)
    }

    pub(crate) fn item(&self, id: &TrackId) -> Option<&Item> {
        self.exposed(id).map(|entry| &entry.item)
    }

    pub(crate) fn metadata(&self, ids: &[TrackId]) -> Vec<(TrackId, &Item)> {
        ids.iter()
            .filter_map(|id| self.item(id).map(|item| (id.clone(), item)))
            .collect()
    }

    fn exposed(&self, id: &TrackId) -> Option<&Occurrence> {
        self.occurrences[self.window.clone()]
            .iter()
            .find(|entry| &entry.track_id == id)
    }

    #[cfg(test)]
    fn id_for_occurrence(&self, source_id: &str) -> Option<TrackId> {
        self.occurrences
            .iter()
            .find(|entry| entry.source_id == source_id)
            .map(|entry| entry.track_id.clone())
    }

    fn allocate(&mut self) -> TrackId {
        loop {
            let path = format!("/dev/miguelrincon/Slipmat/tracklist/{}", self.next_id);
            self.next_id = self.next_id.saturating_add(1);
            if let Ok(id) = TrackId::try_from(path) {
                return id;
            }
        }
    }
}

fn same_metadata(a: &Item, b: &Item) -> bool {
    a.title == b.title
        && a.artist == b.artist
        && a.album == b.album
        && a.duration_ms == b.duration_ms
        && a.track_number == b.track_number
}

fn context_window(queue_len: usize, anchor: usize) -> Range<usize> {
    if queue_len == 0 {
        return 0..0;
    }
    let width = queue_len.min(LIMIT);
    let anchor = anchor.min(queue_len - 1);
    let start = anchor.saturating_sub(RADIUS).min(queue_len - width);
    start..start + width
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(occurrence_id: &str, title: &str) -> Item {
        Item {
            occurrence_id: occurrence_id.into(),
            id: Some("song-a".into()),
            title: title.into(),
            ..Default::default()
        }
    }

    fn paths(projection: &Projection) -> Vec<String> {
        projection
            .tracks()
            .iter()
            .map(|id| id.as_str().to_owned())
            .collect()
    }

    #[test]
    fn context_stays_bounded_and_shifts_at_both_edges() {
        assert_eq!(context_window(0, 0), 0..0);
        assert_eq!(context_window(8, 4), 0..8);
        assert_eq!(context_window(100, 0), 0..21);
        assert_eq!(context_window(100, 50), 40..61);
        assert_eq!(context_window(100, 99), 79..100);
        assert_eq!(context_window(100, 500), 79..100);
    }

    #[test]
    fn a_long_queue_still_exposes_only_twenty_one_occurrences() {
        let window = context_window(500, 250);
        assert_eq!(window.len(), 21);
        assert_eq!(window, 240..261);
    }

    #[test]
    fn duplicate_occurrences_have_distinct_ids_and_the_exact_current_one_wins() {
        let queue = vec![item("run:1", "Same"), item("run:2", "Same")];
        let mut projection = Projection::default();

        projection.reconcile(&queue, 1, Some(&queue[0]));

        let tracks = projection.tracks();
        assert_ne!(tracks[0], tracks[1]);
        assert_eq!(projection.current(), Some(tracks[0].clone()));
        assert_eq!(projection.index(&tracks[0]), Some(0));
        assert_eq!(projection.index(&tracks[1]), Some(1));
    }

    #[test]
    fn retained_occurrences_keep_ids_through_edits_and_window_slides() {
        let mut queue: Vec<_> = (0..30)
            .map(|n| item(&format!("run:{n}"), &format!("Track {n}")))
            .collect();
        let mut projection = Projection::default();
        projection.reconcile(&queue, 10, Some(&queue[10]));
        let retained = projection.id_for_occurrence("run:10").expect("initial id");

        queue.insert(0, item("run:new", "New"));
        let moved = queue.remove(6);
        queue.insert(20, moved);
        queue.retain(|entry| entry.occurrence_id != "run:4");
        let current = queue
            .iter()
            .find(|entry| entry.occurrence_id == "run:10")
            .expect("retained current");
        projection.reconcile(&queue, 25, Some(current));

        assert_eq!(projection.id_for_occurrence("run:10"), Some(retained));
        assert_eq!(projection.tracks().len(), 21);
    }

    #[test]
    fn stale_and_unknown_ids_are_not_resolved() {
        let queue = vec![item("run:1", "One"), item("run:2", "Two")];
        let mut projection = Projection::default();
        projection.reconcile(&queue, 0, Some(&queue[0]));
        let stale = projection.tracks()[0].clone();

        projection.reconcile(&queue[1..], 0, Some(&queue[1]));
        let unknown = mpris_server::TrackId::try_from(
            "/dev/miguelrincon/Slipmat/tracklist/999999".to_owned(),
        )
        .expect("valid object path");

        assert_eq!(projection.index(&stale), None);
        assert!(projection.item(&stale).is_none());
        assert_eq!(projection.index(&unknown), None);
        assert!(projection.item(&unknown).is_none());
    }

    #[test]
    fn empty_and_out_of_range_inputs_are_safe() {
        let mut projection = Projection::default();
        projection.reconcile(&[], 500, None);
        assert!(projection.tracks().is_empty());
        assert!(projection.current().is_none());

        let queue = vec![item("run:1", "One")];
        projection.reconcile(&queue, 500, None);
        assert_eq!(paths(&projection).len(), 1);
        assert!(projection.current().is_none());
    }

    #[test]
    fn metadata_changes_are_distinct_from_queue_and_window_changes() {
        let mut queue = vec![item("run:1", "One"), item("run:2", "Two")];
        let mut projection = Projection::default();
        projection.reconcile(&queue, 0, Some(&queue[0]));
        let changed_id = projection.id_for_occurrence("run:2").expect("known id");

        queue[1].title = "Two (Remastered)".into();
        let change = projection.reconcile(&queue, 0, Some(&queue[0]));

        assert!(!change.queue);
        assert!(!change.window);
        assert_eq!(change.metadata, vec![changed_id]);
    }

    #[test]
    fn an_edit_outside_the_window_changes_only_the_full_queue() {
        let mut queue: Vec<_> = (0..30)
            .map(|n| item(&format!("run:{n}"), &format!("Track {n}")))
            .collect();
        let mut projection = Projection::default();
        projection.reconcile(&queue, 10, Some(&queue[10]));

        let moved = queue.remove(25);
        queue.push(moved);
        let current = queue
            .iter()
            .find(|entry| entry.occurrence_id == "run:10")
            .expect("current");
        let change = projection.reconcile(&queue, 10, Some(current));

        assert!(change.queue);
        assert!(!change.window);
        assert!(change.metadata.is_empty());
    }

    #[test]
    fn sliding_the_context_is_a_window_change_without_a_queue_change() {
        let queue: Vec<_> = (0..40)
            .map(|n| item(&format!("run:{n}"), &format!("Track {n}")))
            .collect();
        let mut projection = Projection::default();
        projection.reconcile(&queue, 10, Some(&queue[10]));

        let change = projection.reconcile(&queue, 11, Some(&queue[11]));

        assert!(!change.queue);
        assert!(change.window);
        assert!(change.metadata.is_empty());
    }

    #[test]
    fn a_current_change_inside_a_short_queue_needs_no_tracklist_notice() {
        let queue = vec![item("run:1", "One"), item("run:2", "Two")];
        let mut projection = Projection::default();
        projection.reconcile(&queue, 0, Some(&queue[0]));

        let change = projection.reconcile(&queue, 1, Some(&queue[1]));

        assert!(!change.queue);
        assert!(!change.window);
        assert!(change.metadata.is_empty());
    }

    #[test]
    fn metadata_lookup_preserves_request_order_and_ignores_unknown_ids() {
        let queue = vec![item("run:1", "One"), item("run:2", "Two")];
        let mut projection = Projection::default();
        projection.reconcile(&queue, 0, Some(&queue[0]));
        let tracks = projection.tracks();
        let unknown = TrackId::try_from("/dev/miguelrincon/Slipmat/tracklist/999999".to_owned())
            .expect("valid object path");

        let metadata = projection.metadata(&[tracks[1].clone(), unknown, tracks[0].clone()]);

        assert_eq!(
            metadata
                .iter()
                .map(|(_, item)| item.title.as_str())
                .collect::<Vec<_>>(),
            vec!["Two", "One"]
        );
    }

    #[test]
    fn a_five_hundred_item_projection_maps_the_published_window_to_full_indices() {
        let queue: Vec<_> = (0..500)
            .map(|n| item(&format!("run:{n}"), &format!("Track {n}")))
            .collect();
        let mut projection = Projection::default();
        projection.reconcile(&queue, 250, Some(&queue[250]));
        let tracks = projection.tracks();

        assert_eq!(tracks.len(), 21);
        assert_eq!(projection.index(&tracks[0]), Some(240));
        assert_eq!(projection.index(&tracks[20]), Some(260));
    }

    #[test]
    fn an_unmatched_current_item_reports_no_current_occurrence() {
        let queue = vec![item("run:1", "One")];
        let outside = item("run:outside", "Outside");
        let mut projection = Projection::default();

        projection.reconcile(&queue, 0, Some(&outside));

        assert_eq!(projection.tracks().len(), 1);
        assert!(projection.current().is_none());
    }
}
