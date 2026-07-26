// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Turning a click into a queue, and keeping ours honest against MusicKit's.
//!
//! Rule 3 lives here: MusicKit owns the queue and Rust mirrors it, so a click
//! sends the **whole** visible list in one `setQueue` and names its starting
//! track **by id**. Nearly every function below exists because some earlier
//! version carried an *index* across a filter, a network round trip or a user
//! action — and started the wrong song.

use super::AppModel;
use crate::components::track_row::{Entry, apply_row_state};
use crate::player::protocol::Command;

/// Pull the catalog ids out of MusicKit's `NOT_FOUND` error.
///
/// `setQueue` is all-or-nothing: if a single id cannot be resolved it rejects
/// the whole queue, so one delisted track makes an entire library unplayable.
/// The error names the offenders:
///
/// ```text
/// [mk-007] NOT_FOUND; One or more items could not be resolved: 1550626760, 1526511025
/// ```
///
/// Rather than pre-validating every id against the catalog — hundreds of ids
/// per play, on every play — we let MusicKit tell us, remember them, and retry
/// without them. Self-healing and free in the common case.
///
/// This parses an error *string*, which is exactly the kind of thing rule 4
/// warns about, so it is deliberately loose: find the marker, then take digit
/// runs. If Apple rewords the message we get zero ids and fall back to
/// reporting the error, which is where we started — no worse.
pub(super) fn unresolvable_ids(detail: &str) -> Vec<String> {
    const MARKER: &str = "could not be resolved";
    let Some(tail) = detail.split_once(MARKER).map(|(_, t)| t) else {
        return Vec::new();
    };
    tail.split(|c: char| !c.is_ascii_digit())
        .filter(|s| s.len() >= 6) // catalog ids are long; skip stray numbers
        .map(str::to_owned)
        .collect()
}

/// Build a MusicKit queue from the visible rows, plus **the id to start on**.
///
/// The whole visible list is enqueued, never just the clicked track — the
/// gapless rule (rule 3): MusicKit can only transition seamlessly between items
/// it already holds.
///
/// Note this returns an *id*, not an index. Rows are filtered twice on the way
/// to a queue — once for tracks with no catalog id, again for ids MusicKit has
/// rejected — and a retry filters a third time. Carrying an index through that
/// means re-deriving it at every step and being right every time; carrying the
/// id means the answer cannot drift. An earlier version did the arithmetic and
/// started the wrong track once dead ids entered the picture.
///
/// If the clicked track itself can't be streamed, this starts on the first one
/// after it that can — which is what a person expects from clicking a dead row.
pub(super) fn queue_from(
    visible: &[Entry],
    row: usize,
    dead: &std::collections::HashSet<String>,
) -> (Vec<String>, Option<String>) {
    let alive = |id: &String| !dead.contains(id);
    let mut seen = std::collections::HashSet::new();
    // Album and artist rows have no catalog id, so they drop out here. A queue
    // built from a mixed result list is the songs in it, in order.
    let songs: Vec<String> = visible
        .iter()
        .filter_map(|e| e.catalog_id().map(str::to_owned))
        .filter(alive)
        // Deduplicate. MusicKit collapses repeats when it builds the queue, so
        // sending the same id twice makes its queue shorter than ours and every
        // position after the repeat refers to a different track than we meant.
        .filter(|id| seen.insert(id.clone()))
        .collect();
    let start_id = visible
        .get(row..)
        .unwrap_or(&[])
        .iter()
        .filter_map(|e| e.catalog_id().map(str::to_owned))
        .find(alive);
    (songs, start_id)
}

/// Whether MusicKit is already holding exactly this set of songs.
///
/// Compared **unordered**: with shuffle on, MusicKit's order is deliberately
/// not ours, and it is still the same queue. A free function so it can be
/// tested without building an `AppModel`, which owns GTK widgets.
pub(super) fn holds(queue: &[crate::player::protocol::Item], songs: &[String]) -> bool {
    if queue.len() != songs.len() {
        return false;
    }
    let mut theirs: Vec<&str> = queue
        .iter()
        .filter_map(|item| item.catalog_id.as_deref().or(item.id.as_deref()))
        .collect();
    let mut ours: Vec<&str> = songs.iter().map(String::as_str).collect();
    theirs.sort_unstable();
    ours.sort_unstable();
    theirs == ours
}

