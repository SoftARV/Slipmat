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
use relm4::gtk::{gdk, gdk_pixbuf, glib};

use crate::music::types::Artwork;

/// The size we fetch for the Now Playing bar and for MPRIS. One size keeps the
/// cache simple; the Shell scales it down and nobody notices.
pub const ART_SIZE: u32 = 512;

/// `$XDG_CACHE_HOME/slipmat/artwork`, else `~/.cache/slipmat/artwork`.
pub fn cache_dir() -> Option<PathBuf> {
    let base = match std::env::var("XDG_CACHE_HOME") {
        Ok(x) if !x.is_empty() => PathBuf::from(x),
        _ => PathBuf::from(std::env::var("HOME").ok()?).join(".cache"),
    };
    Some(base.join("slipmat/artwork"))
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

/// A cover turned into pixels **off the GTK thread**.
///
/// The decode is the expensive half of showing a cover — measured at 2.5ms for
/// one 320px JPEG — and `gtk_image_set_from_file` does it synchronously on
/// whichever thread calls it. A grid fills 385 tiles in one go (#27), so doing
/// it inline froze the UI for half a second.
///
/// Raw pixels rather than a `gdk::Texture` because a texture is a GObject and
/// therefore not `Send`: it cannot be built on a worker and carried back. A
/// `Vec<u8>` can, and turning one into a `gdk::MemoryTexture` on the main
/// thread is a wrap, not a decode.
pub struct Decoded {
    pixels: Vec<u8>,
    width: i32,
    height: i32,
    stride: usize,
    has_alpha: bool,
}

impl std::fmt::Debug for Decoded {
    /// Without this the pixels would be printed. All of them.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Decoded")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

impl Decoded {
    /// Wrap the pixels as a texture. Main thread, and cheap — no decoding here.
    pub fn into_texture(self) -> gdk::MemoryTexture {
        let format = if self.has_alpha {
            gdk::MemoryFormat::R8g8b8a8
        } else {
            gdk::MemoryFormat::R8g8b8
        };
        gdk::MemoryTexture::new(
            self.width,
            self.height,
            format,
            &glib::Bytes::from_owned(self.pixels),
            self.stride,
        )
    }
}

/// Fetch a tile's cover and decode it, off the GTK thread, reporting whichever
/// half fails.
///
/// The two steps live together because their failures are the same kind of
/// thing — cosmetic, so no toast; a missing cover is not worth interrupting
/// anyone — and because they used to be written as `.ok()` and `and_then` at
/// the call site, which is **silent**. A tile stuck on its placeholder for ever
/// left no trace at all, so a 404 on one cover, a slow network and an
/// undecodable file were indistinguishable from each other and from "it is
/// still loading".
///
/// `key` is only for the log, so a warning here can be lined up with the
/// `tile art delivered` trace on the other side.
pub async fn load_tile(art: Artwork, size: u32, key: &str) -> (Option<PathBuf>, Option<Decoded>) {
    let path = match fetch(art, size).await {
        Ok(path) => Some(path),
        Err(err) => {
            // The error carries the URL it tried — see `fetch`.
            tracing::warn!(%key, ?err, "tile artwork not fetched");
            None
        }
    };
    let decoded = path.as_deref().and_then(|path| {
        let decoded = decode(path, size as i32);
        if decoded.is_none() {
            tracing::warn!(%key, file = %path.display(), "tile artwork on disk but undecodable");
        }
        decoded
    });
    (path, decoded)
}

/// Decode a cover that is already on disk. Call this off the GTK thread.
pub fn decode(path: &Path, size: i32) -> Option<Decoded> {
    let pixbuf = gdk_pixbuf::Pixbuf::from_file_at_scale(path, size, size, true).ok()?;
    Some(Decoded {
        width: pixbuf.width(),
        height: pixbuf.height(),
        stride: pixbuf.rowstride() as usize,
        has_alpha: pixbuf.has_alpha(),
        pixels: pixbuf.read_pixel_bytes().to_vec(),
    })
}

/// Where a mosaic lands, named after the covers it is made of.
///
/// The four cache keys concatenated, rather than the playlist's id, and that is
/// deliberate: it means the file **invalidates itself**. Add a track to the
/// front of a playlist and the first four covers change, so the name changes
/// and a new mosaic is composed; nothing has to notice the edit or expire
/// anything. Keys are sixteen hex characters each, so four of them is a
/// perfectly good filename and no second hash is needed.
fn mosaic_path(arts: &[Artwork], size: u32) -> Option<PathBuf> {
    let name: String = arts.iter().map(Artwork::cache_key).collect();
    Some(cache_dir()?.join(format!("{name}-mosaic-{size}.png")))
}

/// Draw the 2×2 mosaic Apple's web player draws for a playlist you made.
///
/// Apple sends **no artwork at all** for a user-created playlist — measured,
/// and written up in CLAUDE.md — so unlike everything else in this module there
/// is nothing to fetch. There is only something to build, out of covers we are
/// fetching anyway.
///
/// Off the GTK thread with everything else here (rule 8), and cached like
/// everything else here: composed once and found on disk afterwards, including
/// across restarts.
pub async fn mosaic(arts: Vec<Artwork>, size: u32, tile_px: u32) -> Option<PathBuf> {
    let out = mosaic_path(&arts, size)?;
    if out.is_file() {
        return Some(out);
    }
    let started = std::time::Instant::now();

    // **All four at once, and at a size something else already fetches.**
    //
    // Both halves of that were wrong first time round and both were felt as
    // "the page takes a long time". Awaiting them in a loop makes four round
    // trips where one will do; and fetching the exact quadrant size — half the
    // finished mosaic — asks for a size *nothing else in the app uses*, so four
    // covers already sitting on disk at `tile_px` were downloaded again anyway.
    // Passed in rather than assumed here, so the call site is what guarantees
    // it matches the grids.
    let mut tasks = tokio::task::JoinSet::new();
    for (slot, art) in arts.iter().cloned().enumerate() {
        tasks.spawn(async move { (slot, fetch(art, tile_px).await) });
    }

    // Collected by slot: which download finishes first must not decide where a
    // cover lands, or the mosaic reshuffles itself between runs.
    let mut tiles: Vec<Option<PathBuf>> = vec![None; arts.len()];
    while let Some(joined) = tasks.join_next().await {
        let Ok((slot, fetched)) = joined else {
            tracing::warn!("mosaic tile task panicked; no mosaic");
            return None;
        };
        match fetched {
            Ok(path) => tiles[slot] = Some(path),
            Err(err) => {
                tracing::warn!(?err, "mosaic tile not fetched; no mosaic");
                return None;
            }
        }
    }
    let tiles: Vec<PathBuf> = tiles.into_iter().collect::<Option<Vec<_>>>()?;

    let bytes = compose(&tiles, size as i32)?;
    if let Err(err) = write_atomically(&out, &bytes) {
        tracing::warn!(?err, "mosaic not written");
        return None;
    }
    tracing::debug!(
        file = %out.display(),
        tiles = tiles.len(),
        ms = started.elapsed().as_millis(),
        "mosaic composed"
    );
    Some(out)
}

/// Tile `covers` into one square image, top-left to bottom-right, as PNG bytes.
fn compose(covers: &[PathBuf], size: i32) -> Option<Vec<u8>> {
    let half = size / 2;
    let canvas = gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, false, 8, size, size)?;
    // Opaque, so a cover with alpha does not composite onto uninitialised
    // memory — `Pixbuf::new` does not clear what it allocates.
    canvas.fill(0x0000_00ff);

    for (i, path) in covers.iter().enumerate() {
        let x = (i as i32 % 2) * half;
        let y = (i as i32 / 2) * half;
        // `false` for aspect ratio: a quadrant must be exactly square or the
        // seams between the four do not line up. Apple's own artwork is square
        // already, so nothing is being distorted in practice.
        let tile = gdk_pixbuf::Pixbuf::from_file_at_scale(path, half, half, false).ok()?;
        // Scale 1:1 and offset to the quadrant — the tile is already the right
        // size, so this is a blit, clipped to the quadrant's rectangle.
        tile.composite(
            &canvas,
            x,
            y,
            half,
            half,
            f64::from(x),
            f64::from(y),
            1.0,
            1.0,
            gdk_pixbuf::InterpType::Bilinear,
            255,
        );
    }

    // PNG rather than JPEG: this is synthetic, with four hard edges that JPEG
    // would ring around, and it is written once per playlist.
    canvas
        .save_to_bufferv("png", &[])
        .map_err(|err| tracing::warn!(?err, "mosaic not encoded"))
        .ok()
}

/// How wide the drawer's backdrop image is, in pixels. Deliberately tiny.
const BACKDROP_PX: i32 = 48;

/// Write a deliberately tiny copy of a cover beside it, and say where.
///
/// This is the drawer's backdrop, and it is blown up to fill the whole sheet.
/// **That upscale is the blur.** GTK interpolates when it scales a texture, so
/// forty-eight pixels stretched across nine hundred arrives soft on its own —
/// there is no blur pass to write, no shader, and nothing per-frame to pay for.
/// A real Gaussian would mean a CPU convolution on every track change for a
/// result nobody could tell apart once it is behind a scrim.
///
/// Cached like everything else here: the file is written once per cover and
/// found on disk from then on, including across restarts.
///
/// Off the GTK thread (rule 8), on the same trip as [`tint`] — the cover, its
/// colour and its backdrop must never be applied a frame apart.
pub fn backdrop(path: &Path) -> Option<PathBuf> {
    let out = path.with_extension("backdrop.png");
    if out.exists() {
        return Some(out);
    }
    let pixbuf =
        gdk_pixbuf::Pixbuf::from_file_at_scale(path, BACKDROP_PX, BACKDROP_PX, false).ok()?;
    pixbuf.savev(&out, "png", &[]).ok()?;
    Some(out)
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

    /// A solid-colour square on disk, standing in for a cover.
    fn swatch(dir: &Path, name: &str, rgb: u32) -> PathBuf {
        let pixbuf =
            gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, false, 8, 16, 16).unwrap();
        pixbuf.fill(rgb << 8 | 0xff);
        let path = dir.join(format!("{name}.png"));
        pixbuf.savev(&path, "png", &[]).unwrap();
        path
    }

    /// The colour at a pixel, as `0xRRGGBB`.
    fn pixel_at(pixbuf: &gdk_pixbuf::Pixbuf, x: i32, y: i32) -> u32 {
        let bytes = pixbuf.read_pixel_bytes();
        let channels = pixbuf.n_channels() as usize;
        let offset = y as usize * pixbuf.rowstride() as usize + x as usize * channels;
        u32::from(bytes[offset]) << 16
            | u32::from(bytes[offset + 1]) << 8
            | u32::from(bytes[offset + 2])
    }

    #[test]
    fn a_mosaic_puts_each_cover_in_its_own_quadrant() {
        // A real composition, not a mock: gdk-pixbuf needs no GTK main loop, so
        // the arithmetic that decides where each quadrant lands is testable —
        // and getting it wrong is the kind of thing that looks *nearly* right.
        let dir = std::env::temp_dir().join(format!("slipmat-mosaic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let covers = [
            swatch(&dir, "tl", 0xff_0000),
            swatch(&dir, "tr", 0x00_ff00),
            swatch(&dir, "bl", 0x00_00ff),
            swatch(&dir, "br", 0xff_ff00),
        ];
        let png = compose(&covers, 64).unwrap();
        let out = dir.join("mosaic.png");
        write_atomically(&out, &png).unwrap();
        let made = gdk_pixbuf::Pixbuf::from_file(&out).unwrap();

        assert_eq!((made.width(), made.height()), (64, 64));
        // Sampled well inside each quadrant, away from the seams.
        assert_eq!(pixel_at(&made, 16, 16), 0xff_0000, "top left");
        assert_eq!(pixel_at(&made, 48, 16), 0x00_ff00, "top right");
        assert_eq!(pixel_at(&made, 16, 48), 0x00_00ff, "bottom left");
        assert_eq!(pixel_at(&made, 48, 48), 0xff_ff00, "bottom right");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_mosaic_is_named_after_its_covers_so_it_reinvalidates() {
        // Named by content rather than by playlist id, so adding a track to the
        // front of a playlist changes the filename and a new mosaic is composed.
        // Nothing has to notice the edit or expire anything.
        let a = Artwork::new("https://is1.mzstatic.com/a/{w}x{h}bb.jpg");
        let b = Artwork::new("https://is1.mzstatic.com/b/{w}x{h}bb.jpg");

        let one = mosaic_path(&[a.clone(), b.clone()], 512).unwrap();
        let same = mosaic_path(&[a.clone(), b.clone()], 512).unwrap();
        let reordered = mosaic_path(&[b, a], 512).unwrap();

        assert_eq!(one, same, "the same covers must reuse one file");
        assert_ne!(one, reordered, "order is part of the picture");
        assert!(one.to_string_lossy().ends_with("-mosaic-512.png"));
    }

    #[test]
    fn write_atomically_creates_the_directory_and_leaves_no_temp() {
        let dir = std::env::temp_dir().join(format!("slipmat-art-test-{}", std::process::id()));
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
