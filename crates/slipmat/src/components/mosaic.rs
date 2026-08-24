// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The 2x2 cover Slipmat composes for a playlist Apple sends no artwork for.
//!
//! Split from `artwork.rs` because it is the opposite job. Everything there
//! fetches, caches and decodes pictures **Apple sent**; this is the one place
//! the app draws a picture Apple did not — measured, both endpoints: a playlist
//! you made yourself carries no `artwork` object at all, and `music.apple.com`
//! composes the mosaic in its own web player.
//!
//! It still leans on `artwork` for the parts that are genuinely shared — the
//! cache directory, the fetch, the atomic write — because a mosaic's tiles are
//! ordinary covers and must not be fetched or stored a second way.

use std::path::{Path, PathBuf};

use relm4::gtk::gdk_pixbuf;

use super::artwork::{cache_dir, fetch, write_atomically};
use slipmat_core::music::types::Artwork;

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

/// Where each cover goes, for however many there are. `(x, y, w, h)`.
///
/// **A playlist is not guaranteed four albums**, and a grid with holes in it
/// looks broken rather than sparse. So the covers there are divide the square
/// between them instead:
///
/// ```text
///   two            three          four
///  ┌────┬────┐   ┌────┬────┐   ┌────┬────┐
///  │    │    │   │    ├────┤   ├────┼────┤
///  └────┴────┘   └────┴────┘   └────┴────┘
/// ```
///
/// `b` rather than a second `a` so an odd `size` still tiles exactly: a stray
/// pixel column down the middle is the sort of thing nobody sees in a mockup
/// and everybody sees at 2am.
fn layout(count: usize, size: i32) -> Vec<(i32, i32, i32, i32)> {
    let a = size / 2;
    let b = size - a;
    match count {
        0 => Vec::new(),
        1 => vec![(0, 0, size, size)],
        2 => vec![(0, 0, a, size), (a, 0, b, size)],
        3 => vec![(0, 0, a, size), (a, 0, b, a), (a, a, b, b)],
        _ => vec![(0, 0, a, a), (a, 0, b, a), (0, a, a, b), (a, a, b, b)],
    }
}

/// Draw one cover to fill `(x, y, w, h)`, cropping rather than squashing.
///
/// Scaled by whichever axis needs the *most* — "cover", not "contain" — then
/// centred, so the rectangle is filled edge to edge and what falls outside is
/// trimmed evenly. The alternative distorts: a square sleeve stretched into a
/// half-width strip makes everyone on the cover look thin, which reads as a
/// rendering fault rather than as a crop.
fn place(canvas: &gdk_pixbuf::Pixbuf, cover: &Path, x: i32, y: i32, w: i32, h: i32) -> Option<()> {
    let src = gdk_pixbuf::Pixbuf::from_file(cover).ok()?;
    let (sw, sh) = (f64::from(src.width()), f64::from(src.height()));
    if sw <= 0.0 || sh <= 0.0 {
        return None;
    }
    let scale = (f64::from(w) / sw).max(f64::from(h) / sh);
    // Where the scaled cover's own origin lands, so its middle sits over the
    // middle of the rectangle. `composite` clips to the rectangle, so the
    // overhang costs nothing but is what makes the fill exact.
    let offset_x = f64::from(x) + (f64::from(w) - sw * scale) / 2.0;
    let offset_y = f64::from(y) + (f64::from(h) - sh * scale) / 2.0;
    src.composite(
        canvas,
        x,
        y,
        w,
        h,
        offset_x,
        offset_y,
        scale,
        scale,
        gdk_pixbuf::InterpType::Bilinear,
        255,
    );
    Some(())
}

/// Tile `covers` into one square image, as PNG bytes.
fn compose(covers: &[PathBuf], size: i32) -> Option<Vec<u8>> {
    let canvas = gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, false, 8, size, size)?;
    // Opaque, so a cover with alpha does not composite onto uninitialised
    // memory — `Pixbuf::new` does not clear what it allocates.
    canvas.fill(0x0000_00ff);

    for (cover, (x, y, w, h)) in covers.iter().zip(layout(covers.len(), size)) {
        place(&canvas, cover, x, y, w, h)?;
    }

    // PNG rather than JPEG: this is synthetic, with hard edges that JPEG would
    // ring around, and it is written once per playlist.
    canvas
        .save_to_bufferv("png", &[])
        .map_err(|err| tracing::warn!(?err, "mosaic not encoded"))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_mosaic_divides_the_square_however_many_covers_there_are() {
        // A playlist is not guaranteed four albums, and a 2x2 with holes in it
        // looks broken rather than sparse. Whatever there is fills the square.
        for count in 1..=4 {
            let rects = layout(count, 64);
            assert_eq!(rects.len(), count, "one rectangle per cover");
            let area: i32 = rects.iter().map(|(_, _, w, h)| w * h).sum();
            assert_eq!(area, 64 * 64, "{count} covers must leave no gap");
        }
        assert!(layout(0, 64).is_empty());
    }

    #[test]
    fn an_odd_size_still_tiles_exactly() {
        // `size / 2` twice leaves a stray pixel column down the middle — the
        // sort of thing nobody sees in a mockup and everybody sees at 2am.
        for count in 1..=4 {
            let area: i32 = layout(count, 65).iter().map(|(_, _, w, h)| w * h).sum();
            assert_eq!(area, 65 * 65, "{count} covers at an odd size");
        }
    }

    #[test]
    fn two_and_three_covers_land_where_the_diagram_says() {
        let dir = std::env::temp_dir().join(format!("slipmat-layout-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let red = swatch(&dir, "r", 0xff_0000);
        let green = swatch(&dir, "g", 0x00_ff00);
        let blue = swatch(&dir, "b", 0x00_00ff);

        // Two: left half, right half.
        let out = dir.join("two.png");
        write_atomically(&out, &compose(&[red.clone(), green.clone()], 64).unwrap()).unwrap();
        let two = gdk_pixbuf::Pixbuf::from_file(&out).unwrap();
        assert_eq!(pixel_at(&two, 16, 32), 0xff_0000, "left");
        assert_eq!(pixel_at(&two, 48, 32), 0x00_ff00, "right");

        // Three: tall left, then two stacked on the right.
        let out = dir.join("three.png");
        write_atomically(&out, &compose(&[red, green, blue], 64).unwrap()).unwrap();
        let three = gdk_pixbuf::Pixbuf::from_file(&out).unwrap();
        assert_eq!(pixel_at(&three, 16, 32), 0xff_0000, "tall left");
        assert_eq!(pixel_at(&three, 48, 16), 0x00_ff00, "top right");
        assert_eq!(pixel_at(&three, 48, 48), 0x00_00ff, "bottom right");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
