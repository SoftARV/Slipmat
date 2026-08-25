// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Colours, chosen against the terminal actually in use.
//!
//! **The neutrals were the problem, not the accent.** They were fixed RGB tuned
//! against a neutral dark grey, so on a dark *blue* terminal the greys read as
//! warm and muddy and everything looked pasted on from somewhere else. A grey
//! with a slight bias toward the background reads as chosen; a fixed one reads
//! as borrowed.
//!
//! So the background is asked for, once, and the greys are mixed from it toward
//! the foreground. The accent stays Apple Music's red — that is the identity,
//! and it is what should sit *on* the theme rather than dissolve into it. The
//! spectrum's green-to-orange ramp is fixed for the same reason, and lives with
//! the drawing rather than here.

use std::io::{Read, Write};
use std::sync::OnceLock;

use ratatui::style::Color;

/// How long to wait for the terminal to answer. It is a round trip to a
/// program, not a network, and a terminal that does not implement the query
/// will never answer at all — so this is the cost of asking, paid once.
const REPLY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(120);

/// Apple Music's red, and the only colour here that is not derived.
const APPLE_RED: Color = Color::Rgb(0xFF, 0x4A, 0x5E);
/// The neutral dark this was originally tuned against, and the fallback when a
/// terminal will not say what it is using.
const ASSUMED_BG: Color = Color::Rgb(0x14, 0x12, 0x13);

pub struct Palette {
    pub accent: Color,
    pub bright: Color,
    pub muted: Color,
    pub dim: Color,
}

static PALETTE: OnceLock<Palette> = OnceLock::new();

pub fn accent() -> Color {
    palette().accent
}
pub fn bright() -> Color {
    palette().bright
}
pub fn muted() -> Color {
    palette().muted
}
pub fn dim() -> Color {
    palette().dim
}

fn palette() -> &'static Palette {
    PALETTE.get_or_init(|| build(ASSUMED_BG))
}

/// Ask the terminal what it is, and settle the palette.
///
/// Call once, after raw mode is on — the reply arrives on stdin as an escape
/// sequence, and in cooked mode it would be echoed and line-buffered instead.
/// Calling it late, or not at all, is not an error: the fallback is what this
/// looked like before.
pub fn detect() {
    let _ = PALETTE.set(build(background().unwrap_or(ASSUMED_BG)));
}

fn build(bg: Color) -> Palette {
    // Light or dark decides which way the text goes; the *hue* of the ground is
    // what the greys are mixed from, and it is the part that was missing.
    let fg = if luminance(bg) > 0.5 {
        Color::Rgb(0x0E, 0x0E, 0x10)
    } else {
        Color::Rgb(0xFF, 0xFF, 0xFF)
    };
    Palette {
        accent: APPLE_RED,
        bright: mix(bg, fg, 0.95),
        muted: mix(bg, fg, 0.62),
        dim: mix(bg, fg, 0.34),
    }
}

fn luminance(c: Color) -> f32 {
    let Color::Rgb(r, g, b) = c else { return 0.0 };
    (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) / 255.0
}

pub fn mix(from: Color, to: Color, t: f32) -> Color {
    let (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) = (from, to) else {
        return from;
    };
    let f = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t.clamp(0.0, 1.0)) as u8;
    Color::Rgb(f(ar, br), f(ag, bg), f(ab, bb))
}

/// `OSC 11` — "what is your background?". Widely implemented and harmless where
/// it is not: a terminal that does not know the query simply never replies, so
/// this costs the timeout and nothing else.
fn background() -> Option<Color> {
    let mut out = std::io::stdout();
    out.write_all(b"\x1b]11;?\x07").ok()?;
    out.flush().ok()?;
    parse(&read_reply()?)
}

fn read_reply() -> Option<String> {
    use std::os::unix::io::AsRawFd;
    let stdin = std::io::stdin();
    let fd = stdin.as_raw_fd();
    let deadline = std::time::Instant::now() + REPLY_TIMEOUT;
    let mut buf = Vec::new();

    while std::time::Instant::now() < deadline {
        // `poll` rather than a blocking read: a terminal that ignores the query
        // would otherwise hang the program before it has drawn anything.
        let mut fds = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if unsafe { libc::poll(&mut fds, 1, left.as_millis() as i32) } <= 0 {
            break;
        }
        let mut chunk = [0u8; 64];
        match stdin.lock().read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
        // The reply ends at BEL or ST; anything after is somebody's keystroke.
        if buf.contains(&0x07) || buf.windows(2).any(|w| w == b"\x1b\\") {
            break;
        }
    }
    (!buf.is_empty()).then(|| String::from_utf8_lossy(&buf).into_owned())
}

/// `…rgb:1a1a/1b1b/2626…` — components are 1 to 4 hex digits each, scaled to
/// the width they are given rather than assumed to be 16-bit.
fn parse(reply: &str) -> Option<Color> {
    let rest = reply.split("rgb:").nth(1)?;
    let mut parts = rest.split('/');
    let mut channel = || {
        let raw: String = parts
            .next()?
            .chars()
            .take_while(|c| c.is_ascii_hexdigit())
            .collect();
        let value = u32::from_str_radix(&raw, 16).ok()?;
        let full = (1u32 << (4 * raw.len() as u32)) - 1;
        Some((value * 255 / full.max(1)) as u8)
    };
    Some(Color::Rgb(channel()?, channel()?, channel()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_is_read_at_whatever_width_it_comes_in() {
        // xterm answers in 16-bit, some terminals in 8. Treating the second as
        // the first makes every colour almost black, which would flip the
        // light/dark decision on a white terminal.
        assert_eq!(
            parse("\x1b]11;rgb:ffff/ffff/ffff\x07"),
            Some(Color::Rgb(255, 255, 255))
        );
        assert_eq!(
            parse("\x1b]11;rgb:ff/00/80\x07"),
            Some(Color::Rgb(255, 0, 128))
        );
        assert_eq!(parse("no reply at all"), None);
    }

    #[test]
    fn the_greys_take_the_hue_of_the_ground() {
        // The whole point: on a blue terminal the dim text is blue-grey, not
        // the warm grey that was hardcoded.
        let blue = build(Color::Rgb(0x0A, 0x12, 0x2A));
        let Color::Rgb(r, _, b) = blue.dim else {
            panic!("dim is not rgb")
        };
        assert!(
            b > r,
            "dim {:?} did not lean toward the background",
            blue.dim
        );
    }

    #[test]
    fn a_light_terminal_gets_dark_text() {
        let light = build(Color::Rgb(0xFA, 0xFA, 0xF8));
        assert!(luminance(light.bright) < 0.5, "text would vanish on white");
    }
}
