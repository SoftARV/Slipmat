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
pub(super) fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
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

/// How wide the drawer's backdrop image is, in pixels.
///
/// Big enough that the upscale to the sheet is a few times rather than twenty,
/// because **GTK does not interpolate this smoothly**. See [`backdrop`].
const BACKDROP_PX: i32 = 256;

/// Radius of the blur, as a fraction of [`BACKDROP_PX`].
///
/// **Small, and paired with [`SATURATION`].** Two earlier attempts got this
/// wrong in the same direction. `/16` was chosen so that "no feature of the
/// sleeve survives", which is true and produced a flat grey panel; `/32` was
/// barely distinguishable from it.
///
/// The reason a wide blur fails is not softness, it is *chroma*. Averaging
/// pulls colour toward neutral, and a sleeve of alternating complementary
/// colours — concentric red, green and blue rings, in the case that exposed
/// this — averages to grey once the window spans several of them. No amount of
/// saturation afterwards recovers that, because there is no chroma left to
/// multiply.
///
/// So: blur just enough to destroy legible detail, and put the colour back
/// with saturation rather than trying to keep it through a wide average.
const BLUR_RADIUS: usize = BACKDROP_PX as usize / 64;

/// How much to lift the colour after blurring, as a percentage.
///
/// Blurring desaturates — it is an average, and averages tend to the middle —
/// so a backdrop that is *only* blurred reads as grey behind a light veil. This
/// puts back what the average took, which is why every platform's blurred
/// backdrop is saturated rather than plain.
///
/// Applied after the blur, deliberately: saturating first would amplify the
/// noise the blur is about to smear.
const SATURATION: u32 = 190;

/// Write a blurred copy of a cover beside it, and say where.
///
/// This is the drawer's backdrop, blown up to fill the whole sheet.
///
/// **The upscale is not the blur, which is what this used to claim.** The old
/// version stored 48px and let CSS stretch it, on the reasoning that GTK
/// interpolates when it scales a texture and a real Gaussian would be "a CPU
/// convolution on every track change for a result nobody could tell apart".
/// Both halves were wrong. GTK stretches this one nearest-neighbour, so 48px
/// across ~950 arrived as **twenty-pixel squares with hard edges** — reported
/// unprompted, in both themes, as "pixelated". And a backdrop is written once
/// per cover and cached, so the convolution is not per track change; it is per
/// cover, ever, on a worker thread.
///
/// So the blur is real now, and the stored image is larger for a second reason:
/// with nearest-neighbour upscaling the block size is the ratio, so 256px
/// leaves blocks of about four pixels where 48px left twenty. Blurring alone
/// would have smoothed their *colour* while leaving their edges.
///
/// Cached like everything else here, and keyed by size — changing
/// [`BACKDROP_PX`] invalidates every stored backdrop by construction, rather
/// than leaving the old geometry on disk to be found later.
///
/// Off the GTK thread (rule 8).
pub fn backdrop(path: &Path) -> Option<PathBuf> {
    let out = path.with_extension(format!("backdrop{BACKDROP_PX}.png"));
    if out.exists() {
        return Some(out);
    }
    let pixbuf =
        gdk_pixbuf::Pixbuf::from_file_at_scale(path, BACKDROP_PX, BACKDROP_PX, false).ok()?;

    let (w, h) = (pixbuf.width() as usize, pixbuf.height() as usize);
    let channels = pixbuf.n_channels() as usize;
    let stride = pixbuf.rowstride() as usize;
    let mut pixels = pixbuf.read_pixel_bytes().to_vec();
    blur(&mut pixels, w, h, channels, stride, BLUR_RADIUS);
    saturate(&mut pixels, w, h, channels, stride, SATURATION);

    let blurred = gdk_pixbuf::Pixbuf::from_bytes(
        &glib::Bytes::from_owned(pixels),
        pixbuf.colorspace(),
        pixbuf.has_alpha(),
        pixbuf.bits_per_sample(),
        pixbuf.width(),
        pixbuf.height(),
        pixbuf.rowstride(),
    );
    blurred.savev(&out, "png", &[]).ok()?;
    Some(out)
}

/// Lift every pixel's colour away from its own brightness.
///
/// `percent` is 100 for no change. Each channel moves away from the pixel's
/// luma by that factor, which is the standard saturation adjust: it leaves
/// greys alone — they have no chroma to lift — and makes coloured pixels more
/// so, without changing how light or dark they are.
fn saturate(pixels: &mut [u8], w: usize, h: usize, channels: usize, stride: usize, percent: u32) {
    if percent == 100 || channels < 3 {
        return;
    }
    for y in 0..h {
        for x in 0..w {
            let i = y * stride + x * channels;
            let [r, g, b] = [pixels[i], pixels[i + 1], pixels[i + 2]];
            // Rec. 709 luma: green carries most of perceived brightness, which
            // is why an unweighted mean would shift the picture's lightness.
            let luma = (2126 * u32::from(r) + 7152 * u32::from(g) + 722 * u32::from(b)) / 10000;
            for (c, v) in [r, g, b].into_iter().enumerate() {
                let lifted = i32::from(luma as u8)
                    + (i32::from(v) - i32::from(luma as u8)) * percent as i32 / 100;
                pixels[i + c] = lifted.clamp(0, 255) as u8;
            }
        }
    }
}

/// Three box passes, which is a close enough Gaussian and far cheaper.
///
/// Separable and running-sum, so the cost is O(pixels) per pass rather than
/// O(pixels x radius) — at 256px that is under a millisecond, once per cover,
/// off the GTK thread. Pure, so the tests can check it rather than trusting it.
fn blur(pixels: &mut [u8], w: usize, h: usize, channels: usize, stride: usize, radius: usize) {
    if radius == 0 || w == 0 || h == 0 {
        return;
    }
    for _ in 0..3 {
        box_pass(pixels, w, h, channels, stride, radius, true);
        box_pass(pixels, w, h, channels, stride, radius, false);
    }
}