/// Where `start_id` sits in `songs`. Falls back to the top rather than failing:
/// playing from the start beats not playing.
pub(super) fn start_index(songs: &[String], start_id: Option<&String>) -> usize {
    start_id
        .and_then(|id| songs.iter().position(|s| s == id))
        .unwrap_or(0)
}

impl AppModel {
    /// Remember the queue and where we are in it.
    ///
    /// Called on every track change *and* on shutdown, deliberately. Shutdown
    /// is the only moment the position is accurate, but it is also the one that
    /// might not run — a crash, a SIGKILL, a session ending badly. Saving on
    /// each track change means the worst case is restoring the right track at
    /// its start rather than restoring nothing at all.
    pub(super) fn save_session(&self) {
        let songs: Vec<String> = self
            .player
            .queue
            .iter()
            .filter_map(|item| item.catalog_id.clone().or_else(|| item.id.clone()))
            .collect();

        if songs.is_empty() {
            crate::session::clear();
            return;
        }

        crate::session::save(&crate::session::Session {
            start: self
                .player
                .queue_position
                .min(songs.len().saturating_sub(1)),
            position_ms: self.player.position_ms,
            songs,
        });
    }

    /// Put back what was playing when the app last closed.
    ///
    /// Loaded **paused**, and the position is applied only once MusicKit
    /// confirms it is holding the queue we asked for — see `finish_restore`.
    pub(super) fn restore_session(&mut self) {
        let Some(session) = crate::session::load() else {
            return;
        };
        let start = session.start.min(session.songs.len() - 1);
        let wanted = session.songs.get(start).cloned();

        tracing::info!(
            tracks = session.songs.len(),
            start,
            position_ms = session.position_ms,
            "restoring the last session"
        );

        self.pending_start = wanted.clone();
        self.last_queue = Some((session.songs.clone(), wanted));
        // Zero is not worth a seek, and neither is a position the track has
        // effectively already finished at — restoring two seconds from the end
        // just skips to the next song.
        self.restore_to_ms = (session.position_ms > 1_000).then_some(session.position_ms);
        self.send(Command::SetQueue {
            songs: session.songs,
            start_position: start,
            start_playing: false,
        });
    }

    /// Seek to the restored position, once the restored queue is actually here.
    ///
    /// Same discipline as `verify_start`: the mirror holds the *previous* queue
    /// for a moment after a `setQueue`, and seeking into that would land
    /// somewhere arbitrary.
    pub(super) fn finish_restore(&mut self) {
        let Some(position_ms) = self.restore_to_ms else {
            return;
        };
        let Some((sent, _)) = &self.last_queue else {
            return;
        };
        if !holds(&self.player.queue, sent) {
            return; // not our queue yet
        }
        // Past the end of the track it belongs to: start it over instead.
        let duration = self.player.duration_ms;
        self.restore_to_ms = None;
        if duration > 0 && position_ms + 5_000 >= duration {
            tracing::debug!("restored position was at the end; starting the track over");
            return;
        }
        tracing::info!(position_ms, "restoring position");
        self.send(Command::Seek { position_ms });
    }

    /// The catalog id of the track MusicKit is on, if any.
    pub(super) fn playing_catalog_id(&self) -> Option<String> {
        self.player
            .now_playing
            .as_ref()
            .and_then(|i| i.catalog_id.clone().or_else(|| i.id.clone()))
    }

