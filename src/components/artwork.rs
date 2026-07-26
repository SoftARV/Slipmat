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
/// Runs off the GTK thread (rule 8): decoding even a small JPEG is
/// milliseconds, and it happens on every track change.
pub fn tint(path: &Path) -> Option<(u8, u8, u8)> {
    // 32x32 is plenty. The question is "what colour is this sleeve", not "what
    // is in it", and scaling down also averages out JPEG noise.
    let pixbuf = gdk_pixbuf::Pixbuf::from_file_at_scale(path, 32, 32, false).ok()?;
    let channels = pixbuf.n_channels() as usize;
    let rowstride = pixbuf.rowstride() as usize;
    let bytes = pixbuf.read_pixel_bytes();

    let mut pixels = Vec::with_capacity(1024);
    for y in 0..pixbuf.height() as usize {
        for x in 0..pixbuf.width() as usize {
            let i = y * rowstride + x * channels;
            match (bytes.get(i), bytes.get(i + 1), bytes.get(i + 2)) {
                (Some(&r), Some(&g), Some(&b)) => pixels.push((r, g, b)),
                _ => break,
            }
        }
    }
    dominant(&pixels)
}

/// The colour a sleeve *reads* as.
///
/// Not the average — averaging album art gives mud, because most of a sleeve is
/// background. And not the single most saturated pixel either, which was the
/// first attempt: one stray red pixel outvoted a cover that was mostly pink and
/// teal, because an argmax over 1024 pixels is decided by noise.
///
/// So: bin by hue, weight each pixel by how colourful it is, and take the
/// heaviest **window of three adjacent bins**. That answers "which colour is
/// there most of", and the window keeps red — which straddles 0° — from
/// splitting its vote across the first and last bin and losing to nothing.
///
/// The hue comes back from the image; the saturation and lightness are given a
/// floor. A tint has to be legible as a colour on a dark bar, and the pooled
/// average of a hue includes every washed-out pixel that happens to share it —
/// which is how an orange sleeve first came back as off-white.
pub(crate) fn dominant(pixels: &[(u8, u8, u8)]) -> Option<(u8, u8, u8)> {
    /// 20° each. Fine enough to separate red from orange, coarse enough that a
    /// gradient across one hue still lands in one bin.
    const BINS: usize = 18;
    /// Below this a pixel is effectively grey and votes for no hue at all.
    const COLOURFUL_ENOUGH: f32 = 0.05;
    /// Floors, so the result reads as a colour rather than as a smudge.
    const MIN_SATURATION: f32 = 0.45;
    const LIGHTNESS: std::ops::RangeInclusive<f32> = 0.35..=0.55;

    if pixels.is_empty() {
        return None;
    }

    let mut weight = [0f32; BINS];
    let (mut hue_sum, mut sat_sum, mut light_sum) = ([0f32; BINS], [0f32; BINS], [0f32; BINS]);
    let mut grey_sum = (0f32, 0f32, 0f32);

    for &(r, g, b) in pixels {
        grey_sum = (
            grey_sum.0 + r as f32,
            grey_sum.1 + g as f32,
            grey_sum.2 + b as f32,
        );

        let (hue, sat, light) = hsl(r, g, b);
        // Near-black and near-white carry no usable colour, and album art is
        // full of both — letterboxing, borders, blown highlights.
        let usable = 1.0 - (light - 0.5).abs() * 1.6;
        if usable <= 0.0 {
            continue;
        }
        let w = sat * usable;
        if w < COLOURFUL_ENOUGH {
            continue;
        }

        let bin = (((hue / 360.0) * BINS as f32) as usize).min(BINS - 1);
        weight[bin] += w;
        hue_sum[bin] += hue * w;
        sat_sum[bin] += sat * w;
        light_sum[bin] += light * w;
    }

    let around = |i: usize| [(i + BINS - 1) % BINS, i, (i + 1) % BINS];
    let best = (0..BINS)
        .map(|i| (i, around(i).iter().map(|&j| weight[j]).sum::<f32>()))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .filter(|(_, total)| *total > 0.0);

    let Some((centre, total)) = best else {
        // A sleeve with no colour in it at all — black and white photography, a
        // plain typographic cover. Its average is the honest answer, and a
        // neutral tint beats no tint.
        let n = pixels.len() as f32;
        return Some((
            (grey_sum.0 / n) as u8,
            (grey_sum.1 / n) as u8,
            (grey_sum.2 / n) as u8,
        ));
    };

    let window = around(centre);

    // The hue comes from the heaviest bin *inside* the window, not from the
    // window's centre — which may hold nothing at all. A cover that is mostly
    // pink with three red pixels puts pink in one bin and red in another, and
    // the window between them wins on their combined weight; taking the centre
    // then invented a hue halfway between and reported the sleeve as red.
    let anchor = window
        .iter()
        .copied()
        .max_by(|a, b| weight[*a].total_cmp(&weight[*b]))
        .unwrap_or(centre);
    let hue = if weight[anchor] > 0.0 {
        hue_sum[anchor] / weight[anchor]
    } else {
        (anchor as f32 + 0.5) * (360.0 / BINS as f32)
    };
    let sat = (window.iter().map(|&i| sat_sum[i]).sum::<f32>() / total).max(MIN_SATURATION);
    let light = (window.iter().map(|&i| light_sum[i]).sum::<f32>() / total)
        .clamp(*LIGHTNESS.start(), *LIGHTNESS.end());

    Some(rgb(hue, sat.min(1.0), light))
}