/// One box blur along one axis. `horizontal` picks which.
fn box_pass(
    pixels: &mut [u8],
    w: usize,
    h: usize,
    channels: usize,
    stride: usize,
    radius: usize,
    horizontal: bool,
) {
    let (lines, len) = if horizontal { (h, w) } else { (w, h) };
    let at = |line: usize, i: usize, c: usize| {
        if horizontal {
            line * stride + i * channels + c
        } else {
            i * stride + line * channels + c
        }
    };
    let mut row = vec![0u8; len];
    for line in 0..lines {
        for c in 0..channels {
            for (i, slot) in row.iter_mut().enumerate() {
                *slot = pixels[at(line, i, c)];
            }
            // Running sum over a window clamped to the edges, so the border
            // does not darken — a black fringe around a backdrop is exactly
            // the sort of thing that reads as a rendering fault.
            let primed = radius.min(len - 1) + 1;
            let mut sum: u32 = row[..primed].iter().map(|v| u32::from(*v)).sum();
            let mut count = primed as u32;
            for i in 0..len {
                pixels[at(line, i, c)] = (sum / count) as u8;
                if let Some(add) = row.get(i + radius + 1) {
                    sum += u32::from(*add);
                    count += 1;
                }
                if radius <= i {
                    sum -= u32::from(row[i - radius]);
                    count -= 1;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-channel image, so the blur maths is readable in the assertions.
    fn grey(values: &[u8], w: usize) -> (Vec<u8>, usize, usize) {
        (values.to_vec(), w, values.len() / w)
    }

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
    fn saturating_leaves_grey_alone_and_lifts_colour() {
        // Grey has no chroma to lift, so it must come back untouched — a
        // saturation pass that drifts neutrals would tint the whole backdrop.
        let mut grey = vec![128, 128, 128];
        saturate(&mut grey, 1, 1, 3, 3, 190);
        assert_eq!(grey, vec![128, 128, 128], "grey should not move");

        // A muted red should get redder without getting lighter or darker.
        let mut red = vec![150, 100, 100];
        saturate(&mut red, 1, 1, 3, 3, 190);
        assert!(red[0] > 150, "red channel did not lift: {red:?}");
        assert!(red[1] < 100 && red[2] < 100, "others did not fall: {red:?}");
    }

    #[test]
    fn saturating_cannot_overflow_a_channel() {
        // Clamping is the whole risk here: an already-vivid pixel lifted 90%
        // would wrap without it, turning a bright colour into its opposite.
        let mut vivid = vec![250, 5, 5, 0, 0, 0];
        saturate(&mut vivid, 2, 1, 3, 6, 400);
        // Both directions wrap without the clamp: the bright channel runs past
        // 255 and the dim ones go negative, which on a u8 comes back as a very
        // bright value — a vivid red would return as its own opposite.
        assert_eq!(vivid[0], 255, "bright channel should clamp to full");
        assert_eq!(&vivid[1..3], &[0, 0], "dim channels wrapped: {vivid:?}");
    }

    #[test]
    fn a_hundred_percent_saturation_is_a_no_op() {
        let mut px = vec![10, 20, 30, 40, 50, 60];
        let before = px.clone();
        saturate(&mut px, 2, 1, 3, 6, 100);
        assert_eq!(px, before);
    }

    #[test]
    fn a_blur_spreads_a_spike_and_keeps_the_total_brightness() {
        // One bright pixel in a dark field. After blurring it must be dimmer
        // and its neighbours brighter — that is the whole job.
        let (mut px, w, h) = grey(&[0, 0, 0, 0, 0, 0, 0, 255, 0, 0, 0, 0, 0, 0, 0], 15);
        let before = px[7];
        blur(&mut px, w, h, 1, w, 2);

        assert!(px[7] < before, "the spike did not spread");
        assert!(px[6] > 0 && px[8] > 0, "neighbours got nothing");
        assert!(
            px.iter().map(|v| u32::from(*v)).sum::<u32>() > 200,
            "the blur threw the light away instead of spreading it"
        );
    }

    #[test]
    fn a_flat_image_survives_the_blur_unchanged() {
        // The edge clamp is the reason this is worth a test: a window that
        // counted off-image pixels as zero would darken every border, and a
        // black fringe around a backdrop reads as a rendering fault.
        let (mut px, w, h) = grey(&[200; 64], 8);
        blur(&mut px, w, h, 1, w, 3);
        assert!(
            px.iter().all(|v| *v == 200),
            "a flat image should blur to itself, got {px:?}"
        );
    }

    #[test]
    fn a_zero_radius_is_a_no_op() {
        let (mut px, w, h) = grey(&[1, 2, 3, 4], 2);
        let before = px.clone();
        blur(&mut px, w, h, 1, w, 0);
        assert_eq!(px, before);
    }

    #[test]
    fn the_backdrop_filename_carries_its_size() {
        // Changing BACKDROP_PX must invalidate what is already on disk. The old
        // scheme wrote plain `.backdrop.png`, so a geometry change would have
        // left 48px files to be found and reused for ever.
        let name = std::path::Path::new("/tmp/abc-512.jpg")
            .with_extension(format!("backdrop{BACKDROP_PX}.png"));
        assert!(
            name.to_string_lossy()
                .ends_with(&format!("backdrop{BACKDROP_PX}.png"))
        );
        assert_ne!(name.to_string_lossy(), "/tmp/abc-512.backdrop.png");
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
