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
use crate::theme::{
    accent as ACCENT, bright as BRIGHT, dim as DIM, mix, muted as MUTED, peak as PEAK,
};

/// Which list the one pane is showing.
///
/// **One pane, not two.** A permanently visible queue costs rows every moment
/// it is not being read, and on a short window it and the library were both too
/// small to use. The queue is somewhere you go — a tab beside the others,
/// reachable in one key and left the same way — which is how the GNOME client
/// treats it too.
#[derive(Clone, Copy, PartialEq, Default)]
pub enum Pane {
    /// Where a fresh client starts: the library is what you came to look at.
    #[default]
    Browser,
    Queue,
}

/// The volume meter.
///
/// **Twenty, so a step is a cell.** The keys move volume by 5%, and over ten
/// cells that is half a cell — `filled` rounds, so every other press changed
/// nothing on screen and the whole control felt like it was missing keystrokes.
const VOL: usize = 20;
/// How much of the window the spectrum may take, and the bounds either side.
/// Two rows is enough to read; more is where the gradient becomes visible.
const SPECTRUM_SHARE: u16 = 6;
const SPECTRUM_MIN: usize = 2;
const SPECTRUM_MAX: usize = 6;

pub struct View<'a> {
    pub snap: &'a Snapshot,
    pub stage: &'a Stage,
    pub browser: &'a mut Browser,
    pub queue: &'a mut Queue,
    pub pane: Pane,
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
    let rows = spectrum_rows(area.height);
    // name + title + album + a blank + spectrum + a blank + seek + state + modes.
    let band = 4 + rows as u16 + 4;
    let [top, note, tabs, list, hints] = Layout::vertical([
        Constraint::Length(band),
        Constraint::Length(if view.message.is_some() { 2 } else { 0 }),
        Constraint::Length(2),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(area);

    frame.render_widget(
        Paragraph::new(now_playing(&view, area.width as usize, rows)),
        top,
    );
    if let Some(message) = view.message {
        // Rule 4 reaching the terminal: a request the daemon refused says so
        // rather than looking like a key that did not register.
        frame.render_widget(
            Paragraph::new(spaced(Line::from(Span::styled(
                fit(message, area.width as usize),
                Style::from(ACCENT()),
            )))),
            note,
        );
    }

    let queued = view.queue.items.len();
    frame.render_widget(
        Paragraph::new(spaced(browser::header(
            view.browser,
            (view.pane == Pane::Queue).then_some(queued),
        ))),
        tabs,
    );
    match view.pane {
        Pane::Browser => browser::render(frame, list, view.browser),
        Pane::Queue => queue::render(frame, list, view.queue),
    }

    frame.render_widget(
        Paragraph::new(key_hints(
            area.width as usize,
            view.pane,
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

/// How tall the spectrum is, for a given window.
///
/// A share of the height rather than a constant, for the same reason the bars
/// take the width: a tall terminal has room to show the gradient, and a short
/// one has to spend its rows on the lists.
fn spectrum_rows(height: u16) -> usize {
    (height / SPECTRUM_SHARE).clamp(SPECTRUM_MIN as u16, SPECTRUM_MAX as u16) as usize
}

fn now_playing(view: &View, width: usize, rows: usize) -> Vec<Line<'static>> {
    // The program's own name, in the accent, above everything. It is the first
    // thing on screen in every state — including the ones where there is no
    // track to describe, which is exactly when it is useful to be told what
    // you are looking at.
    let name = Line::from(Span::styled(
        "climat",
        Style::from(ACCENT()).add_modifier(Modifier::BOLD),
    ));

    // Anything but Ready has nothing to say about a track, so it says what it
    // is doing instead — a blank player looks broken, a waiting one does not.
    if !matches!(view.stage, Stage::Ready) {
        return vec![
            name,
            Line::from(Span::styled(stage_text(view.stage), Style::from(MUTED()))),
        ];
    }
    if view.snap.title.is_empty() {
        return vec![
            name,
            Line::from(Span::styled("Nothing playing", Style::from(MUTED()))),
        ];
    }

    // **Under the title, where Winamp put it**, and two rows tall — one row of
    // block characters is only eight heights, and a bar has to travel an eighth
    // of its range before anything changes on screen. That reads as a slow
    // visualiser however fast the data behind it arrives.
    let spectrum = if view.bars.iter().any(|v| *v > 0.0) {
        bars(view.bars, width, rows)
    } else {
        vec![Vec::new(); rows]
    };

    let title = Line::from(vec![
        Span::styled(
            view.snap.title.clone(),
            Style::from(BRIGHT()).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  —  ", Style::from(DIM())),
        Span::styled(view.snap.artist.clone(), Style::from(MUTED())),
    ]);
    // Its own line rather than a third clause on the title: an album is the
    // one field long enough to push the artist off a narrow window.
    let album = Line::from(Span::styled(view.snap.album.clone(), Style::from(DIM())));

    // **The full width, exactly like the bars above it.** Nothing shares this
    // line, so the two meters line up and a seek is aimed at the same scale the
    // spectrum is drawn on.
    let seek = Line::from(vec![
        Span::styled(
            meter(progress(view.snap), width, '━'),
            Style::from(ACCENT()),
        ),
        Span::styled(rest(progress(view.snap), width, '─'), Style::from(DIM())),
    ]);

    // What moved off the seek line: the state at one end, the clock at the
    // other. There is room to say the state in a word now rather than trusting
    // a glyph alone to carry it.
    let state = Line::from(ends(
        vec![
            Span::styled(view.snap.glyph(), Style::from(ACCENT())),
            Span::raw("  "),
            Span::styled(view.snap.state_word(), Style::from(MUTED())),
        ],
        vec![Span::styled(
            format!(
                "{} / {}",
                clock(view.snap.position_ms),
                clock(view.snap.duration_ms)
            ),
            Style::from(DIM()),
        )],
        width,
    ));

    let modes = Line::from(vec![
        Span::styled("shuffle ", Style::from(DIM())),
        mode(
            view.snap.shuffle,
            if view.snap.shuffle { "on" } else { "off" },
        ),
        Span::styled("   repeat ", Style::from(DIM())),
        mode(
            !matches!(
                view.snap.repeat,
                slipmat_core::player::protocol::RepeatMode::None
            ),
            repeat_text(view.snap),
        ),
        Span::styled("   vol ", Style::from(DIM())),
        Span::styled(meter(view.snap.volume, VOL, '█'), Style::from(ACCENT())),
        Span::styled(rest(view.snap.volume, VOL, '░'), Style::from(DIM())),
    ]);

    let mut band = vec![name, title, album, Line::default()];
    band.extend(spectrum.into_iter().map(Line::from));
    // **A blank between the bars and the seek line.** Without it the rule sits
    // flush against the spectrum and reads as part of it.
    band.push(Line::default());
    band.push(seek);
    band.push(state);
    band.push(modes);
    band
}

/// One line with `left` at one end and `right` at the other.
///
/// Falls back to a single space between them when the window cannot hold both,
/// because a negative pad is a panic and a truncated clock is worse than a
/// crowded one.
fn ends(left: Vec<Span<'static>>, right: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let used: usize = left
        .iter()
        .chain(&right)
        .map(|s| s.content.chars().count())
        .sum();
    let mut out = left;
    out.push(Span::raw(" ".repeat(width.saturating_sub(used).max(1))));
    out.extend(right);
    out
}

/// The always-there row. **Nothing has to be memorised**, which is the whole
/// reason it is always there rather than behind a key.
///
/// It changes with focus, because the keys do: only the queue reorders, only
/// the browser filters. And it is built by priority and stops when the window
/// runs out, so a narrow terminal loses the least useful hint rather than
/// losing the row. Leaving and quitting are reserved from the start — a player
/// you cannot see how to leave is worse than one with no hints at all.
fn key_hints(width: usize, pane: Pane, typing: bool, catalog: bool) -> Line<'static> {
    // **What the terminal already means.** Ctrl+C is how everybody leaves a
    // program in a terminal, and it is the right key for the one that leaves
    // the music playing. `q` is the one that takes the player with it.
    const LEAVING: [(&str, &str); 2] = [("^C", "hide"), ("q", "quit")];

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
    } else if pane == Pane::Browser {
        vec![
            ("space", "play/pause"),
            ("↑↓", "move"),
            ("↵", "play/open"),
            ("/", "filter"),
            ("1-6", "tab"),
            ("esc", "back"),
        ]
    } else {
        vec![
            ("space", "play/pause"),
            ("↑↓", "move"),
            ("↵", "play"),
            ("d", "remove"),
            ("KJ", "reorder"),
            ("1-6", "tab"),
            ("-=", "volume"),
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
            Style::from(MUTED()).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!(" {what}   "), Style::from(DIM())));
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

/// One cell per column, `rows` tall, coloured from the accent at the bottom to
/// white at the top. Returned top row first, the order they are drawn in.
///
/// Height is where the gradient lives: over two rows it is two shades, over
/// five it is a gradient somebody can actually see.
fn bars(levels: &[f32], width: usize, rows: usize) -> Vec<Vec<Span<'static>>> {
    const STEPS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let total = rows * STEPS.len();
    let columns = columns(levels, width);
    // Bottom row first while filling, reversed at the end — a bar fills upward
    // and it is simpler to say so than to invert the arithmetic.
    let mut out: Vec<Vec<Span<'static>>> = Vec::with_capacity(rows);
    for row in 0..rows {
        let floor = row * STEPS.len();
        out.push(
            columns
                .iter()
                .map(|&v| {
                    let height = (v.clamp(0.0, 1.0) * total as f32).round() as usize;
                    let eighths = height.saturating_sub(floor).min(STEPS.len());
                    // Where this cell sits in the whole column decides its
                    // colour, so the gradient runs over the bars rather than
                    // over each one separately.
                    let foot = floor as f32 / total as f32;
                    let head = (floor + STEPS.len()) as f32 / total as f32;
                    cell(
                        eighths,
                        mix(ACCENT(), PEAK(), foot),
                        mix(ACCENT(), PEAK(), head),
                        &STEPS,
                    )
                })
                .collect(),
        );
    }
    out.reverse();
    out
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
        Style::from(mix(foot, head, t)),
    )
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

/// What the transport is doing, in a glyph and in a word.
///
/// `busy` is its own answer rather than a kind of paused: a track that is
/// loading has not failed to start, and saying "Paused" while it works is the
/// small lie that makes people press the key again.
trait Transport {
    fn glyph(&self) -> &'static str;
    fn state_word(&self) -> &'static str;
}

impl Transport for Snapshot {
    fn glyph(&self) -> &'static str {
        if self.busy {
            "◌"
        } else if self.playing {
            "▶"
        } else {
            "❚❚"
        }
    }

    fn state_word(&self) -> &'static str {
        if self.busy {
            "Loading"
        } else if self.playing {
            "Playing"
        } else {
            "Paused"
        }
    }
}

fn mode(on: bool, text: &str) -> Span<'static> {
    Span::styled(
        text.to_owned(),
        if on {
            Style::from(ACCENT())
        } else {
            Style::from(DIM())
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

fn meter(fraction: f64, width: usize, glyph: char) -> String {
    glyph.to_string().repeat(filled(fraction, width))
}

fn rest(fraction: f64, width: usize, glyph: char) -> String {
    glyph.to_string().repeat(width - filled(fraction, width))
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
    fn every_volume_step_moves_the_meter_by_exactly_one_cell() {
        // The keys move volume by 5%. At ten cells that was half a cell, so
        // `filled` rounded and every *other* press changed nothing on screen —
        // which reads as the app missing keystrokes, not as a rounding choice.
        const STEP: f64 = 0.05;
        for i in 0..20 {
            let from = i as f64 * STEP;
            assert_eq!(
                filled(from + STEP, VOL) - filled(from, VOL),
                1,
                "a step from {from:.2} did not move the meter"
            );
        }
    }

    #[test]
    fn a_silent_bar_is_a_space_rather_than_a_stub() {
        // A row of ▁ across the whole width reads as a broken meter; silence
        // should read as nothing at all.
        let glyphs = |rows: Vec<Vec<Span>>| {
            rows.iter()
                .map(|r| r.iter().map(|s| s.content.to_string()).collect::<String>())
                .collect::<Vec<_>>()
        };
        assert_eq!(glyphs(bars(&[0.0, 0.0], 2, 2)), vec!["  ", "  "]);
        // Full height fills every row; half fills the lower half only.
        assert_eq!(glyphs(bars(&[1.0], 1, 2)), vec!["█", "█"]);
        assert_eq!(glyphs(bars(&[0.5], 1, 2)), vec![" ", "█"]);
        assert_eq!(glyphs(bars(&[0.5], 1, 4)), vec![" ", " ", "█", "█"]);
    }

    #[test]
    fn the_bars_take_the_width_they_are_given() {
        // Both directions: more columns than bands, and fewer.
        let bands = [0.1, 0.9, 0.2, 0.8];
        for width in [1usize, 3, 4, 12, 80] {
            for rows in [2usize, 4, 6] {
                let drawn = bars(&bands, width, rows);
                assert_eq!(drawn.len(), rows, "row count at {width}x{rows}");
                for row in &drawn {
                    assert_eq!(row.len(), width, "row width at {width}x{rows}");
                }
            }
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
                    meter(f, width, '━').chars().count() + rest(f, width, '─').chars().count(),
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
