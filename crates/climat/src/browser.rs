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
use slipmat_core::ipc::{CatalogFilter, View};
use slipmat_core::sort::SortBy;

use crate::theme::{accent as ACCENT, bright as BRIGHT, dim as DIM, muted as MUTED};
use crate::ui::fit;

/// The sections, in the order `1`–`4` select them.
pub const SECTIONS: [(View, &str); 4] = [
    (View::Songs, "SONGS"),
    (View::Albums, "ALBUMS"),
    (View::Artists, "ARTISTS"),
    (View::Playlists, "PLAYLISTS"),
];

/// What the pane is showing: a section of the library, all of Apple Music, or a
/// page opened from either.
pub enum Showing {
    Library,
    /// Catalog results. **Nothing is fetched until it is asked for** — every
    /// query here is a request to Apple, where a library filter is a read of
    /// the daemon's own cache.
    Catalog {
        searching: bool,
    },
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
    /// The text narrowing each list, **kept per tab**.
    ///
    /// One shared string meant a filter typed on Songs followed you to Albums,
    /// to Playlists, and to the queue — which has no filtering at all and sat
    /// there showing a box full of somebody else's search. A filter belongs to
    /// the list it was typed for, the same way a sort does.
    ///
    /// Five slots: the four library sections and the catalog, whose text is not
    /// a filter but a question already asked of Apple — losing it on the way to
    /// the queue and back would mean asking again.
    filters: [String; 5],
    /// Whether the field has the keyboard. `/` opens it; Esc closes it and
    /// clears it, because a filter you cannot see is a list that is lying about
    /// what the library holds.
    pub typing: bool,
    /// Which kinds of thing a catalog search asks Apple for.
    ///
    /// Everything is the useful default — a few artists and albums, then songs
    /// — but looking for a record among the songs that share its name is what
    /// narrowing is for.
    pub kinds: CatalogFilter,
    /// Whether Apple has more results behind the ones on screen, and whether a
    /// page is already on its way. **Both, not one**: without the second, every
    /// keypress near the end asks again for a page already in flight.
    pub more: bool,
    pub paging: bool,
    /// What each section is ordered by, and which way round.
    ///
    /// **Per section, not one shared setting.** The keys differ because the
    /// data does — an album has a year, a playlist has neither an artist nor a
    /// year — so choosing "Recently Added" for albums must not leave songs
    /// claiming to be sorted by a date they do not carry.
    pub sorts: [(SortBy, bool); 4],
    /// Whether the page on screen was opened from a catalog search, so `esc`
    /// knows which list to go back to.
    pub from_catalog: bool,
    /// How many matched before the page limit, so a truncated list says so.
    pub total: usize,
    offset: usize,
}

impl Showing {
    pub fn is_catalog(&self) -> bool {
        matches!(self, Showing::Catalog { .. })
    }
}

impl Default for Browser {
    fn default() -> Self {
        Self {
            view: View::Songs,
            rows: Vec::new(),
            cursor: 0,
            showing: Showing::Library,
            filters: Default::default(),
            typing: false,
            kinds: CatalogFilter::default(),
            more: false,
            paging: false,
            sorts: [(SortBy::Title, false); 4],
            from_catalog: false,
            total: 0,
            offset: 0,
        }
    }
}

/// Where a section's sort lives in [`Browser::sorts`].
fn slot(view: View) -> usize {
    SECTIONS.iter().position(|(v, _)| *v == view).unwrap_or(0)
}

impl Browser {
    /// Which slot the text on screen belongs to.
    fn filter_slot(&self) -> usize {
        if self.showing.is_catalog() {
            SECTIONS.len()
        } else {
            slot(self.view)
        }
    }

    pub fn filter(&self) -> &str {
        &self.filters[self.filter_slot()]
    }

    pub fn filter_mut(&mut self) -> &mut String {
        let at = self.filter_slot();
        &mut self.filters[at]
    }

    pub fn sort(&self) -> (SortBy, bool) {
        self.sorts[slot(self.view)]
    }

