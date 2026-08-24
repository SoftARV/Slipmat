// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Turning a list of rows into a queue.
//!
//! Rule 3 lives here: MusicKit owns the queue and we mirror it, so a click
//! sends the **whole** visible list in one `setQueue` and names its starting
//! track **by id**. Nearly every function below exists because some earlier
//! version carried an *index* across a filter, a network round trip or a user
//! action — and started the wrong song.
//!
//! Nothing here draws anything or holds a model: it is the arithmetic a
//! frontend and the daemon both have to get right, and getting it right twice
//! is how they would come to disagree.

use crate::entry::Entry;
use crate::player::protocol::Item;

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
pub fn unresolvable_ids(detail: &str) -> Vec<String> {
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
pub fn queue_from(
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

/// The rows a shuffled queue could open on: those holding a track that can
/// actually be streamed.
///
/// **Chosen among the playable rows rather than over the whole list**, because
/// `queue_from` walks *forward* from the row it is given. An index landing on
/// an album heading or a dead track slides to the next song, which is fine
/// everywhere except at the end of the list — there is nothing to slide to
/// there, `start_id` comes back `None`, and `start_index` falls back to 0.
/// That is #147 reappearing on the last track, at 1/n odds instead of always.
pub fn playable_rows(visible: &[Entry], dead: &std::collections::HashSet<String>) -> Vec<usize> {
    visible
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.catalog_id().is_some_and(|id| !dead.contains(id)))
        .map(|(row, _)| row)
        .collect()
}

/// Whether MusicKit is already holding exactly this set of songs.
///
/// Compared **unordered**: with shuffle on, MusicKit's order is deliberately
/// not ours, and it is still the same queue. A free function so it can be
/// tested without building an `AppModel`, which owns GTK widgets.
pub fn holds(queue: &[Item], songs: &[String]) -> bool {
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
pub fn start_index(songs: &[String], start_id: Option<&String>) -> usize {
    start_id
        .and_then(|id| songs.iter().position(|s| s == id))
        .unwrap_or(0)
}

/// What a queue is **created** in, stated by whoever asked for it.
///
/// Shuffle is a *player* mode in MusicKit, not a property of the queue, so a
/// queue built while it is on comes out shuffled whatever the caller meant.
/// Making it a parameter is what stops that (#152).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// A **row click** — this list, from here. Sequential when it creates a
    /// queue; silent on one already loaded, where turning shuffle off would
    /// reorder the queue out from under a playing track.
    Clicked,
    /// The page's **Play** button: this list, in order, whatever was on before.
    InOrder,
    /// The page's **Shuffle** button. A mode request either way, so it states
    /// itself on a queue already loaded too.
    Shuffled,
}

impl Start {
    /// The mode to state, or `None` to leave the player's alone. `creating`
    /// distinguishes a new queue from a move within the loaded one.
    pub fn mode(self, creating: bool) -> Option<bool> {
        match self {
            Self::Clicked => creating.then_some(false),
            Self::InOrder => Some(false),
            Self::Shuffled => Some(true),
        }
    }

    /// Whether the queue comes back in MusicKit's order rather than ours, which
    /// makes "did it start the right track" unanswerable.
    pub fn reorders(self) -> bool {
        matches!(self, Self::Shuffled)
    }
}

/// Where a clicked row is, from where it sat and what it was.
///
/// **The position is the key, the id is the check.** A queue may hold the same
/// track twice — Play Next and Add to Queue insert into a queue that already
/// has it — so resolving by id alone found the first copy and acted on that
/// instead (#88). If the queue moved since the click the position is wrong, and
/// searching by id is the better wrong answer: it is what this did before.
pub fn index_at(queue: &[Item], at: usize, id: &str) -> Option<usize> {
    let is_it =
        |item: &Item| item.catalog_id.as_deref() == Some(id) || item.id.as_deref() == Some(id);
    match queue.get(at) {
        Some(item) if is_it(item) => Some(at),
        _ => queue.iter().position(is_it),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::music::types::{Album, Artist, Track, TrackId};

    /// A queue holding `ids` in order, duplicates included.
    fn queue(ids: &[&str]) -> Vec<Item> {
        ids.iter()
            .map(|id| Item {
                catalog_id: Some((*id).to_owned()),
                ..Default::default()
            })
            .collect()
    }

    #[test]
    fn a_duplicated_track_resolves_to_the_copy_that_was_clicked() {
        // The bug: `a` appears twice, and resolving by id alone always found
        // index 0 — so removing the second copy removed the first (#88).
        let q = queue(&["a", "b", "a", "c"]);
        assert_eq!(index_at(&q, 2, "a"), Some(2), "clicked the second");
        assert_eq!(index_at(&q, 0, "a"), Some(0), "clicked the first");
        assert_eq!(index_at(&q, usize::MAX, "a"), Some(0), "id alone finds one");
    }

    #[test]
    fn a_moved_queue_falls_back_to_searching_by_id() {
        // The drift the id is there to catch: the click said position 2, but
        // the queue shifted and `a` is at 1 now. Searching is the better wrong
        // answer, and right whenever the track is not duplicated.
        let q = queue(&["b", "a", "c"]);
        assert_eq!(index_at(&q, 2, "a"), Some(1));
    }

    #[test]
    fn a_position_past_the_end_does_not_panic() {
        let q = queue(&["a"]);
        assert_eq!(index_at(&q, 99, "a"), Some(0));
        assert_eq!(index_at(&q, 99, "gone"), None);
    }

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
            library_id: None,
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
                date_added: String::new(),
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
    fn a_shuffle_can_start_on_any_streamable_row_and_only_those() {
        // #147: Shuffle named row 0 every time, so MusicKit pinned track 1 as
        // the head and shuffled only what came after it.
        let visible = vec![
            song("a", Some("1")),
            song("b", None), // no catalog id
            song("c", Some("3")),
            song("d", Some("4")), // dead
            song("e", Some("5")),
        ];
        assert_eq!(playable_rows(&visible, &dead(&["4"])), vec![0, 2, 4]);
    }

    #[test]
    fn a_shuffle_never_starts_on_a_row_that_would_fall_back_to_the_top() {
        // The subtle half. `queue_from` walks *forward* for the first playable
        // track, so an unplayable row usually slides to the next one — but at
        // the end of the list there is nothing to slide to and the start falls
        // back to 0. Picking only among playable rows is what keeps #147 from
        // reappearing on the last track at 1/n odds.
        let visible = vec![song("a", Some("1")), song("b", None)];
        let rows = playable_rows(&visible, &dead(&[]));
        assert_eq!(rows, vec![0], "row 1 would have fallen back to the top");

        for row in rows {
            let (songs, start_id) = queue_from(&visible, row, &dead(&[]));
            assert!(start_id.is_some(), "row {row} names no track");
            assert_eq!(songs[start_index(&songs, start_id.as_ref())], "1");
        }
    }

    #[test]
    fn browse_rows_are_not_somewhere_a_shuffle_can_start() {
        // Same reason they are not enqueued: they are doors, not tracks.
        let visible = vec![
            Entry::Album(Album {
                date_added: String::new(),
                id: "al1".into(),
                name: "Superestrella".into(),
                artist: "Aitana".into(),
                artwork: None,
                year: "2020".into(),
                library: false,
                track_count: 12,
            }),
            song("a", Some("1")),
        ];
        assert_eq!(playable_rows(&visible, &dead(&[])), vec![1]);
    }

    #[test]
    fn a_list_with_nothing_playable_offers_no_shuffle_start() {
        // `shuffle_start` falls back to row 0 here, and `play_entries` toasts
        // "Nothing here can be streamed" a moment later.
        assert!(playable_rows(&[song("a", None)], &dead(&[])).is_empty());
    }

    #[test]
    fn a_row_click_states_sequential_when_it_builds_a_queue() {
        // The bug: shuffle is a *player* mode, so a queue built while it is on
        // comes out shuffled whether or not anyone asked. A click on a song in
        // a list is not a mode request, but it does have to stop inheriting
        // one — otherwise a playlist shuffled an hour ago goes on shuffling
        // every list clicked after it.
        assert_eq!(Start::Clicked.mode(true), Some(false));
    }

    #[test]
    fn a_row_click_leaves_the_mode_alone_when_it_only_moves_within_a_queue() {
        // The other half, and the reason this is not just `Some(false)`.
        // Clicking a track in the list already playing is a move, not a new
        // queue: turning shuffle off there would restore MusicKit's original
        // order and reorder the queue out from under the track being played.
        // The toggle in the bar owns the mode of a queue that already exists.
        assert_eq!(Start::Clicked.mode(false), None);
    }

    #[test]
    fn both_buttons_state_a_mode_either_way() {
        // These *are* mode requests, so they say so on a queue MusicKit already
        // holds too — pressing Shuffle on the playlist you are hearing has to
        // shuffle it. Play is the same argument pointed the other way, and it
        // is the one that was already right before this: it has always turned
        // shuffle off, which is what made the row clicks beside it look wrong.
        for creating in [true, false] {
            assert_eq!(Start::Shuffled.mode(creating), Some(true));
            assert_eq!(Start::InOrder.mode(creating), Some(false));
        }
    }

    #[test]
    fn only_a_queue_that_keeps_our_order_is_worth_verifying() {
        // `verify_start` asks "did MusicKit open on the track we named", and
        // under shuffle that has no answer: the order comes back MusicKit's, so
        // the row we named is not the row it opens on and every track is a
        // legitimate place for a shuffle to start. Asking anyway corrected on
        // every shuffled play — always to index 0 — which is what interrupted
        // the load `setQueue` was still running.
        assert!(Start::Shuffled.reorders());
        assert!(!Start::Clicked.reorders(), "a click keeps the list's order");
        assert!(!Start::InOrder.reorders(), "so does Play");
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
    fn item(catalog: &str) -> Item {
        Item {
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