    /// Enqueue a list and start at `row`, per rule 3: the whole thing goes to
    /// MusicKit in one `setQueue`, and the starting track is named by id.
    ///
    /// Shared by the results list and every pushed page — one enqueue path, so
    /// a fix to it cannot land on one list and miss the other.
    pub(super) fn play_entries(&mut self, entries: &[Entry], row: usize) {
        let (songs, start_id) = queue_from(entries, row, &self.dead_ids);
        if songs.is_empty() {
            self.toast("Nothing here can be streamed");
            return;
        }

        // **Rule 3, in the one place it was being broken.** Clicking a track in
        // a list whose queue MusicKit already holds is a move *within* that
        // queue, not a reason to rebuild it. Rebuilding tore the queue down and
        // built the same 117 tracks again, which cost the gapless buffer,
        // blanked the bar, and — because the old queue and the new one are
        // identical — left `verify_start` unable to tell whether the queue it
        // was looking at was the one it had just asked for. It corrected
        // against the stale one and started the wrong song.
        if let Some(wanted) = &start_id
            && holds(&self.player.queue, &songs)
            && let Some(index) = self.queue_index_of(wanted)
        {
            tracing::info!(index, "already loaded; moving within the queue");
            // Nothing pending: there is no new queue to verify against.
            self.pending_start = None;
            self.send(Command::ChangeToIndex { index });
            return;
        }

        let start = start_index(&songs, start_id.as_ref());
        tracing::info!(queue = songs.len(), start, "enqueuing");
        self.pending_start = start_id.clone();
        self.last_queue = Some((songs.clone(), start_id));
        self.send(Command::SetQueue {
            songs,
            start_position: start,
            start_playing: true,
        });
    }

    /// Handle MusicKit's all-or-nothing `NOT_FOUND` by dropping the ids it
    /// named and trying again.
    ///
    /// Returns true when it took ownership of the error, so the caller doesn't
    /// also toast a message the user can do nothing about.
    pub(super) fn retry_without_dead_tracks(&mut self, detail: &str) -> bool {
        let dead = unresolvable_ids(detail);
        if dead.is_empty() {
            return false;
        }
        let Some((songs, wanted)) = self.last_queue.take() else {
            return false;
        };

        let newly_dead = dead
            .iter()
            .filter(|id| !self.dead_ids.contains(*id))
            .count();
        self.dead_ids.extend(dead);
        if newly_dead > 0 {
            // Remember them, so the next run starts already knowing.
            crate::unplayable::save(&self.dead_ids);
        }

        // Nothing new: the retry already happened and failed again. Stop, or we
        // loop forever on an error we cannot parse our way out of.
        if newly_dead == 0 {
            tracing::warn!("queue still unresolvable after dropping known-dead ids");
            return false;
        }

        // If the track we were aiming at is itself newly dead, aim at the next
        // surviving track *after* it in the original order — not at the top of
        // the list, which is where falling back to index 0 would land.
        let from = songs
            .iter()
            .position(|s| Some(s) == wanted.as_ref())
            .unwrap_or(0);
        let wanted = songs[from..]
            .iter()
            .find(|id| !self.dead_ids.contains(*id))
            .cloned();

        let retry: Vec<String> = songs
            .into_iter()
            .filter(|id| !self.dead_ids.contains(id))
            .collect();

        if retry.is_empty() {
            self.toast("None of these tracks are available to stream");
            return true;
        }

        let start = start_index(&retry, wanted.as_ref());
        tracing::info!(
            dropped = newly_dead,
            queue = retry.len(),
            start,
            "retrying queue without unresolvable tracks"
        );
        self.mark_dead_tracks_unplayable();
        self.pending_start = wanted.clone();
        self.last_queue = Some((retry.clone(), wanted));
        self.send(Command::SetQueue {
            songs: retry,
            start_position: start,
            start_playing: true,
        });
        true
    }

    /// Reflect newly-refused tracks in the list **without rebuilding it**.
    ///
    /// This fires on the first play of a session — exactly when the user is
    /// looking at the row they just clicked — so a rebuild here is what sent
    /// the library back to the top, once per run. Rows consult the shared set
    /// at bind, so updating it covers everything off screen; the rows that are
    /// on screen are repainted directly.
    ///
    /// `all_tracks` keeps its catalog ids: playability is now a question for
    /// `dead_rows`, and blanking the id would also lose the handle the queue
    /// builder needs.
    pub(super) fn mark_dead_tracks_unplayable(&mut self) {
        *self.dead_rows.borrow_mut() = self.dead_ids.clone();

        let playing = self.playing_catalog_id();
        let registry = self.library_icons.borrow();
        for id in &self.dead_ids {
            if let Some(w) = registry.get(id) {
                apply_row_state(&w.icon, &w.root, Some(id) == playing.as_ref(), false);
            }
        }
    }

