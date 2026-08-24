// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The queue pane: what is in it, where the cursor is, and how it scrolls.
//!
//! **The list is the daemon's; the cursor is ours.** Rule 3 says a client never
//! edits the queue it draws — `d` and `K`/`J` send a request and the rows move
//! when the echo arrives. The cursor is not queue state, though: it is where
//! this terminal is looking, so it moves the instant a key is pressed.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use slipmat_core::ipc::QueueItem;

use crate::ui::{ACCENT, BRIGHT, DIM, MUTED, clock, fit};

/// How wide the artist column is, when there is room for one at all.
const ARTIST: usize = 22;

pub struct Queue {
    pub items: Vec<QueueItem>,
    /// What is playing.
    pub position: usize,
    /// What is selected.
    pub cursor: usize,
    /// Until the cursor is moved by hand it follows the music, so a queue left
    /// alone always shows the track that is playing rather than drifting off
    /// the top as the album advances.
    following: bool,
    /// First visible row, kept between frames so the list scrolls instead of
    /// jumping the selection to the middle every time.
    offset: usize,
}

impl Default for Queue {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            position: 0,
            cursor: 0,
            // **Following from the start**, or a client that has just attached
            // opens with its cursor on the first track of a queue that is
            // playing its fortieth. `Default` derives this as `false`, which is
            // the wrong answer and a silent one.
            following: true,
            offset: 0,
        }
    }
}

impl Queue {
    /// Adopt a queue from the daemon, keeping the cursor somewhere sensible.
    pub fn replace(&mut self, items: Vec<QueueItem>, position: usize) {
        self.items = items;
        self.position = position;
        if self.following {
            self.cursor = position;
        }
        self.cursor = self.cursor.min(self.items.len().saturating_sub(1));
    }

    pub fn move_cursor(&mut self, delta: isize) {
        if self.items.is_empty() {
            return;
        }
        let last = self.items.len() - 1;
        self.cursor = (self.cursor as isize + delta).clamp(0, last as isize) as usize;
        // Moved by hand, so stop chasing the music — until `follow` is pressed.
        self.following = false;
    }

    /// Put the cursor back on the playing track and leave it there.
    pub fn follow(&mut self) {
        self.following = true;
        self.cursor = self.position.min(self.items.len().saturating_sub(1));
    }

    /// Where a `K`/`J` reorder would land, if it is legal.
    pub fn swap_target(&self, delta: isize) -> Option<usize> {
        let to = self.cursor as isize + delta;
        (to >= 0 && (to as usize) < self.items.len()).then_some(to as usize)
    }

    /// Take the cursor with a row that is about to move, so the selection stays
    /// on the track rather than on the slot.
    pub fn cursor_to(&mut self, index: usize) {
        self.cursor = index;
        self.following = false;
    }

    fn scroll(&mut self, height: usize) {
        if height == 0 {
            return;
        }
        if self.cursor < self.offset {
            self.offset = self.cursor;
        } else if self.cursor >= self.offset + height {
            self.offset = self.cursor + 1 - height;
        }
        self.offset = self.offset.min(self.items.len().saturating_sub(height));
    }
}

pub fn header(queue: &Queue) -> Line<'static> {
    let count = match queue.items.len() {
        1 => "1 track".to_string(),
        n => format!("{n} tracks"),
    };
    Line::from(vec![
        Span::styled("QUEUE", Style::from(MUTED).add_modifier(Modifier::BOLD)),
        Span::styled(format!("   {count}"), Style::from(DIM)),
    ])
}

pub fn render(frame: &mut Frame, area: Rect, queue: &mut Queue) {
    if queue.items.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled("Nothing queued", Style::from(DIM)))),
            area,
        );
        return;
    }

    let height = area.height as usize;
    queue.scroll(height);

    let width = area.width as usize;
    let rows: Vec<Line> = queue
        .items
        .iter()
        .enumerate()
        .skip(queue.offset)
        .take(height)
        .map(|(i, item)| row(i, item, queue, width))
        .collect();
    frame.render_widget(Paragraph::new(rows), area);
}