    /// Step to the next key this section can honestly be ordered by.
    ///
    /// Returns false when there is only one — a library artist carries a name
    /// and nothing else, so there is nothing to step through and saying so
    /// beats a key that appears to do nothing.
    /// Step to the next kind of thing a catalog search asks for.
    pub fn cycle_kinds(&mut self) {
        let at = CatalogFilter::ALL
            .iter()
            .position(|k| *k == self.kinds)
            .unwrap_or(0);
        self.kinds = CatalogFilter::ALL[(at + 1) % CatalogFilter::ALL.len()];
    }

    /// Is the cursor close enough to the end to be worth asking for more?
    ///
    /// **Before it arrives, not when it lands.** A page takes a round trip to
    /// Apple, so asking at the last row means stopping at the last row.
    pub fn wants_more(&self) -> bool {
        const LOOKAHEAD: usize = 8;
        self.more
            && !self.paging
            && self.showing.is_catalog()
            && self.cursor + LOOKAHEAD >= self.rows.len()
    }

    /// Add a page to what is already showing, leaving the cursor where it is.
    pub fn extend(&mut self, rows: Vec<Entry>) {
        self.rows.extend(rows);
        self.total = self.rows.len();
    }

    pub fn cycle_sort(&mut self) -> bool {
        let keys = SortBy::for_view(self.view);
        if keys.len() < 2 {
            return false;
        }
        let (by, _) = self.sort();
        let next = keys[(keys.iter().position(|k| *k == by).unwrap_or(0) + 1) % keys.len()];
        self.sorts[slot(self.view)].0 = next;
        true
    }

    pub fn flip_sort(&mut self) {
        let at = slot(self.view);
        self.sorts[at].1 = !self.sorts[at].1;
    }
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
/// The tab strip, or the opened page's name.
///
/// `queue` is the queue's own tab: it is not a section of the library and the
/// browser knows nothing about it, but it belongs in the same row because it is
/// one of the places the one pane can be showing.
pub fn header(browser: &Browser, queue: Option<usize>, width: usize) -> Line<'static> {
    if let Showing::Page {
        title,
        subtitle,
        loading,
    } = &browser.showing
    {
        let mut spans = vec![Span::styled(
            title.to_uppercase(),
            Style::from(MUTED()).add_modifier(Modifier::BOLD),
        )];
        if !subtitle.is_empty() {
            spans.push(Span::styled(format!("   {subtitle}"), Style::from(DIM())));
        }
        if *loading {
            spans.push(Span::styled("   opening…", Style::from(DIM())));
        }
        return Line::from(spans);
    }

    let showing_queue = queue.is_some();
    let catalog = browser.showing.is_catalog();
    let mut spans = Vec::new();
    for (view, name) in SECTIONS {
        let on = !showing_queue && !catalog && view == browser.view;
        spans.push(tab(name, on));
        spans.push(Span::raw("   "));
    }
    // The fifth is not a section of anything — it is the rest of Apple Music,
    // and it is empty until asked.
    spans.push(tab("APPLE MUSIC", !showing_queue && catalog));
    spans.push(Span::raw("   "));
    spans.push(tab("QUEUE", showing_queue));

    // **The rule runs to the far end of every tab, and carries whatever that
    // tab has to say about itself.** At the end of the tabs these read as one
    // more tab; pushed right and ruled off, they read as what the list *is*
    // rather than somewhere to go.
    let right = if showing_queue {
        queue.map(|n| format!("[{n}]"))
    } else if catalog {
        // What Apple was asked for. A catalog list has no order of ours — it
        // comes back in Apple's — but which *kinds* it holds is a choice, and
        // the same place is where a choice belongs.
        Some(format!("[{}]", browser.kinds.label()))
    } else {
        let (by, reversed) = browser.sort();
        let arrow = if reversed != by.descends_by_default() {
            "↑"
        } else {
            "↓"
        };
        Some(format!("[{arrow} {}]", by.label().to_lowercase()))
    };

    let tail = right.unwrap_or_default();
    let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    // A space either side of the rule, and never less than one dash — on a
    // window too narrow for both, the value still wins its place.
    let rule = width.saturating_sub(used + tail.chars().count() + 2).max(1);
    spans.push(Span::styled(
        format!(" {} ", "─".repeat(rule)),
        Style::from(DIM()),
    ));
    if !tail.is_empty() {
        spans.push(Span::styled(tail, Style::from(MUTED())));
    }

