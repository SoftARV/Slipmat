// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Album art: fetch once, keep on disk.
//!
//! Two reasons this caches to a file rather than holding bytes in memory:
//!
//! 1. **MPRIS needs a path.** `mpris:artUrl` has to be a `file://` URL — the
//!    GNOME Shell applet will not reliably fetch an `https://` one.
//! 2. Apple serves artwork as a *template* (`…/{w}x{h}bb.jpg`), so we request
//!    exactly the pixels the caller needs instead of scaling a 3000px JPEG.
//!
//! Decoding is not here. Turning a JPEG into pixels is a toolkit's job and a
//! terminal wants none of it — but every frontend, and the daemon, needs the
//! same file in the same place.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::music::types::Artwork;
use crate::paths::artwork_dir;

/// The size fetched for a now-playing bar and for MPRIS. One size keeps the
/// cache simple; a Shell scales it down and nobody notices.
pub const ART_SIZE: u32 = 512;

/// Where a cover of this size lands on disk.
pub fn cache_path(art: &Artwork, size: u32) -> Option<PathBuf> {
    Some(artwork_dir()?.join(format!("{}-{size}.jpg", art.cache_key())))
}

/// surfacing an error, because a missing cover is not worth a toast.
pub async fn fetch(art: Artwork, size: u32) -> Result<PathBuf> {
    let path = cache_path(&art, size).context("no cache directory available")?;
    if path.is_file() {
        return Ok(path);
    }

    let url = art.url(size);
    let bytes = reqwest::get(&url)
        .await
        .with_context(|| format!("requesting artwork {url}"))?
        .error_for_status()
        .context("artwork request failed")?
        .bytes()
        .await
        .context("reading artwork body")?;

    write_atomically(&path, &bytes)?;
    Ok(path)
}

/// `rename` within the same directory is atomic.
pub fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context("path has no parent")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;

    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_atomic_write_keeps_the_destination() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "slipmat-atomic-write-{}-{unique}",
            std::process::id()
        ));
        let path = root.join("snapshot.json");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&path, "previous complete snapshot").unwrap();
        std::fs::create_dir(path.with_extension(format!("tmp{}", std::process::id()))).unwrap();

        assert!(write_atomically(&path, b"replacement").is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "previous complete snapshot"
        );

        let _ = std::fs::remove_dir_all(root);
    }
}
