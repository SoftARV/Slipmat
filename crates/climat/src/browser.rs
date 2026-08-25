// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The library pane: four sections, a filter, and what a row leads to.
//!
//! Rows come from the daemon's cache, which is why a section switch is instant
//! and a filter can run on every keystroke — nothing here is a round trip to
//! Apple. Opening an album *is* one, so a page shows what it has and says when
//! it is still coming.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use slipmat_core::entry::Entry;
use slipmat_core::ipc::View;

use crate::ui::{ACCENT, BRIGHT, DIM, MUTED, fit};

/// The sections, in the order `1`–`4` select them.
pub const SECTIONS: [(View, &str); 4] = [
    (View::Songs, "SONGS"),
    (View::Albums, "ALBUMS"),
    (View::Artists, "ARTISTS"),
    (View::Playlists, "PLAYLISTS"),
];

/// What the pane is showing: a section of the library, or a page opened from it.
pub enum Showing {
    Library,
    /// An album, artist or playlist. `header` is `None` until the daemon
    /// answers, so an opening page says whose it is rather than going blank.
    Page {
        title: String,
        subtitle: String,
        loading: bool,
    },
}

pub struct Browser {
    pub view: View,
    pub rows: Vec<Entry>,
    pub cursor: usize,
    pub showing: Showing,
    /// The filter, and whether it is being typed into. `/` opens it; Esc closes
    /// it and clears it, because a filter you cannot see is a list that is
    /// lying about what the library holds.
    pub filter: String,
    pub typing: bool,
    /// How many matched before the page limit, so a truncated list says so.
    pub total: usize,
    offset: usize,
}

impl Default for Browser {
    fn default() -> Self {
        Self {
            view: View::Songs,
            rows: Vec::new(),
            cursor: 0,
            showing: Showing::Library,
            filter: String::new(),
            typing: false,
            total: 0,
            offset: 0,
        }
    }
}

impl Browser {
    pub fn replace(&mut self, rows: Vec<Entry>, total: usize) {
        self.rows = rows;
        self.total = total;
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
    }

    /// A new list to look at from the top — a section switch, a filter, a page.
    pub fn reset(&mut self) {
        self.cursor = 0;
        self.offset = 0;
    }

    pub fn move_cursor(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let last = self.rows.len() as isize - 1;
        self.cursor = (self.cursor as isize + delta).clamp(0, last) as usize;
    }

    pub fn selected(&self) -> Option<&Entry> {
        self.rows.get(self.cursor)
    }

    /// The ids to send as a queue, and where the selected row lands in them.
    ///
    /// **Rows with no playable id drop out of both**, or the index would count
    /// rows the daemon is about to discard and open on the wrong track.
    pub fn queue_from_here(&self) -> (Vec<String>, usize) {
        let ids: Vec<String> = self
            .rows
            .iter()
            .filter_map(|e| e.catalog_id().map(str::to_owned))
            .collect();
        let index = self
            .rows
            .iter()
            .take(self.cursor)
            .filter(|e| e.catalog_id().is_some())
            .count();
        (ids, index)
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
        self.offset = self.offset.min(self.rows.len().saturating_sub(height));
    }
}

/// The section tabs, or the opened page's name — one line, either way.
pub fn header(browser: &Browser) -> Line<'static> {
    if let Showing::Page {
        title,
        subtitle,
        loading,
    } = &browser.showing
    {
        let mut spans = vec![Span::styled(
            title.to_uppercase(),
            Style::from(MUTED).add_modifier(Modifier::BOLD),
        )];
        if !subtitle.is_empty() {
            spans.push(Span::styled(format!("   {subtitle}"), Style::from(DIM)));
        }
        if *loading {
            spans.push(Span::styled("   opening…", Style::from(DIM)));
        }
        return Line::from(spans);
    }

    let mut spans = Vec::new();
    for (view, name) in SECTIONS {
        let on = view == browser.view;
        spans.push(Span::styled(
            if on {
                name.to_string()
            } else {
                name.to_lowercase()
            },
            if on {
                Style::from(ACCENT).add_modifier(Modifier::BOLD)
            } else {
                Style::from(DIM)
            },
        ));
        spans.push(Span::raw("   "));
    }
    if browser.typing || !browser.filter.is_empty() {
        spans.push(Span::styled(
            format!("/{}", browser.filter),
            Style::from(if browser.typing { BRIGHT } else { MUTED }),
        ));
        // A block, so it is obvious the next keystroke goes here and not to the
        // transport — `p` in a filter must never mean pause.
        if browser.typing {
            spans.push(Span::styled("▌", Style::from(ACCENT)));
        }
    }
    Line::from(spans)
}

