// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Drawing. Borderless: whitespace separates the regions and one accent marks
//! what is playing, what is selected and what is on — the way Apple Music's own
//! interface works.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use slipmat_core::ipc::{Snapshot, Stage};

use crate::browser::{self, Browser};
use crate::queue::{self, Queue};

/// Which pane the arrow keys are talking to.
#[derive(Clone, Copy, PartialEq, Default)]
pub enum Focus {
    /// Where a fresh client starts: the library is what you came to look at.
    #[default]
    Browser,
    Queue,
}

/// Apple Music's red, and the only colour that means anything here.
pub const ACCENT: Color = Color::Rgb(0xFF, 0x4A, 0x5E);
/// Track titles and anything the eye should land on.
pub const BRIGHT: Color = Color::Rgb(0xF2, 0xEC, 0xED);
/// Artists, and the second half of a line.
pub const MUTED: Color = Color::Rgb(0x9A, 0x8E, 0x90);
/// Times, labels, and everything that is only there when looked for.
pub const DIM: Color = Color::Rgb(0x65, 0x5B, 0x5D);

/// The volume meter, which stays small on purpose: it is a state to glance at,
/// not a thing to read along. The progress bar takes the rest of the line.
const VOL: usize = 10;
/// What the transport line spends on things that are not the bar: the play
/// glyph, the gaps either side, and `mm:ss / mm:ss`.
const TRANSPORT_FURNITURE: usize = 22;
/// White, for the top of the bars.
const PEAK: Color = Color::Rgb(0xFF, 0xFF, 0xFF);

pub struct View<'a> {
    pub snap: &'a Snapshot,
    pub stage: &'a Stage,
    pub browser: &'a mut Browser,
    pub queue: &'a mut Queue,
    pub focus: Focus,
    pub typing: bool,
    /// Whether the pane is showing Apple Music rather than the library, which
    /// changes what typing costs and therefore what the hints promise.
    pub catalog: bool,
    pub bars: &'a [f32],
    pub message: Option<&'a str>,
}

pub fn draw(frame: &mut Frame, view: View) {
    // Two cells of margin either side: a terminal player that runs to the edge
    // of the window reads as output rather than as an interface.
    let area = Rect {
        x: frame.area().x + 2,
        y: frame.area().y + 1,
        width: frame.area().width.saturating_sub(4),
        height: frame.area().height.saturating_sub(2),
    };
    // The browser gets the larger share of what is *left over*: it is what you
    // are reading, and the queue is what you already decided. `Fill` rather
    // than `Percentage`, which measures against the whole area — with the
    // fixed rows also claiming their share the two over-subscribe it, and the
    // browser is what gets squeezed, down to a single visible row on a short
    // window.
    let [top, note, queue_head, queue, lib_head, lib, hints] = Layout::vertical([
        Constraint::Length(5),
        Constraint::Length(if view.message.is_some() { 2 } else { 0 }),
        Constraint::Length(2),
        Constraint::Fill(2),
        Constraint::Length(2),
        Constraint::Fill(3),
        Constraint::Length(1),
    ])
    .areas(area);

    frame.render_widget(Paragraph::new(now_playing(&view, area.width as usize)), top);
    if let Some(message) = view.message {
        // Rule 4 reaching the terminal: a request the daemon refused says so
        // rather than looking like a key that did not register.
        frame.render_widget(
            Paragraph::new(spaced(Line::from(Span::styled(
                fit(message, area.width as usize),
                Style::from(ACCENT),
            )))),
            note,
        );
    }

    // The queue sits under the transport, because it is what the transport is
    // moving through. The library is below it, and keeps the larger share:
    // browsing is the reading you do, the queue is the decision already made.
    frame.render_widget(
        Paragraph::new(spaced(queue::header(view.queue))),
        queue_head,
    );
    queue::render(frame, queue, view.queue, view.focus == Focus::Queue);
    frame.render_widget(
        Paragraph::new(spaced(browser::header(view.browser))),
        lib_head,
    );
    browser::render(frame, lib, view.browser, view.focus == Focus::Browser);

    frame.render_widget(
        Paragraph::new(key_hints(
            area.width as usize,
            view.focus,
            view.typing,
            view.catalog,
        )),
        hints,
    );
}

