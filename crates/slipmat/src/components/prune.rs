// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Keeping `~/.cache/slipmat/artwork` from growing forever.
//!
//! It is the last unbounded thing Slipmat writes: the Chromium cache is capped
//! and the library cache is one 430KB file, but every cover fetched for every
//! search anybody ever ran stays on disk. Measured at 973 files and 41MB
//! against a library that can only account for about 700 of them.
//!
//! **Provenance is not recorded, and deliberately is not going to be.** A
//! filename is a one-way hash of the URL, so the directory cannot say whether a
//! cover belongs to your library or to a search — but the *app* can, because it
//! holds the library. Deriving the keep-set at prune time beats tagging files
//! at write time for three reasons: a cover can be both (an album you own also
//! appears in search, and a tag would have to pick), it self-corrects when an
//! album leaves your library, and it can be counted before anything is deleted.
//!
//! **The cache is why the grids are fast** — #27 measured 520ms against 75ms,
//! and the win was covers already being on disk. So the keep-set prune only
//! runs when the directory is over its cap. What runs unconditionally is the
//! narrower sweep for files in shapes the app has stopped writing, which
//! nothing will ever read again whatever they contain.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Above this, covers that are not in the library are fair game.
///
/// Five times the 41MB this was measured at, so it is a backstop rather than a
/// policy — it should be reached by browsing a lot of catalogue, which is
/// exactly the case where the oldest of it is worth losing.
const CACHE_CAP: u64 = 200 * 1024 * 1024;

/// What a file in the cache directory turns out to be.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Entry<'a> {
    /// A cover, a backdrop, or anything else that hangs off one key.
    Cover(&'a str),
    /// A mosaic, named after every cover it was drawn from.
    Mosaic(Vec<&'a str>),
    /// A shape the app no longer writes. Dead whatever its key says.
    Retired,
    /// Not ours, or not recognisable. Left alone.
    Unknown,
}

/// Sizes the app still asks for. `256` was the first mosaic implementation's
/// tile size and nothing has read one since.
const LIVE_SIZES: [&str; 2] = ["320", "512"];

/// Read a filename back into what wrote it.
///
/// Every rule here is a shape currently on disk, and the two that are *not*
/// simply "key-size" are the two the issue warned a naive sweep would bin:
/// mosaics carry four keys rather than one, and backdrops hang off their
/// cover's key and have to follow it.
pub(super) fn classify(name: &str) -> Entry<'_> {
    // `<k1k2k3k4>-mosaic-512.png`. Checked first: it is the only name with a
    // word in the middle, and its key half would otherwise parse as one long
    // nonsense key.
    if let Some((keys, rest)) = name.split_once("-mosaic-") {
        if !rest.ends_with(".png") || keys.len() % 16 != 0 || keys.is_empty() {
            return Entry::Unknown;
        }
        return Entry::Mosaic(
            (0..keys.len() / 16)
                .map(|i| &keys[i * 16..(i + 1) * 16])
                .collect(),
        );
    }

    let Some((key, rest)) = name.split_once('-') else {
        return Entry::Unknown;
    };
    if key.len() != 16 || !key.chars().all(|c| c.is_ascii_hexdigit()) {
        return Entry::Unknown;
    }

    // `<key>-512.backdrop256.png` — a blurred copy of the cover beside it, and
    // it lives or dies with that cover. The numberless `backdrop.png` form
    // predates the size going into the name and is not written any more.
    if let Some((size, tail)) = rest.split_once(".backdrop") {
        return if LIVE_SIZES.contains(&size) && tail != ".png" {
            Entry::Cover(key)
        } else {
            Entry::Retired
        };
    }

    // `<key>-320.jpg`
    match rest.strip_suffix(".jpg") {
        Some(size) if LIVE_SIZES.contains(&size) => Entry::Cover(key),
        Some(_) => Entry::Retired,
        None => Entry::Unknown,
    }
}

/// What a prune did, or would do.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub removed: usize,
    pub freed: u64,
    pub kept: usize,
    /// What the directory weighed before the sweep.
    pub total: u64,
    /// Whether the keep-set was applied at all, or only the retired-shape sweep.
    pub over_cap: bool,
}

/// Delete what nothing will read again.
///
/// `keep` is every cache key the library can account for. Best-effort
/// throughout: a cover that cannot be deleted is a cover that gets fetched
/// again, which is the cost this whole file is trying to bound.
pub fn run(keep: &HashSet<String>) -> Report {
    let Some(dir) = super::artwork::cache_dir() else {
        return Report::default();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Report::default();
    };

    let files: Vec<(PathBuf, u64)> = entries
        .flatten()
        .filter_map(|e| {
            let len = e.metadata().ok().filter(|m| m.is_file())?.len();
            Some((e.path(), len))
        })
        .collect();

    let total: u64 = files.iter().map(|(_, len)| len).sum();
    let over_cap = total > CACHE_CAP;
    let mut report = Report {
        over_cap,
        total,
        ..Report::default()
    };

    for (path, len) in &files {
        if evictable(path, keep, over_cap) {
            match std::fs::remove_file(path) {
                Ok(()) => {
                    report.removed += 1;
                    report.freed += len;
                }
                Err(err) => tracing::debug!(?err, ?path, "could not prune"),
            }
        } else {
            report.kept += 1;
        }
    }

    report
}

