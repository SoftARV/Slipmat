// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! One row in a results list — a song, an album or an artist.
//!
//! A `RelmListItem` over `gtk::ListView`, **not** a factory over
//! `gtk::ListBox`. A `ListBox` builds a real widget per row, so a 541-track
//! library meant 541 live rows. `ListView` recycles: it keeps about as many
//! widgets as fit on screen and rebinds them as you scroll.
//!
//! The consequence to keep in mind: widgets are **reused**. `bind` must set
//! every property it cares about, because the widget it is handed was showing
//! something else a moment ago, and anything left unset keeps the old value.

use relm4::adw::prelude::*;
use relm4::prelude::*;
use relm4::typed_view::list::RelmListItem;
use relm4::{gtk, view};

use crate::components::{CurrentTrack, DeadTracks, RowRegistry, TrackOverrides, overridden};
use slipmat_core::music::types::{Album, Artist, Playlist, Track};

/// What a row stands for. Songs play; everything else opens a page.
#[derive(Debug, Clone)]
pub enum Entry {
    Song(Track),
    Album(Album),
    Artist(Artist),
    Playlist(Playlist),
}

impl Entry {
    /// The id used to match a row against what is playing. Only songs have one.
    pub fn catalog_id(&self) -> Option<&str> {
        match self {
            Entry::Song(track) => track.catalog_id.as_deref(),
            _ => None,
        }
    }

    pub fn title(&self) -> &str {
        match self {
            Entry::Song(t) => &t.title,
            Entry::Album(a) => &a.name,
            Entry::Artist(a) => &a.name,
            Entry::Playlist(p) => &p.name,
        }
    }

    /// The second line. Collapses rather than rendering a dangling separator
    /// when a field is missing, which real catalogue entries often are.
    pub fn subtitle(&self) -> String {
        match self {
            Entry::Song(t) => match (t.artist.is_empty(), t.album.is_empty()) {
                (false, false) => format!("{} — {}", t.artist, t.album),
                (false, true) => t.artist.clone(),
                (true, false) => t.album.clone(),
                (true, true) => String::new(),
            },
            Entry::Album(a) => match (a.artist.is_empty(), a.year.is_empty()) {
                (false, false) => format!("{} · {}", a.artist, a.year),
                (false, true) => a.artist.clone(),
                (true, false) => a.year.clone(),
                (true, true) => String::new(),
            },
            Entry::Artist(a) => a.genres.clone(),
            // The curator is the useful line — Apple's editors made most of
            // what a catalogue search returns. The blurb is prose and belongs
            // on the page, not in a row.
            Entry::Playlist(p) => p.curator.clone(),
        }
    }

    fn icon(&self) -> &'static str {
        match self {
            Entry::Song(_) => "audio-x-generic-symbolic",
            Entry::Album(_) => "media-optical-symbolic",
            Entry::Artist(_) => "avatar-default-symbolic",
            Entry::Playlist(_) => "view-list-symbolic",
        }
    }

    /// Albums and artists lead somewhere; songs are the destination.
    fn opens_a_page(&self) -> bool {
        !matches!(self, Entry::Song(_))
    }
}

/// What the recycled widget is currently showing, for the context-menu gesture
/// — which is older than any particular track and must not capture one.
#[derive(Debug, Clone)]
pub struct RowFacts {
    pub catalog_id: String,
    /// The `i.…` id, present only for a track read out of the library. Removal
    /// needs it and the catalog id will not do — the two id spaces are not
    /// interchangeable.
    pub library_id: Option<String>,
    pub in_library: bool,
    pub favorite: bool,
}

