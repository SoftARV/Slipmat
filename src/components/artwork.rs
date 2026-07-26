// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Album art: fetch once, keep on disk.
//!
//! Two reasons this caches to a file rather than holding bytes in memory:
//!
//! 1. **MPRIS needs a path.** `mpris:artUrl` has to be a `file://` URL — the
//!    GNOME Shell applet will not reliably fetch an `https://` one. M3 depends
//!    on this, which is why it is built now rather than alongside MPRIS.
//! 2. Apple serves artwork as a *template* (`…/{w}x{h}bb.jpg`), so we request
//!    exactly the pixels the widget needs instead of scaling a 3000px JPEG.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use relm4::gtk::gdk_pixbuf;

use crate::music::types::Artwork;

/// The size we fetch for the Now Playing bar and for MPRIS. One size keeps the
/// cache simple; the Shell scales it down and nobody notices.
pub const ART_SIZE: u32 = 512;

/// `$XDG_CACHE_HOME/tonearm/artwork`, else `~/.cache/tonearm/artwork`.
pub fn cache_dir() -> Option<PathBuf> {
    let base = match std::env::var("XDG_CACHE_HOME") {
        Ok(x) if !x.is_empty() => PathBuf::from(x),
        _ => PathBuf::from(std::env::var("HOME").ok()?).join(".cache"),
    };
    Some(base.join("tonearm/artwork"))
}

fn cache_path(art: &Artwork, size: u32) -> Option<PathBuf> {
    Some(cache_dir()?.join(format!("{}-{size}.jpg", art.cache_key())))
}

/// Return a local path for `art`, downloading it only if we don't have it.
///
/// Runs off the GTK thread as a relm4 command (rule 8). A failure here is
/// cosmetic — the caller falls back to a placeholder icon rather than
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

/// Write via a temporary file and rename.
///
/// Two processes can race here — the app and, later, a second instance — and a
/// half-written JPEG that `gdk::Texture` chokes on is worse than a slow one.
/// `rename` within the same directory is atomic.
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context("artwork path has no parent")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;

    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))?;
    Ok(())
}

/// A colour to tint the Now Playing bar with, taken from a cover.
///
/// Not the average — averaging a cover gives mud, because most album art is
/// mostly background. This picks the most *colourful* pixel at a usable
/// lightness, which is what the eye reads as "the colour of that sleeve".
///
/// Runs off the GTK thread (rule 8): decoding even a small JPEG is milliseconds,
/// and it happens on every track change.
pub fn tint(path: &Path) -> Option<(u8, u8, u8)> {
    // 32x32 is plenty. The question is "what colour is this", not "what is in
    // it", and scaling down is also a cheap way to average out noise.
    let pixbuf = gdk_pixbuf::Pixbuf::from_file_at_scale(path, 32, 32, false).ok()?;
    let channels = pixbuf.n_channels() as usize;
    let rowstride = pixbuf.rowstride() as usize;
    let width = pixbuf.width() as usize;
    let height = pixbuf.height() as usize;
    let bytes = pixbuf.read_pixel_bytes();

    let mut best: Option<(f32, (u8, u8, u8))> = None;
    let mut sum = (0u32, 0u32, 0u32);
    let mut counted = 0u32;

    for y in 0..height {
        for x in 0..width {
            let i = y * rowstride + x * channels;
            let (r, g, b) = (*bytes.get(i)?, *bytes.get(i + 1)?, *bytes.get(i + 2)?);
            sum = (sum.0 + r as u32, sum.1 + g as u32, sum.2 + b as u32);
            counted += 1;

            let (rf, gf, bf) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
            let max = rf.max(gf).max(bf);
            let min = rf.min(gf).min(bf);
            let lightness = (max + min) / 2.0;
            let chroma = max - min;

            // Penalise the very dark and the very pale: black bars and white
            // borders are colourful in neither sense, and both are everywhere
            // in album art.
            let usable = 1.0 - (lightness - 0.5).abs() * 1.6;
            let score = chroma * usable.max(0.0);
            if score > best.map(|(s, _)| s).unwrap_or(0.0) {
                best = Some((score, (r, g, b)));
            }
        }
    }

    // A sleeve with no colour at all — a black-and-white cover — still gets a
    // tint, just a neutral one, rather than nothing.
    match best {
        Some((score, rgb)) if score > 0.08 => Some(rgb),
        _ if counted > 0 => Some((
            (sum.0 / counted) as u8,
            (sum.1 / counted) as u8,
            (sum.2 / counted) as u8,
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_paths_are_stable_and_size_specific() {
        let art = Artwork::new("https://is1.mzstatic.com/image/thumb/x/{w}x{h}bb.jpg");
        let a = cache_path(&art, 512).unwrap();
        let b = cache_path(&art, 512).unwrap();
        let c = cache_path(&art, 64).unwrap();

        assert_eq!(a, b, "same art and size must reuse one file");
        assert_ne!(a, c, "different sizes must not collide");
        assert!(a.to_string_lossy().ends_with("-512.jpg"));
    }

    #[test]
    fn different_art_does_not_share_a_file() {
        let a = Artwork::new("https://is1.mzstatic.com/a/{w}x{h}bb.jpg");
        let b = Artwork::new("https://is1.mzstatic.com/b/{w}x{h}bb.jpg");
        assert_ne!(cache_path(&a, 512), cache_path(&b, 512));
    }

    #[test]
    fn write_atomically_creates_the_directory_and_leaves_no_temp() {
        let dir = std::env::temp_dir().join(format!("tonearm-art-test-{}", std::process::id()));
        let target = dir.join("nested/cover.jpg");
        write_atomically(&target, b"not really a jpeg").unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"not really a jpeg");
        let strays: Vec<_> = std::fs::read_dir(target.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("tmp"))
            .collect();
        assert!(strays.is_empty(), "temp file left behind");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