/// A label with its blank line *above* it.
///
/// The gap has to lead, not trail: a section is separated from what comes
/// before it, and putting the blank after the label only looked right while the
/// library happened to be the first pane on screen.
fn spaced(line: Line<'static>) -> Vec<Line<'static>> {
    vec![Line::default(), line]
}

fn now_playing(view: &View, width: usize) -> Vec<Line<'static>> {
    // Anything but Ready has nothing to say about a track, so it says what it
    // is doing instead — a blank player looks broken, a waiting one does not.
    if !matches!(view.stage, Stage::Ready) {
        return vec![Line::from(vec![Span::styled(
            stage_text(view.stage),
            Style::from(MUTED),
        )])];
    }
    if view.snap.title.is_empty() {
        return vec![Line::from(Span::styled(
            "Nothing playing",
            Style::from(MUTED),
        ))];
    }

    // **Under the title, where Winamp put it**, and two rows tall — one row of
    // block characters is only eight heights, and a bar has to travel an eighth
    // of its range before anything changes on screen. That reads as a slow
    // visualiser however fast the data behind it arrives.
    let (upper, lower) = if view.bars.iter().any(|v| *v > 0.0) {
        bars(view.bars, width)
    } else {
        (Vec::new(), Vec::new())
    };

    let title = Line::from(vec![
        Span::styled(
            view.snap.title.clone(),
            Style::from(BRIGHT).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  —  ", Style::from(DIM)),
        Span::styled(view.snap.artist.clone(), Style::from(MUTED)),
    ]);

    // The bar takes the line: a seek bar you can aim at is worth more than
    // whitespace, and every other list here already grows with the window.
    let bar = width.saturating_sub(TRANSPORT_FURNITURE).max(8);
    let transport = Line::from(vec![
        Span::styled(
            if view.snap.playing { "▶" } else { "❚❚" },
            Style::from(ACCENT),
        ),
        Span::raw("   "),
        Span::styled(meter(progress(view.snap), bar), Style::from(ACCENT)),
        Span::styled(rest(progress(view.snap), bar), Style::from(DIM)),
        Span::raw("   "),
        Span::styled(
            format!(
                "{} / {}",
                clock(view.snap.position_ms),
                clock(view.snap.duration_ms)
            ),
            Style::from(DIM),
        ),
    ]);

    let modes = Line::from(vec![
        Span::raw("    "),
        Span::styled("shuffle ", Style::from(DIM)),
        mode(
            view.snap.shuffle,
            if view.snap.shuffle { "on" } else { "off" },
        ),
        Span::styled("   repeat ", Style::from(DIM)),
        mode(
            !matches!(
                view.snap.repeat,
                slipmat_core::player::protocol::RepeatMode::None
            ),
            repeat_text(view.snap),
        ),
        Span::styled("   vol ", Style::from(DIM)),
        Span::styled(meter(view.snap.volume, VOL), Style::from(ACCENT)),
        Span::styled(rest(view.snap.volume, VOL), Style::from(DIM)),
    ]);

    vec![
        title,
        Line::from(upper),
        Line::from(lower),
        transport,
        modes,
    ]
}

/// The always-there row. **Nothing has to be memorised**, which is the whole
/// reason it is always there rather than behind a key.
///
/// It changes with focus, because the keys do: only the queue reorders, only
/// the browser filters. And it is built by priority and stops when the window
/// runs out, so a narrow terminal loses the least useful hint rather than
/// losing the row. Leaving and quitting are reserved from the start — a player
/// you cannot see how to leave is worse than one with no hints at all.
fn key_hints(width: usize, focus: Focus, typing: bool, catalog: bool) -> Line<'static> {
    const LEAVING: [(&str, &str); 2] = [("_", "hide"), ("q", "quit")];

    // While typing, every letter goes into the filter, so advertising the
    // transport keys would be a lie about what the keyboard does.
    let keys: Vec<(&str, &str)> = if typing && catalog {
        // **Say that this one leaves the machine.** Over the library the list
        // narrows as you type; here nothing happens until `↵`, and a box that
        // looks identical while behaving differently has to announce it.
        vec![
            ("↵", "search Apple Music"),
            ("esc", "cancel"),
            ("⌫", "back"),
        ]
    } else if typing {
        vec![("↵", "done"), ("esc", "clear"), ("⌫", "back")]
    } else if focus == Focus::Browser {
        vec![
            ("space", "play/pause"),
            ("↑↓", "move"),
            ("↵", "play/open"),
            ("/", "filter"),
            ("1-5", "section"),
            ("⇥", "pane"),
            ("esc", "back"),
        ]
    } else {
        vec![
            ("space", "play/pause"),
            ("↑↓", "move"),
            ("↵", "play"),
            ("d", "remove"),
            ("KJ", "reorder"),
            ("⇥", "pane"),
            ("zb", "prev/next"),
        ]
    };

    let cost = |(key, what): &(&str, &str)| key.chars().count() + what.chars().count() + 4;
    let reserved: usize = LEAVING.iter().map(cost).sum();

    let mut spans = Vec::new();
    let mut used = 0;
    for pair in keys.iter().copied().chain(LEAVING) {
        let optional = !LEAVING.contains(&pair);
        if optional {
            if used + cost(&pair) + reserved > width {
                continue;
            }
            used += cost(&pair);
        }
        let (key, what) = pair;
        spans.push(Span::styled(
            key,
            Style::from(MUTED).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!(" {what}   "), Style::from(DIM)));
    }
    Line::from(spans)
}