/// What a right-click on a row is asking for: a menu, for this track, here.
#[derive(Debug)]
pub struct RowMenuRequest {
    pub catalog_id: String,
    /// The library id, when this track came from the library. `None` means
    /// removal cannot be offered even if `in_library` is true, because we do
    /// not know which row to delete.
    pub library_id: Option<String>,
    /// Already saved, so "Add to Library" is not offered.
    pub in_library: bool,
    /// Already starred, so "Favourite" is not offered.
    pub favorite: bool,
    /// Where in `over` the click landed, so the popover points at the pointer
    /// rather than at the middle of the row.
    pub at: (i32, i32),
    pub over: gtk::Box,
}

/// Who shows a row's context menu, once installed.
type RowMenuHandler = std::rc::Rc<dyn Fn(RowMenuRequest)>;

thread_local! {
    /// Who shows the row menu.
    ///
    /// A thread-local rather than a field on `LibraryItem`, because the gesture
    /// is created in `setup`, which is a *static* method with no access to any
    /// item — and threading a callback through every construction site of every
    /// list, for one menu, is a worse trade. Set once at startup; GTK is
    /// single-threaded, so there is exactly one.
    static ROW_MENU: std::cell::RefCell<Option<RowMenuHandler>> =
        const { std::cell::RefCell::new(None) };
}

/// Install the handler that shows a row's context menu. Called once, from the
/// root component's `init`.
pub fn set_row_menu(handler: impl Fn(RowMenuRequest) + 'static) {
    ROW_MENU.with(|m| *m.borrow_mut() = Some(std::rc::Rc::new(handler)));
}

pub struct LibraryItem {
    pub entry: Entry,
    /// Who is playing, shared with every other row. Read at `bind`, never
    /// stored per-item: a per-item flag has to be changed in the model, and any
    /// model edit costs the scroll position (see `CurrentTrack`).
    pub current: CurrentTrack,
    /// Ids MusicKit has refused. Consulted at bind, so a track discovered to be
    /// dead mid-session dims without the list being rebuilt.
    pub dead: DeadTracks,
    /// Favourite and library membership as they are *now*, which is not always
    /// what `entry` was fetched with. Same discipline as the two above.
    pub overrides: TrackOverrides,
    pub registry: RowRegistry<LibraryRowWidgets>,
}

/// The widgets a row publishes while it is on screen.
#[derive(Debug, Clone)]
pub struct LibraryRowWidgets {
    /// The favourite star, so it can be repainted without rebuilding the list.
    pub star: gtk::Image,
    pub icon: gtk::Image,
    pub root: gtk::Box,
    /// What the row menu will read if it opens next.
    ///
    /// Published because repainting the star is not enough: the menu decides
    /// what to offer from these facts, captured at bind time. Updating the
    /// model and the star but not this is why a just-un-starred row still
    /// offered "Remove Favourite".
    pub facts: std::rc::Rc<std::cell::RefCell<Option<RowFacts>>>,
}

impl LibraryRowWidgets {
    /// Bring a row already on screen up to date, without rebinding it.
    ///
    /// Both flags together on purpose. They were separate setters and a repaint
    /// called only one, so a row could show a cleared star while its menu still
    /// believed the song was in the library. Off-screen rows need none of this
    /// — they read the shared cell when they next bind.
    pub fn refresh(&self, favorite: bool, in_library: bool) {
        if let Some(facts) = self.facts.borrow_mut().as_mut() {
            facts.favorite = favorite;
            facts.in_library = in_library;
        }
        self.star.set_visible(favorite);
    }
}

impl LibraryItem {
    pub fn new(
        entry: Entry,
        registry: RowRegistry<LibraryRowWidgets>,
        current: CurrentTrack,
        dead: DeadTracks,
        overrides: TrackOverrides,
    ) -> Self {
        Self {
            entry,
            registry,
            current,
            dead,
            overrides,
        }
    }

    /// Streamable, and not on the refused list. Only songs can be unplayable —
    /// an album row always opens.
    pub fn playable(&self) -> bool {
        match &self.entry {
            Entry::Song(track) => match &track.catalog_id {
                Some(id) => !self.dead.borrow().contains(id),
                None => false,
            },
            _ => true,
        }
    }
}

