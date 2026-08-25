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

/// How wide the progress bar is drawn, in cells.
const BAR: usize = 25;
/// And the volume meter beside it.
const VOL: usize = 10;

pub struct View<'a> {
    pub snap: &'a Snapshot,
    pub stage: &'a Stage,
    pub browser: &'a mut Browser,
    pub queue: &'a mut Queue,
    pub focus: Focus,
    pub typing: bool,
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
        Constraint::Length(4),
        Constraint::Length(if view.message.is_some() { 2 } else { 0 }),
        Constraint::Length(2),
        Constraint::Fill(2),
        Constraint::Length(2),
        Constraint::Fill(3),
        Constraint::Length(1),
    ])
    .areas(area);

    frame.render_widget(Paragraph::new(now_playing(&view)), top);
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
        Paragraph::new(key_hints(area.width as usize, view.focus, view.typing)),
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

fn now_playing(view: &View) -> Vec<Line<'static>> {
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

    let title = Line::from(vec![
        Span::styled(
            view.snap.title.clone(),
            Style::from(BRIGHT).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  —  ", Style::from(DIM)),
        Span::styled(view.snap.artist.clone(), Style::from(MUTED)),
    ]);

    let transport = Line::from(vec![
        Span::styled(
            if view.snap.playing { "▶" } else { "❚❚" },
            Style::from(ACCENT),
        ),
        Span::raw("   "),
        Span::styled(meter(progress(view.snap), BAR), Style::from(ACCENT)),
        Span::styled(rest(progress(view.snap), BAR), Style::from(DIM)),
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

    vec![title, Line::default(), transport, modes]
}

/// The always-there row. **Nothing has to be memorised**, which is the whole
/// reason it is always there rather than behind a key.
///
/// It changes with focus, because the keys do: only the queue reorders, only
/// the browser filters. And it is built by priority and stops when the window
/// runs out, so a narrow terminal loses the least useful hint rather than
/// losing the row. Leaving and quitting are reserved from the start — a player
/// you cannot see how to leave is worse than one with no hints at all.
fn key_hints(width: usize, focus: Focus, typing: bool) -> Line<'static> {
    const LEAVING: [(&str, &str); 2] = [("_", "hide"), ("q", "quit")];

    // While typing, every letter goes into the filter, so advertising the
    // transport keys would be a lie about what the keyboard does.
    let keys: Vec<(&str, &str)> = if typing {
        vec![("↵", "done"), ("esc", "clear"), ("⌫", "back")]
    } else if focus == Focus::Browser {
        vec![
            ("space", "play/pause"),
            ("↑↓", "move"),
            ("↵", "play/open"),
            ("/", "filter"),
            ("1-4", "section"),
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
    fn a_clock_grows_a_field_only_when_it_needs_one() {
        assert_eq!(clock(0), "0:00");
        assert_eq!(clock(63_000), "1:03");
        assert_eq!(clock(3_723_000), "1:02:03");
    }

    #[test]
    fn a_meter_and_its_remainder_always_fill_the_width() {
        // Drawn as two spans in different colours, so a rounding disagreement
        // between them would make the bar change length as it played.
        for pct in 0..=100 {
            let f = pct as f64 / 100.0;
            assert_eq!(
                meter(f, BAR).chars().count() + rest(f, BAR).chars().count(),
                BAR,
                "at {pct}%"
            );
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