    Line::from(spans)
}

fn tab(name: &str, on: bool) -> Span<'static> {
    Span::styled(
        if on {
            name.to_string()
        } else {
            name.to_lowercase()
        },
        if on {
            Style::from(ACCENT()).add_modifier(Modifier::BOLD)
        } else {
            Style::from(DIM())
        },
    )
}

/// How many rows the search box takes, or none when it is not showing.
pub fn search_height(browser: &Browser) -> u16 {
    if browser.typing || !browser.filter().is_empty() {
        3
    } else {
        0
    }
}

/// The search box: a field the width of the window, under the tabs.
///
/// **A box rather than a word on the strip.** `/odesza` tucked among the tabs
/// read as one more label; a bordered field reads as somewhere text goes, and
/// its own border can say what typing there will do — the two are different
/// questions and the answer changes with the tab.
pub fn search_box(browser: &Browser, width: usize) -> Vec<Line<'static>> {
    let label = if browser.showing.is_catalog() {
        " search Apple Music "
    } else {
        " filter the library "
    };
    let inner = width.saturating_sub(2);
    let lead = 2usize;
    let rule = inner.saturating_sub(lead + label.chars().count());

    let top = Line::from(vec![
        Span::styled(
            format!("┌{}", "─".repeat(lead.min(inner))),
            Style::from(DIM()),
        ),
        Span::styled(label, Style::from(MUTED())),
        Span::styled(format!("{}┐", "─".repeat(rule)), Style::from(DIM())),
    ]);

    // **The cursor follows the text, so the text cannot be padded first.**
    // `fit` pads to a width, which put the block at the far right of the field
    // as though the caret were parked at the end of an empty line.
    //
    // The row is `│ ` + text + caret + padding + `│`, so text and padding
    // together are `width - 4`.
    let room = width.saturating_sub(4);
    let typed: String = browser.filter().chars().take(room).collect();
    let pad = room - typed.chars().count();
    let middle = vec![
        Span::styled("│ ", Style::from(DIM())),
        Span::styled(typed, Style::from(BRIGHT())),
        Span::styled(
            if browser.typing { "▌" } else { " " },
            Style::from(ACCENT()),
        ),
        Span::raw(" ".repeat(pad)),
        Span::styled("│", Style::from(DIM())),
    ];

    vec![
        top,
        Line::from(middle),
        Line::from(Span::styled(
            format!("└{}┘", "─".repeat(inner)),
            Style::from(DIM()),
        )),
    ]
}

pub fn render(frame: &mut Frame, area: Rect, browser: &mut Browser) {
    if browser.rows.is_empty() {
        let what = match &browser.showing {
            Showing::Page { loading: true, .. } => "Opening…",
            Showing::Catalog { searching: true } => "Searching Apple Music…",
            // The catalog is empty until asked, so an empty pane means "type
            // something", not "there is nothing" — two very different things to
            // read on a screen that looks identical.
            Showing::Catalog { .. } if browser.filter().is_empty() => {
                "Press / to search Apple Music"
            }
            Showing::Catalog { .. } => "Nothing on Apple Music matches",
            _ if !browser.filter().is_empty() => "Nothing matches",
            _ => "Nothing here",
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(what, Style::from(DIM())))),
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
        .map(|(i, entry)| row(entry, i == browser.cursor, width))
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
            Style::from(BRIGHT()).add_modifier(Modifier::REVERSED),
            Style::from(MUTED()).add_modifier(Modifier::REVERSED),
        )
    } else {
        (Style::from(BRIGHT()), Style::from(MUTED()))
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
    fn an_empty_catalog_invites_a_search_rather_than_reporting_none() {
        // The two states look identical — an empty pane — and mean opposite
        // things. "Nothing matches" over a catalog nobody has queried yet is a
        // lie about Apple's library.
        let mut b = Browser {
            showing: Showing::Catalog { searching: false },
            ..Default::default()
        };
        assert!(b.filter().is_empty());
        assert!(b.showing.is_catalog());

        *b.filter_mut() = "odesza".into();
        assert!(b.showing.is_catalog(), "still the catalog once asked");
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