    /// Check that MusicKit actually landed on the track we asked for, and
    /// correct it if not.
    ///
    /// `setQueue` takes a *position*, but the queue MusicKit builds is not
    /// always the list we handed it — it drops repeats and anything it decides
    /// it cannot use, and every position after such an item then refers to a
    /// different track. Observed directly: 531 ids sent, `queue_len=530` back,
    /// and playback one track further down than the row that was clicked.
    ///
    /// No amount of arithmetic on our side can fix that, because the
    /// discrepancy happens inside MusicKit. So we check its own queue for the
    /// id we wanted and jump if we are not on it — `changeToMediaAtIndex`, not
    /// a second `setQueue`, so the queue is not rebuilt and gapless survives.
    pub(super) fn verify_start(&mut self) {
        let Some(wanted) = self.pending_start.clone() else {
            return;
        };
        if self.player.queue.is_empty() {
            return; // queue hasn't arrived yet; try again on the next event
        }

        let id_of = |item: &crate::player::protocol::Item| {
            item.catalog_id.clone().or_else(|| item.id.clone())
        };

        // **Wait for the queue we actually sent.** The mirror still holds the
        // previous one for a few milliseconds after `setQueue`, and playing the
        // same playlist twice means both have the same length and the same
        // ids — so "is a queue loaded" is not enough to tell them apart. An
        // earlier version corrected 3ms after sending, against the old queue,
        // and jumped to whatever sat at that index. Compare *sorted* ids: with
        // shuffle on, MusicKit's order is deliberately not ours.
        if let Some((sent, _)) = &self.last_queue {
            let mut theirs: Vec<String> = self.player.queue.iter().filter_map(id_of).collect();
            let mut ours = sent.clone();
            theirs.sort_unstable();
            ours.sort_unstable();
            if theirs != ours {
                return; // not our queue yet
            }
        }

        // One shot either way: acting or giving up both clear the flag, so a
        // correction can never bounce against MusicKit's own echo.
        self.pending_start = None;
        let Some(index) = self
            .player
            .queue
            .iter()
            .position(|item| id_of(item).as_deref() == Some(wanted.as_str()))
        else {
            tracing::debug!(%wanted, "chosen track is not in MusicKit's queue");
            return;
        };

        if index == self.player.queue_position {
            return; // already right
        }
        tracing::info!(
            from = self.player.queue_position,
            to = index,
            "MusicKit started the wrong track; correcting"
        );
        self.send(Command::ChangeToIndex { index });
    }