/// Paint a row. Shared between the bind path and the live-update path so the
/// two cannot drift into showing different things for the same state.
pub fn apply_row_state(icon: &gtk::Image, root: &gtk::Box, playing: bool, playable: bool) {
    let (name, classes) = row_icon(playing, playable);
    icon.set_icon_name(Some(name));
    icon.set_css_classes(classes);
    // An unplayable track is shown, not hidden — it is in the library, and
    // pretending otherwise is more confusing than dimming it.
    root.set_sensitive(playable);
    root.set_tooltip_text(if playable {
        None
    } else {
        Some("Not available to stream — this track is only in your library")
    });
}

/// Icon name and CSS classes for a row's leading indicator.
pub fn row_icon(playing: bool, playable: bool) -> (&'static str, &'static [&'static str]) {
    if playing {
        ("media-playback-start-symbolic", &["accent"])
    } else if !playable {
        ("action-unavailable-symbolic", &["dim-label"])
    } else {
        ("audio-x-generic-symbolic", &["dim-label"])
    }
}

pub struct LibraryItemWidgets {
    star: gtk::Image,
    menu_button: gtk::Button,
    /// What this recycled widget is showing **right now**. The context-menu
    /// gesture is attached once in `setup` and lives as long as the widget, so
    /// it cannot capture a track — it reads this, which `bind` rewrites every
    /// time the row is reused. The same recycling rule as everything else here.
    showing: std::rc::Rc<std::cell::RefCell<Option<RowFacts>>>,
    icon: gtk::Image,
    title: gtk::Label,
    subtitle: gtk::Label,
    trailing: gtk::Label,
    chevron: gtk::Image,
}

impl RelmListItem for LibraryItem {
    type Root = gtk::Box;
    type Widgets = LibraryItemWidgets;

    fn setup(_item: &gtk::ListItem) -> (Self::Root, Self::Widgets) {
        crate::components::count_widget("track-row");

        let showing: std::rc::Rc<std::cell::RefCell<Option<RowFacts>>> = Default::default();

        view! {
            root = gtk::Box {
                set_orientation: gtk::Orientation::Horizontal,
                set_spacing: 12,
                set_margin_all: 8,

                #[name = "icon"]
                gtk::Image {
                    set_pixel_size: 16,
                    set_valign: gtk::Align::Center,
                },

                gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_valign: gtk::Align::Center,
                    set_hexpand: true,
                    set_spacing: 2,

                    #[name = "title"]
                    gtk::Label {
                        set_xalign: 0.0,
                        set_ellipsize: gtk::pango::EllipsizeMode::End,
                        // Names are plain text. Left as markup, anything with
                        // an `&` fails to render at all.
                        set_use_markup: false,
                    },

                    #[name = "subtitle"]
                    gtk::Label {
                        set_xalign: 0.0,
                        set_ellipsize: gtk::pango::EllipsizeMode::End,
                        set_use_markup: false,
                        add_css_class: "caption",
                        add_css_class: "dim-label",
                    },
                },

                // Only ever visible for a favourited track. Read straight off
                // `inFavorites`, which the library endpoint returns — no
                // read-back, no request per row.
                #[name = "star"]
                gtk::Image {
                    set_icon_name: Some("starred-symbolic"),
                    set_visible: false,
                    // Yellow, not the accent: a favourite is a star everywhere
                    // else it appears, including on the phone this syncs with.
                    add_css_class: "favorite-star",
                },

                // The same menu the right-click opens, as a button.
                //
                // A context menu you can only reach by right-clicking is a
                // context menu a touchscreen cannot reach at all — and a
                // trackpad user has to know is there. Always visible rather
                // than on hover, for the same reason.
                // A plain Button, not a MenuButton: a MenuButton owns its
                // popover, and this one has to come from the same place the
                // right-click menu does or the two will drift apart.
                #[name = "menu_button"]
                gtk::Button {
                    set_icon_name: "view-more-symbolic",
                    set_tooltip_text: Some("Track options"),
                    set_valign: gtk::Align::Center,
                    add_css_class: "flat",
                    add_css_class: "circular",
                },

                #[name = "trailing"]
                gtk::Label {
                    set_valign: gtk::Align::Center,
                    add_css_class: "numeric",
                    add_css_class: "dim-label",
                },

                #[name = "chevron"]
                gtk::Image {
                    set_icon_name: Some("go-next-symbolic"),
                    set_valign: gtk::Align::Center,
                    add_css_class: "dim-label",
                },
            }
        }

        // Secondary button only. A GtkGestureClick that claims *any* button
        // swallows the sequence — that is what silently killed scrubbing on the
        // seek scale — so this is scoped to button 3 and never sees the primary
        // click that activates a row.
        let menu = gtk::GestureClick::new();
        menu.set_button(gtk::gdk::BUTTON_SECONDARY);
        // The button asks for exactly the same menu, at its own position, so
        // there is one code path and it cannot drift from the right-click one.
        let asked_by_button = showing.clone();
        let button_root = root.clone();
        let button = menu_button.clone();
        menu_button.connect_clicked(move |_| {
            let Some(shown) = asked_by_button.borrow().clone() else {
                return;
            };
            if let Some(request) = ROW_MENU.with(|m| m.borrow().clone()) {
                // Where the button sits inside the row, so the popover points
                // at it rather than at wherever the last right-click was.
                // `allocation()` is deprecated; this is the replacement.
                let at = button
                    .compute_bounds(&button_root)
                    .map(|b| (b.x() as i32, (b.y() + b.height()) as i32))
                    .unwrap_or((0, 0));
                request(RowMenuRequest {
                    catalog_id: shown.catalog_id,
                    library_id: shown.library_id,
                    in_library: shown.in_library,
                    favorite: shown.favorite,
                    at,
                    over: button_root.clone(),
                });
            }
        });

        let asked = showing.clone();
        let root_for_menu = root.clone();
        menu.connect_pressed(move |gesture, _, x, y| {
            let Some(shown) = asked.borrow().clone() else {
                return; // an album or artist row: nothing to enqueue
            };
            if let Some(request) = ROW_MENU.with(|m| m.borrow().clone()) {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                request(RowMenuRequest {
                    catalog_id: shown.catalog_id,
                    library_id: shown.library_id,
                    in_library: shown.in_library,
                    favorite: shown.favorite,
                    at: (x as i32, y as i32),
                    over: root_for_menu.clone(),
                });
            }
        });
        root.add_controller(menu);

        (
            root,
            LibraryItemWidgets {
                star,
                menu_button,
                showing,
                icon,
                title,
                subtitle,
                trailing,
                chevron,
            },
        )
    }

    fn bind(&mut self, widgets: &mut Self::Widgets, root: &mut Self::Root) {
        // Before anything else: tell the context-menu gesture what it is
        // pointing at now. `None` for a row with nothing to enqueue.
        // One read, used for both the facts the menu will consult and the star
        // below — so the two cannot disagree about the same track.
        let (favorite, in_library) = match &self.entry {
            Entry::Song(track) => overridden(
                &self.overrides,
                track.catalog_id.as_deref(),
                track.favorite,
                track.in_library,
            ),
            _ => (false, false),
        };

        *widgets.showing.borrow_mut() = match &self.entry {
            Entry::Song(track) if self.playable() => {
                track.catalog_id.clone().map(|catalog_id| RowFacts {
                    catalog_id,
                    // Never inferred from `id`: a catalog row can be in the
                    // library too, and there `id` is the catalog id — handing
                    // that to the removal endpoint is a well-formed request
                    // that deletes nothing.
                    library_id: track.library_id.clone(),
                    in_library,
                    favorite,
                })
            }
            _ => None,
        };

        widgets.title.set_label(self.entry.title());
        widgets.subtitle.set_label(&self.entry.subtitle());

        // A chevron says "this opens something". Songs do not.
        let opens = self.entry.opens_a_page();
        widgets.chevron.set_visible(opens);
        widgets.trailing.set_visible(!opens);

        // Both set unconditionally: this widget was showing a different track a
        // moment ago, and anything left over from it is a lie about this one.
        widgets
            .menu_button
            .set_visible(widgets.showing.borrow().is_some());

        // A star left over from the previous track is a lie about this one —
        // and so is one baked in when the row was built, if it has been
        // un-starred since. Hence the shared cell rather than `track.favorite`.
        widgets.star.set_visible(favorite);

        match &self.entry {
            Entry::Song(track) => {
                widgets.trailing.set_label(&track.duration_label());

                let playable = self.playable();
                let playing = track.catalog_id.is_some()
                    && self.current.borrow().as_deref() == track.catalog_id.as_deref();
                apply_row_state(&widgets.icon, root, playing, playable);

                if let Some(id) = &track.catalog_id {
                    self.registry.borrow_mut().insert(
                        id.clone(),
                        LibraryRowWidgets {
                            star: widgets.star.clone(),
                            icon: widgets.icon.clone(),
                            root: root.clone(),
                            facts: widgets.showing.clone(),
                        },
                    );
                }
            }
            other => {
                // Albums and artists are never dimmed and never carry the play
                // marker, so they bypass the shared state entirely.
                widgets.icon.set_icon_name(Some(other.icon()));
                widgets.icon.set_css_classes(&["dim-label"]);
                root.set_sensitive(true);
                root.set_tooltip_text(None);
            }
        }
    }

    fn unbind(&mut self, _widgets: &mut Self::Widgets, _root: &mut Self::Root) {
        // The widget is about to be handed to another row.
        if let Some(id) = self.entry.catalog_id() {
            self.registry.borrow_mut().remove(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slipmat_core::music::types::TrackId;

    fn song(artist: &str, album: &str) -> Entry {
        Entry::Song(Track {
            id: TrackId("i.test".into()),
            catalog_id: Some("1".into()),
            favorite: false,
            in_library: false,
            library_id: None,
            date_added: String::new(),
            year: String::new(),
            title: "Title".into(),
            artist: artist.into(),
            album: album.into(),
            duration_ms: 200_000,
            track_number: 1,
            artwork: None,
        })
    }

    fn album(artist: &str, year: &str) -> Entry {
        Entry::Album(Album {
            date_added: String::new(),
            id: "1".into(),
            name: "Fragile".into(),
            artist: artist.into(),
            artwork: None,
            year: year.into(),
            track_count: 9,
            library: false,
        })
    }

    #[test]
    fn a_song_subtitle_collapses_rather_than_dangling() {
        assert_eq!(
            song("Aitana", "Superestrella").subtitle(),
            "Aitana — Superestrella"
        );
        assert_eq!(song("Aitana", "").subtitle(), "Aitana");
        assert_eq!(song("", "Album").subtitle(), "Album");
        assert_eq!(song("", "").subtitle(), "");
    }

    #[test]
    fn an_album_subtitle_collapses_too() {
        assert_eq!(album("Yes", "1971").subtitle(), "Yes · 1971");
        assert_eq!(album("Yes", "").subtitle(), "Yes");
        assert_eq!(album("", "1971").subtitle(), "1971");
        assert_eq!(album("", "").subtitle(), "");
    }

    #[test]
    fn only_songs_are_a_destination() {
        assert!(!song("a", "b").opens_a_page());
        assert!(album("a", "b").opens_a_page());
    }

    #[test]
    fn only_songs_have_a_catalog_id_to_match_against() {
        assert_eq!(song("a", "b").catalog_id(), Some("1"));
        assert_eq!(album("a", "b").catalog_id(), None);
    }
}