/// HSL back to RGB.
fn rgb(hue: f32, sat: f32, light: f32) -> (u8, u8, u8) {
    let chroma = (1.0 - (2.0 * light - 1.0).abs()) * sat;
    let h = hue / 60.0;
    let x = chroma * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (chroma, x, 0.0),
        1 => (x, chroma, 0.0),
        2 => (0.0, chroma, x),
        3 => (0.0, x, chroma),
        4 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };
    let m = light - chroma / 2.0;
    let to8 = |v: f32| ((v + m).clamp(0.0, 1.0) * 255.0).round() as u8;
    (to8(r), to8(g), to8(b))
}

/// Hue in degrees, saturation and lightness in 0..=1.
fn hsl(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let chroma = max - min;
    let light = (max + min) / 2.0;

    if chroma == 0.0 {
        return (0.0, 0.0, light);
    }

    let sat = chroma / (1.0 - (2.0 * light - 1.0).abs()).max(f32::EPSILON);
    let hue = if max == r {
        60.0 * (((g - b) / chroma) % 6.0)
    } else if max == g {
        60.0 * ((b - r) / chroma + 2.0)
    } else {
        60.0 * ((r - g) / chroma + 4.0)
    };
    ((hue + 360.0) % 360.0, sat.clamp(0.0, 1.0), light)
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

    /// Repeat a colour `n` times, to stand in for "this much of the sleeve".
    fn field(n: usize, rgb: (u8, u8, u8)) -> Vec<(u8, u8, u8)> {
        vec![rgb; n]
    }

    #[test]
    fn the_colour_there_is_most_of_wins() {
        // The reported bug: a mostly-pink sleeve came back dark red, because
        // the old version took the single most saturated pixel and one stray
        // red beat a whole field of pink.
        let mut art = field(400, (240, 120, 190)); // pink, most of the cover
        art.extend(field(3, (255, 0, 0))); // a few vivid red pixels
        let (r, g, b) = dominant(&art).unwrap();
        // What separates pink from the red we planted is *blue*, not green —
        // the tint deepens lightness to read on a dark bar, so the green
        // channel drops even when the hue is right.
        assert!(
            b > 120 && b > g + 60,
            "expected a pink, got ({r}, {g}, {b})"
        );
    }

    #[test]
    fn black_bars_and_white_borders_do_not_get_a_vote() {
        // Letterboxing and blown highlights are everywhere in album art and
        // carry no usable colour.
        let mut art = field(600, (0, 0, 0));
        art.extend(field(600, (255, 255, 255)));
        art.extend(field(100, (40, 160, 220))); // the only real colour
        let (r, g, b) = dominant(&art).unwrap();
        assert!(b > r, "expected the blue, got ({r}, {g}, {b})");
    }

    #[test]
    fn a_black_and_white_cover_still_gets_a_tint() {
        // No hue anywhere. Its average is the honest answer, and a neutral
        // tint beats no tint at all.
        let art = field(100, (90, 90, 90));
        assert_eq!(dominant(&art), Some((90, 90, 90)));
    }

    #[test]
    fn nothing_in_means_nothing_out() {
        assert_eq!(dominant(&[]), None);
    }

    #[test]
    fn hsl_places_the_primaries_where_they_belong() {
        assert!((hsl(255, 0, 0).0 - 0.0).abs() < 1.0);
        assert!((hsl(0, 255, 0).0 - 120.0).abs() < 1.0);
        assert!((hsl(0, 0, 255).0 - 240.0).abs() < 1.0);
        // Grey has no hue and no saturation, whatever its lightness.
        assert_eq!(hsl(128, 128, 128).1, 0.0);
    }
}