    /// Where a track sits in MusicKit's queue *right now*.
    ///
    /// Resolved at send time rather than carried from the row, because our row
    /// order and MusicKit's queue can drift — and a stale position does not
    /// fail loudly, it removes or plays the wrong track, or gets rejected with
    /// INVALID_ARGUMENTS once it runs off the end.
    pub(super) fn queue_index_of(&self, id: &str) -> Option<usize> {
        self.player.queue.iter().position(|item| {
            item.catalog_id.as_deref() == Some(id) || item.id.as_deref() == Some(id)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::music::types::{Album, Artist, Track, TrackId};

    /// A song row, as the results list holds it.
    fn song(title: &str, catalog: Option<&str>) -> Entry {
        Entry::Song(track(title, catalog))
    }

    fn track(title: &str, catalog: Option<&str>) -> Track {
        Track {
            id: TrackId(format!("i.{title}")),
            catalog_id: catalog.map(str::to_owned),
            title: title.into(),
            favorite: false,
            in_library: false,
            date_added: String::new(),
            year: String::new(),
            artist: "Aitana".into(),
            album: "Superestrella".into(),
            duration_ms: 200_000,
            track_number: 1,
            artwork: None,
        }
    }

    fn dead(ids: &[&str]) -> std::collections::HashSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn clicking_a_row_enqueues_the_whole_visible_list() {
        let visible = vec![
            song("a", Some("1")),
            song("b", Some("2")),
            song("c", Some("3")),
        ];

        // Rule 3: the whole list goes in, not just the clicked track.
        let (songs, start_id) = queue_from(&visible, 1, &dead(&[]));
        assert_eq!(songs, vec!["1", "2", "3"]);
        assert_eq!(start_index(&songs, start_id.as_ref()), 1);
    }

    #[test]
    fn unplayable_rows_do_not_shift_the_chosen_track() {
        // Row 3 is "d", but "b" cannot be streamed so never enters the queue.
        // Carrying an index through that filter is what started the wrong song.
        let visible = vec![
            song("a", Some("1")),
            song("b", None),
            song("c", Some("3")),
            song("d", Some("4")),
        ];

        let (songs, start_id) = queue_from(&visible, 3, &dead(&[]));
        assert_eq!(songs, vec!["1", "3", "4"]);
        assert_eq!(songs[start_index(&songs, start_id.as_ref())], "4");
    }

    #[test]
    fn known_dead_ids_never_reach_the_queue_and_do_not_shift_it() {
        // "2" was rejected by MusicKit on an earlier play. Clicking "c" must
        // still start "c", not the track above or below it.
        let visible = vec![
            song("a", Some("1")),
            song("b", Some("2")),
            song("c", Some("3")),
        ];

        let (songs, start_id) = queue_from(&visible, 2, &dead(&["2"]));
        assert_eq!(songs, vec!["1", "3"], "dead id must not be sent");
        assert_eq!(songs[start_index(&songs, start_id.as_ref())], "3");
    }

    #[test]
    fn clicking_a_dead_row_starts_the_next_streamable_track() {
        let visible = vec![
            song("a", Some("1")),
            song("b", Some("2")),
            song("c", Some("3")),
        ];

        // Click "b", which is dead: the sensible result is "c", not the top.
        let (songs, start_id) = queue_from(&visible, 1, &dead(&["2"]));
        assert_eq!(songs[start_index(&songs, start_id.as_ref())], "3");
    }

    #[test]
    fn album_and_artist_rows_are_not_enqueued() {
        // Catalog results mix browse rows in above the songs. They are doors,
        // not tracks: they must never take a slot in the queue, and the row
        // index must not shift because of them.
        let visible = vec![
            Entry::Artist(Artist {
                id: "a1".into(),
                name: "Aitana".into(),
                artwork: None,
                genres: String::new(),
                library: false,
            }),
            Entry::Album(Album {
                id: "al1".into(),
                name: "Superestrella".into(),
                artist: "Aitana".into(),
                artwork: None,
                year: "2020".into(),
                library: false,
                track_count: 12,
            }),
            song("a", Some("1")),
            song("b", Some("2")),
        ];

        let (songs, start_id) = queue_from(&visible, 3, &dead(&[]));
        assert_eq!(songs, vec!["1", "2"], "browse rows are not tracks");
        assert_eq!(songs[start_index(&songs, start_id.as_ref())], "2");
    }

    #[test]
    fn a_list_with_nothing_playable_produces_no_queue() {
        let (songs, _) = queue_from(&[song("a", None)], 0, &dead(&[]));
        assert!(songs.is_empty(), "caller must toast rather than enqueue");
    }

    #[test]
    fn clicking_past_the_last_streamable_track_falls_back_to_the_top() {
        let visible = vec![song("a", Some("1")), song("b", None)];
        let (songs, start_id) = queue_from(&visible, 1, &dead(&[]));
        // Nothing streamable at or after the click: play from the start rather
        // than not play at all.
        assert_eq!(start_index(&songs, start_id.as_ref()), 0);
    }

    /// A MusicKit queue item, as the mirror holds it.
    fn item(catalog: &str) -> crate::player::protocol::Item {
        crate::player::protocol::Item {
            id: None,
            catalog_id: Some(catalog.into()),
            title: catalog.into(),
            artist: String::new(),
            album: String::new(),
            duration_ms: 0,
            track_number: 0,
            artwork_template: None,
        }
    }

    #[test]
    fn a_shuffled_queue_is_still_the_same_queue() {
        // The reported bug. Clicking a track in a playlist that is already
        // playing shuffled must move within that queue, not rebuild it — and
        // shuffle means MusicKit's order is deliberately not ours.
        let queue = [item("3"), item("1"), item("2")];
        assert!(holds(&queue, &["1".into(), "2".into(), "3".into()]));
    }

    #[test]
    fn a_different_list_is_not_the_same_queue() {
        let queue = [item("1"), item("2")];
        assert!(!holds(&queue, &["1".into(), "3".into()]));
        assert!(!holds(&queue, &["1".into(), "2".into(), "3".into()]));
        assert!(!holds(&queue, &[]));
    }
}
