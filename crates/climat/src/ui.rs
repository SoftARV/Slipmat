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

/// Apple Music's red, and the only colour that means anything here.
const ACCENT: Color = Color::Rgb(0xFF, 0x4A, 0x5E);
/// Track titles and anything the eye should land on.
const BRIGHT: Color = Color::Rgb(0xF2, 0xEC, 0xED);
/// Artists, and the second half of a line.
const MUTED: Color = Color::Rgb(0x9A, 0x8E, 0x90);
/// Times, labels, and everything that is only there when looked for.
const DIM: Color = Color::Rgb(0x65, 0x5B, 0x5D);

/// How wide the progress bar is drawn, in cells.
const BAR: usize = 25;
/// And the volume meter beside it.
const VOL: usize = 10;

pub struct View<'a> {
    pub snap: &'a Snapshot,
    pub stage: &'a Stage,
    pub queue_len: usize,
}

pub fn draw(frame: &mut Frame, view: View) {
    // Two rows of margin either side: a terminal player that runs to the edge
    // of the window reads as output rather than as an interface.
    let area = Rect {
        x: frame.area().x + 2,
        y: frame.area().y + 1,
        width: frame.area().width.saturating_sub(4),
        height: frame.area().height.saturating_sub(2),
    };
    let [top, _rest, hints] = Layout::vertical([
        Constraint::Length(4),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);

    frame.render_widget(Paragraph::new(now_playing(&view)), top);
    frame.render_widget(Paragraph::new(key_hints()), hints);
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
        return vec![
            Line::from(Span::styled("Nothing playing", Style::from(MUTED))),
            Line::default(),
            Line::from(Span::styled(
                format!("{} tracks in the queue", view.queue_len),
                Style::from(DIM),
            )),
        ];
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
fn key_hints() -> Line<'static> {
    let mut spans = Vec::new();
    for (key, what) in [
        ("z", "prev"),
        ("x", "play"),
        ("c", "pause"),
        ("b", "next"),
        ("_", "hide"),
        ("q", "quit"),
    ] {
        spans.push(Span::styled(
            key,
            Style::from(MUTED).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!(" {what}   "), Style::from(DIM)));
    }
    Line::from(spans)
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
fn clock(ms: u64) -> String {
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