/// Whether one file can go.
fn evictable(path: &Path, keep: &HashSet<String>, over_cap: bool) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    match classify(name) {
        // Always: nothing reads these whatever key they carry.
        Entry::Retired => true,
        // Never: an unrecognised file is somebody else's, or ours from a
        // version that knew something this one does not.
        Entry::Unknown => false,
        Entry::Cover(key) => over_cap && !keep.contains(key),
        // **Never, and the keep-set cannot answer for these.** A mosaic is
        // named after a playlist's first four *tracks'* covers, and those come
        // from the playlist's own detail page — a playlist can hold songs that
        // are not in your library, so their keys are not in the keep-set and
        // matching against it evicted all four on the first capped sweep.
        //
        // There is no bound to enforce anyway: one per artless playlist, and
        // eight playlists is 2MB. They are also the most expensive thing in
        // the directory to rebuild — four fetches and a compositing pass.
        Entry::Mosaic(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(n: usize) -> String {
        (0..n).map(|i| format!("{i:016x}")).collect()
    }

    #[test]
    fn every_shape_on_disk_is_recognised() {
        // Taken from a real cache directory, one of each.
        assert_eq!(
            classify("005f375e7566101c-320.jpg"),
            Entry::Cover("005f375e7566101c")
        );
        assert_eq!(
            classify("0061ed5f5d6a433f-512.jpg"),
            Entry::Cover("0061ed5f5d6a433f")
        );
        assert_eq!(
            classify("127f514fc028ac76-512.backdrop256.png"),
            Entry::Cover("127f514fc028ac76")
        );
        let name = format!("{}-mosaic-512.png", keys(4));
        assert!(matches!(classify(&name), Entry::Mosaic(parts) if parts.len() == 4));
    }

    #[test]
    fn a_backdrop_follows_the_cover_it_was_blurred_from() {
        // It is derived from that file and useless without it, so it has to
        // carry the same key rather than one of its own.
        assert_eq!(
            classify("abcdef0123456789-512.backdrop256.png"),
            classify("abcdef0123456789-512.jpg")
        );
    }

    #[test]
    fn the_shapes_we_stopped_writing_are_retired() {
        // 256px covers came from the first mosaic build, which fetched at a
        // size nothing else uses. The numberless backdrop predates the size
        // going into the name.
        assert_eq!(classify("abcdef0123456789-256.jpg"), Entry::Retired);
        assert_eq!(
            classify("abcdef0123456789-512.backdrop.png"),
            Entry::Retired
        );
    }

    #[test]
    fn a_mosaic_is_never_mistaken_for_a_cover() {
        // The trap this file exists against: 64 hex characters and a `-512`
        // that a careless split reads as one enormous key, and then bins as
        // unrecognised — losing the most expensive thing in the directory.
        let name = format!("{}-mosaic-512.png", keys(4));
        let Entry::Mosaic(parts) = classify(&name) else {
            panic!("a mosaic was not read as one: {name}");
        };
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0], "0000000000000000");
        assert_eq!(parts[3], "0000000000000003");
    }

    #[test]
    fn a_mosaic_is_never_evicted_even_by_a_keep_set_that_has_never_heard_of_it() {
        // Measured, and it cost a real sweep: matching a mosaic's four keys
        // against the library keep-set removed **all four** mosaics on disk.
        // They are named after a playlist's first four *tracks'* covers, and a
        // playlist can hold songs that are not in your library — so those keys
        // are not in the keep-set and never were.
        let name = format!("{}-mosaic-512.png", keys(4));
        let strangers: HashSet<String> = HashSet::new();
        assert!(!evictable(&PathBuf::from(&name), &strangers, true));
    }

    #[test]
    fn nothing_but_retired_files_goes_while_under_the_cap() {
        let strangers: HashSet<String> = HashSet::new();
        assert!(!evictable(
            &PathBuf::from("abcdef0123456789-320.jpg"),
            &strangers,
            false
        ));
        // Retired shapes do not wait for the cap: nothing can read them.
        assert!(evictable(
            &PathBuf::from("abcdef0123456789-256.jpg"),
            &strangers,
            false
        ));
    }

    #[test]
    fn a_file_we_do_not_recognise_is_left_alone() {
        // Somebody else's, or ours from a version that knew something this one
        // does not. Deleting on "I cannot classify this" is how a cache prune
        // becomes a data loss bug.
        let strangers: HashSet<String> = HashSet::new();
        for name in ["README", "notes.txt", "short-320.jpg", "zzzz-320.jpg"] {
            assert!(
                !evictable(&PathBuf::from(name), &strangers, true),
                "{name} should have been left alone"
            );
        }
    }
}