pub fn render(frame: &mut Frame, area: Rect, browser: &mut Browser, focused: bool) {
    if browser.rows.is_empty() {
        let what = match &browser.showing {
            Showing::Page { loading: true, .. } => "Opening…",
            _ if !browser.filter.is_empty() => "Nothing matches",
            _ => "Nothing here",
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(what, Style::from(DIM)))),
            area,
        );
        return;
    }

    let height = area.height as usize;
    browser.scroll(height);
    let width = area.width as usize;
    let rows: Vec<Line> = browser
        .rows
        .iter()
        .enumerate()
        .skip(browser.offset)
        .take(height)
        .map(|(i, entry)| row(entry, i == browser.cursor && focused, width))
        .collect();
    frame.render_widget(Paragraph::new(rows), area);
}

fn row(entry: &Entry, selected: bool, width: usize) -> Line<'static> {
    // A marker for the rows that lead somewhere, so "press Enter" means two
    // different things visibly rather than by memory.
    let lead = if entry.opens_a_page() { "▸ " } else { "  " };

    let subtitle = entry.subtitle();
    let sub_room = if width > 40 { width / 3 } else { 0 };
    let title_room = width.saturating_sub(lead.len() + sub_room + 1);

    let (title, quiet) = if selected {
        (
            Style::from(BRIGHT).add_modifier(Modifier::REVERSED),
            Style::from(MUTED).add_modifier(Modifier::REVERSED),
        )
    } else {
        (Style::from(BRIGHT), Style::from(MUTED))
    };

    let mut spans = vec![
        Span::styled(lead, quiet),
        Span::styled(fit(entry.title(), title_room), title),
    ];
    if sub_room > 0 {
        spans.push(Span::styled(fit(&subtitle, sub_room), quiet));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use slipmat_core::music::types::{Track, TrackId};

    fn song(id: Option<&str>) -> Entry {
        Entry::Song(Track {
            date_added: String::new(),
            year: String::new(),
            favorite: false,
            in_library: true,
            library_id: None,
            id: TrackId(String::from("l.1")),
            catalog_id: id.map(String::from),
            title: "A Song".into(),
            artist: "Someone".into(),
            album: "An Album".into(),
            duration_ms: 180_000,
            track_number: 1,
            artwork: None,
        })
    }

    #[test]
    fn unplayable_rows_do_not_shift_the_starting_track() {
        // The trap: Apple returns rows it will not stream, the daemon drops
        // them, and an index counted over the *drawn* list then opens on the
        // wrong song — further wrong the more of them there are.
        let mut b = Browser::default();
        b.replace(
            vec![song(None), song(Some("c1")), song(None), song(Some("c2"))],
            4,
        );
        b.cursor = 3;
        let (ids, index) = b.queue_from_here();
        assert_eq!(ids, vec!["c1", "c2"]);
        assert_eq!(index, 1, "counted rows the daemon is about to discard");
    }

    #[test]
    fn a_shorter_list_cannot_strand_the_cursor() {
        let mut b = Browser::default();
        b.replace((0..10).map(|_| song(Some("c"))).collect(), 10);
        b.cursor = 9;
        b.replace(vec![song(Some("c"))], 1);
        assert_eq!(b.cursor, 0);
    }

    #[test]
    fn the_cursor_stays_on_screen() {
        let mut b = Browser::default();
        b.replace((0..200).map(|_| song(Some("c"))).collect(), 200);
        for target in [0usize, 120, 199, 7] {
            b.cursor = target;
            b.scroll(12);
            assert!((b.offset..b.offset + 12).contains(&b.cursor), "{target}");
        }
    }
}