/// Cut `text` to `width` and pad it out, so a column stays a column.
///
/// Counts characters rather than bytes: an accented artist name is one column
/// per `char` here, and slicing by byte would split it.
pub fn fit(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let count = text.chars().count();
    if count <= width {
        return format!("{text:<width$}");
    }
    let kept: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{kept}… ")
}

/// One cell per column across two rows, so a level has sixteen steps rather
/// than eight, and coloured from the accent at the bottom to white at the top.
///
/// Returns the upper row and the lower one, a span per cell — a block character
/// takes one colour, so a gradient has to be built out of cells rather than
/// painted over a string.
fn bars(levels: &[f32], width: usize) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
    const STEPS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let mut upper = Vec::with_capacity(width);
    let mut lower = Vec::with_capacity(width);
    for v in columns(levels, width) {
        let sixteenths = (v.clamp(0.0, 1.0) * 16.0).round() as usize;
        // The lower row fills first; the upper one only once the lower is full.
        let below = sixteenths.min(8);
        let above = sixteenths.saturating_sub(8);
        // Zero is a space, not the shortest block: a floor of stubs across the
        // whole width reads as a broken meter rather than as silence.
        lower.push(cell(below, ACCENT, blend(ACCENT, PEAK, 0.5), &STEPS));
        upper.push(cell(above, blend(ACCENT, PEAK, 0.5), PEAK, &STEPS));
    }
    (upper, lower)
}

/// One cell of a bar: how full it is picks both the glyph and, between `foot`
/// and `head`, its colour — so a bar that only just reaches a row is still the
/// colour of the row below it.
fn cell(eighths: usize, foot: Color, head: Color, steps: &[char; 8]) -> Span<'static> {
    if eighths == 0 {
        return Span::raw(" ");
    }
    let t = eighths as f32 / steps.len() as f32;
    Span::styled(
        steps[eighths - 1].to_string(),
        Style::from(blend(foot, head, t)),
    )
}

fn blend(from: Color, to: Color, t: f32) -> Color {
    let (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) = (from, to) else {
        return from;
    };
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t.clamp(0.0, 1.0)) as u8;
    Color::Rgb(mix(ar, br), mix(ag, bg), mix(ab, bb))
}

/// Fold the analysed bands into the columns actually on screen.
///
/// **The peak, not the average.** Averaging a band into a wider window is what
/// turns a spectrum into a smooth hump: the loud thing is the one worth seeing,
/// and it is the one an average throws away.
fn columns(levels: &[f32], width: usize) -> Vec<f32> {
    if width == 0 || levels.is_empty() {
        return Vec::new();
    }
    (0..width)
        .map(|c| {
            let from = c * levels.len() / width;
            let to = ((c + 1) * levels.len() / width)
                .max(from + 1)
                .min(levels.len());
            levels[from..to].iter().fold(0f32, |m, v| m.max(*v))
        })
        .collect()
}