fn row(index: usize, item: &QueueItem, queue: &Queue, width: usize) -> Line<'static> {
    let playing = index == queue.position;
    let selected = index == queue.cursor;

    // The marker column carries "this is playing" so the row still says so in a
    // terminal with no colour, and so it survives the cursor's own highlight.
    let marker = if playing { "▸" } else { " " };
    let number = format!("{marker} {:>3}  ", index + 1);

    let time = clock(item.duration_ms);
    // Everything that is not the title has a fixed width, so the title is what
    // gives way when the window is narrow.
    let fixed = number.chars().count() + time.chars().count() + 2;
    let artist_room = if width > fixed + 24 { ARTIST } else { 0 };
    let title_room = width.saturating_sub(fixed + artist_room);

    let base = if playing {
        Style::from(ACCENT)
    } else {
        Style::from(BRIGHT)
    };
    let quiet = if playing {
        Style::from(ACCENT)
    } else {
        Style::from(MUTED)
    };
    let (base, quiet) = if selected {
        (
            base.add_modifier(Modifier::REVERSED),
            quiet.add_modifier(Modifier::REVERSED),
        )
    } else {
        (base, quiet)
    };

    let mut spans = vec![
        Span::styled(number, quiet),
        Span::styled(fit(&item.title, title_room), base),
    ];
    if artist_room > 0 {
        spans.push(Span::styled(fit(&item.artist, artist_room), quiet));
    }
    spans.push(Span::styled(format!("{time:>5} "), quiet));
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue_of(n: usize) -> Queue {
        let mut q = Queue::default();
        q.replace(
            (0..n)
                .map(|i| QueueItem {
                    id: Some(format!("t{i}")),
                    title: format!("Track {i}"),
                    artist: "Someone".into(),
                    album: "An Album".into(),
                    duration_ms: 180_000,
                })
                .collect(),
            0,
        );
        q
    }

    #[test]
    fn an_untouched_cursor_follows_the_music() {
        // The common case: a queue playing an album, nobody scrolling. The
        // selection should stay on the track that is playing — including on the
        // very first queue a freshly attached client is handed, which is what
        // an earlier version of this test called `follow()` to fake.
        let mut q = queue_of(10);
        q.replace(q.items.clone(), 4);
        assert_eq!(q.cursor, 4);

        let mut fresh = Queue::default();
        fresh.replace(queue_of(10).items, 7);
        assert_eq!(fresh.cursor, 7, "a new client opened on the wrong track");
    }

    #[test]
    fn a_cursor_moved_by_hand_stops_following() {
        // Somebody is reading ahead in the queue. Advancing a track must not
        // yank their selection back to the top.
        let mut q = queue_of(10);
        q.move_cursor(7);
        q.replace(q.items.clone(), 4);
        assert_eq!(q.cursor, 7);
    }

    #[test]
    fn a_shorter_queue_cannot_strand_the_cursor() {
        // Removals are what shrink a queue, and a cursor past the end would
        // index out of the list on the very next draw.
        let mut q = queue_of(10);
        q.move_cursor(9);
        let two = q.items[..2].to_vec();
        q.replace(two, 0);
        assert_eq!(q.cursor, 1);
    }

    #[test]
    fn a_reorder_at_either_end_is_refused_rather_than_clamped() {
        // Clamping would turn "move the top track up" into a move to itself —
        // a request to the daemon that reorders nothing and reports nothing.
        let mut q = queue_of(3);
        assert_eq!(q.swap_target(-1), None);
        assert_eq!(q.swap_target(1), Some(1));
        q.move_cursor(2);
        assert_eq!(q.swap_target(1), None);
    }

    #[test]
    fn the_cursor_stays_on_screen_however_far_it_moves() {
        let mut q = queue_of(100);
        for target in [0usize, 50, 99, 3] {
            q.cursor = target;
            q.scroll(10);
            assert!(
                (q.offset..q.offset + 10).contains(&q.cursor),
                "cursor {target} off screen at offset {}",
                q.offset
            );
            assert!(q.offset + 10 <= 100, "scrolled past the end");
        }
    }
}