fn mode(on: bool, text: &str) -> Span<'static> {
    Span::styled(
        text.to_owned(),
        if on {
            Style::from(ACCENT)
        } else {
            Style::from(DIM)
        },
    )
}

fn repeat_text(snap: &Snapshot) -> &'static str {
    use slipmat_core::player::protocol::RepeatMode;
    match snap.repeat {
        RepeatMode::None => "off",
        RepeatMode::All => "all",
        RepeatMode::One => "one",
    }
}

fn progress(snap: &Snapshot) -> f64 {
    if snap.duration_ms == 0 {
        return 0.0;
    }
    (snap.position_ms as f64 / snap.duration_ms as f64).clamp(0.0, 1.0)
}

fn meter(fraction: f64, width: usize) -> String {
    "█".repeat(filled(fraction, width))
}

fn rest(fraction: f64, width: usize) -> String {
    "░".repeat(width - filled(fraction, width))
}

fn filled(fraction: f64, width: usize) -> usize {
    ((fraction.clamp(0.0, 1.0) * width as f64).round() as usize).min(width)
}

/// `m:ss`, or `h:mm:ss` for the rare track that needs it.
pub fn clock(ms: u64) -> String {
    let total = ms / 1000;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

fn stage_text(stage: &Stage) -> String {
    match stage {
        Stage::Connecting => "Starting the playback engine…".into(),
        Stage::SignedOut => "Not signed in — open Slipmat to sign in".into(),
        Stage::Broken { detail } => format!("Playback unavailable: {detail}"),
        Stage::Ready => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_silent_bar_is_a_space_rather_than_a_stub() {
        // A row of ▁ across the whole width reads as a broken meter; silence
        // should read as nothing at all.
        let glyphs = |(up, low): (Vec<Span>, Vec<Span>)| {
            let text = |r: Vec<Span>| r.iter().map(|s| s.content.to_string()).collect::<String>();
            (text(up), text(low))
        };
        assert_eq!(glyphs(bars(&[0.0, 0.0], 2)), ("  ".into(), "  ".into()));
        // Full height fills both rows; half fills only the lower one.
        assert_eq!(glyphs(bars(&[1.0], 1)), ("█".into(), "█".into()));
        assert_eq!(glyphs(bars(&[0.5], 1)), (" ".into(), "█".into()));
    }

    #[test]
    fn the_bars_take_the_width_they_are_given() {
        // Both directions: more columns than bands, and fewer.
        let bands = [0.1, 0.9, 0.2, 0.8];
        for width in [1usize, 3, 4, 12, 80] {
            let (up, low) = bars(&bands, width);
            assert_eq!(up.len(), width, "upper row at {width}");
            assert_eq!(low.len(), width, "lower row at {width}");
        }
    }

    #[test]
    fn folding_bands_keeps_the_peak_rather_than_the_average() {
        // A loud band next to quiet ones must survive the fold, or every
        // spectrum flattens into the same hump as the window narrows.
        assert_eq!(columns(&[0.0, 1.0, 0.0, 0.0], 2), vec![1.0, 0.0]);
    }

    #[test]
    fn a_clock_grows_a_field_only_when_it_needs_one() {
        assert_eq!(clock(0), "0:00");
        assert_eq!(clock(63_000), "1:03");
        assert_eq!(clock(3_723_000), "1:02:03");
    }

    #[test]
    fn a_meter_and_its_remainder_always_fill_the_width() {
        // Drawn as two spans in different colours, so a rounding disagreement
        // between them would make the bar change length as it played. Checked
        // at several widths now that it grows with the window.
        for width in [8usize, 25, 60, 137] {
            for pct in 0..=100 {
                let f = pct as f64 / 100.0;
                assert_eq!(
                    meter(f, width).chars().count() + rest(f, width).chars().count(),
                    width,
                    "at {pct}% and width {width}"
                );
            }
        }
    }

    #[test]
    fn a_track_with_no_duration_reads_as_empty_rather_than_full() {
        // MusicKit reports zero before it knows, and dividing by it would give
        // a bar that starts full and shrinks.
        let snap = Snapshot {
            duration_ms: 0,
            position_ms: 5_000,
            ..Default::default()
        };
        assert_eq!(progress(&snap), 0.0);
    }
}
