// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Tiles for the Albums and Artists grids.
//!
//! The same shape as `track_row`, one dimension over: a `RelmGridItem` bound
//! into a recycled widget. All of that module's rules apply — **`bind` must set
//! every property it cares about**, because the tile it is handed was showing a
//! different album a moment ago.
//!
//! ## Artwork
//!
//! A grid is mostly pictures, and Apple gives us URL *templates* rather than
//! images. Fetching happens the only way it can here (rule 8, never block the
//! GTK thread): `bind` asks the app — through a callback — for the artwork, and
//! the app answers as a `Command` with pixels already decoded off the thread
//! (#27), repainting through [`ArtRegistry`] exactly as the play marker is.
//!
//! `bind` consults no cache of its own, deliberately. It used to, and the
//! decode it saved was the 2.5ms-per-cover one that froze the UI for half a
//! second per grid; the disk cache still exists, but it is consulted by
//! `artwork::fetch` on the worker, where paying for it costs nobody a frame.
//!
//! The consequence of recycling: a tile that requested art may be showing a
//! different album by the time the file arrives. The registry is keyed by the
//! artwork's own cache key, so a late arrival lands on whichever tiles are
//! showing that artwork *now* — or on none, which is correct.
//!
//! **Plural, and that is the fix for a bug that looked like flaky downloads.**
//! The registry used to hold one `Cover` per key, on the stated assumption of
//! "one key, one live tile". That is false twice over: a multi-disc set, an EP
//! beside its single, or a compilation give two visible tiles the *same*
//! artwork template and therefore the same key; and the three grids are
//! searched in order, so a still-bound tile in a grid the user cannot see
//! shadowed the visible one. Either way the second tile to bind took the map
//! entry and the first kept its placeholder until something forced it to
//! rebind — which reads exactly like an image that failed to load.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use relm4::RelmWidgetExt;
use relm4::gtk::prelude::*;
use relm4::typed_view::grid::RelmGridItem;
use relm4::{gtk, view};

use crate::components::cover::Cover;
use crate::music::types::{Album, Artist, Artwork, Playlist};

/// Tile artwork, in logical pixels. Big enough to read, small enough that a
/// library of a few hundred albums is a few hundred small JPEGs.
/// How wide a grid tile asks to be.
///
/// **130, not 160, so two columns fit a narrow window.** A tile claims
/// `TILE_PX + 12` of margin and the grid adds 24 of padding, so two columns
/// need `24 + 2 × (TILE_PX + 12)`. At 160 that is 344 against the 336 a 360px
/// window leaves — missing a second column by 8px, and dropping to one.
///
/// Independent of `TILE_ART`, the size covers are *fetched* at, so changing it
/// costs nothing on disk: the cache is untouched and the tile simply draws the
/// same image smaller.
pub const TILE_PX: i32 = 130;

/// Tiles currently bound, keyed by [`Artwork::cache_key`], so a fetch that
/// finishes late can find the widgets to paint.
///
/// A `Vec` per key, not a single `Cover`: two tiles can legitimately show one
/// artwork — see this module's header.
pub type ArtRegistry = Rc<RefCell<HashMap<String, Vec<Cover>>>>;

/// "Please fetch this artwork." Called from `bind` on a cache miss; the app
/// turns it into a relm4 `Command`.
pub type ArtRequest = Rc<dyn Fn(String, Artwork)>;

/// What a right-click on a playlist tile asks for.
///
/// The tile carries the playlist's identity and where it was clicked, and
/// nothing else: whether it is pinned is the app's to know, and asking the tile
/// would mean telling every tile about the sidebar.
#[derive(Debug)]
pub struct TileMenuRequest {
    /// The **library** id — the same one a pin stores.
    pub playlist_id: String,
    pub at: (i32, i32),
    pub over: gtk::Box,
}

type TileMenuHandler = Rc<dyn Fn(TileMenuRequest)>;

thread_local! {
    /// Set once at startup, for the same reason as `track_row`'s: the gesture is
    /// created in `setup`, a *static* method with no access to any item.
    static TILE_MENU: RefCell<Option<TileMenuHandler>> = const { RefCell::new(None) };
}

/// Install the handler that shows a tile's context menu. Called once, from the
/// root component's `init`.
pub fn set_tile_menu(handler: impl Fn(TileMenuRequest) + 'static) {
    TILE_MENU.with(|m| *m.borrow_mut() = Some(Rc::new(handler)));
}

pub fn art_registry() -> ArtRegistry {
    Rc::new(RefCell::new(HashMap::new()))
}

/// "Is this the same widget?" — the one question the registry asks.
///
/// A trait rather than `PartialEq` because [`Cover`] is a bundle of widgets
/// with shared interior state, and identity here means *the same widget*, not
/// "showing the same thing". Two tiles displaying one album are equal in every
/// sense that matters to a user and must still be tracked separately.
pub trait SameWidget {
    fn same(&self, other: &Self) -> bool;
}

impl SameWidget for Cover {
    fn same(&self, other: &Self) -> bool {
        self.is(other)
    }
}

/// Claim `item` as showing `key`.
///
/// Idempotent: `bind` can be called on a widget already registered for this
/// key — rebinding the same tile to the same item — and the same widget twice
/// in the list would be paint work done twice for ever.
fn register<T: SameWidget + Clone>(registry: &mut HashMap<String, Vec<T>>, key: String, item: &T) {
    let showing = registry.entry(key).or_default();
    if !showing.iter().any(|c| c.same(item)) {
        showing.push(item.clone());
    }
}

/// Give up `item`'s claim on `key`, leaving every other claimant alone.
///
/// The half that used to be wrong by construction: with one widget per key
/// there was nothing to leave alone, so unbinding one tile silently
/// unregistered whatever other tile had taken the entry.
fn unregister<T: SameWidget>(registry: &mut HashMap<String, Vec<T>>, key: &str, item: &T) {
    let Some(showing) = registry.get_mut(key) else {
        return;
    };
    showing.retain(|c| !c.same(item));
    if showing.is_empty() {
        registry.remove(key);
    }
}

/// What a tile stands for.
#[derive(Debug, Clone)]
pub enum Tile {
    Album(Album),
    Artist(Artist),
    Playlist(Playlist),
}

impl Tile {
    pub fn title(&self) -> &str {
        match self {
            Self::Album(a) => &a.name,
            Self::Artist(a) => &a.name,
            Self::Playlist(p) => &p.name,
        }
    }

    /// The line under the title: an album's artist, an artist's album count.
    fn subtitle(&self) -> String {
        match self {
            Self::Album(a) => a.artist.clone(),
            // Genres come from the artist's catalog twin, which the client
            // asks for inline. Empty when Apple had none — an empty line beats
            // a fabricated one.
            Self::Artist(a) => a.genres.clone(),
            // The curator, or nothing. Deliberately **not** the description as
            // a fallback: Apple's blurbs are sentences with newlines in them,
            // and a tile is one line of caption. Falling back to one made a
            // single playlist grow to a dozen lines and shove the whole grid
            // out of shape. The page has room for the blurb; a tile does not.
            Self::Playlist(p) => p.curator.clone(),
        }
    }

    fn artwork(&self) -> Option<&Artwork> {
        match self {
            Self::Album(a) => a.artwork.as_ref(),
            Self::Artist(a) => a.artwork.as_ref(),
            Self::Playlist(p) => p.artwork.as_ref(),
        }
    }

    /// Shown until the picture arrives, and permanently for anything Apple has
    /// no picture for.
    fn placeholder(&self) -> &'static str {
        match self {
            Self::Album(_) => "media-optical-symbolic",
            Self::Artist(_) => "avatar-default-symbolic",
            Self::Playlist(_) => "view-list-symbolic",
        }
    }

    /// Artists are round, albums are square. The same distinction the detail
    /// pages make, so a tile and the page it opens agree with each other.
    fn round(&self) -> bool {
        matches!(self, Self::Artist(_))
    }
}

/// Squash any run of whitespace — newlines included — into single spaces.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub struct GridItem {
    pub tile: Tile,
    registry: ArtRegistry,
    request: ArtRequest,
}

impl GridItem {
    pub fn new(tile: Tile, registry: ArtRegistry, request: ArtRequest) -> Self {
        Self {
            tile,
            registry,
            request,
        }
    }
}

pub struct GridItemWidgets {
    cover: Cover,
    title: gtk::Label,
    subtitle: gtk::Label,
    /// Which playlist this widget is currently showing, or `None` for an album,
    /// an artist, or a tile between binds.
    ///
    /// The gesture is connected once in `setup` and lives as long as the widget,
    /// which outlives every tile it displays — so it cannot capture one. A
    /// right-click is a person, and a person is slow: the popover opens seconds
    /// later, by which time the grid may have recycled this widget onto
    /// something else. Reading the cell at click time is what keeps the menu
    /// about the tile under the pointer.
    showing: Rc<RefCell<Option<String>>>,
}

impl RelmGridItem for GridItem {
    type Root = gtk::Box;
    type Widgets = GridItemWidgets;

    fn setup(_item: &gtk::ListItem) -> (Self::Root, Self::Widgets) {
        crate::components::count_widget("grid-tile");

        view! {
            root = gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_spacing: 6,
                set_margin_all: 6,
                set_width_request: TILE_PX,

                // `halign: Fill` — the default — is load-bearing, and centring
                // is done with `xalign` instead. A centred label is allocated
                // its *natural* width, and `max_width_chars: 1` caps that at one
                // character, so the pair rendered every title as a bare "…".
                //
                // `max_width_chars` still earns its place: without it a long
                // album title would set the natural width of every column in
                // the grid, since GridView allocates all columns to the widest
                // child. Capped natural, filled allocation, ellipsis in
                // between.
                #[name = "title"]
                gtk::Label {
                    set_ellipsize: gtk::pango::EllipsizeMode::End,
                    set_max_width_chars: 1,
                    set_xalign: 0.5,
                    add_css_class: "heading",
                },

                #[name = "subtitle"]
                gtk::Label {
                    set_ellipsize: gtk::pango::EllipsizeMode::End,
                    set_max_width_chars: 1,
                    set_xalign: 0.5,
                    add_css_class: "caption",
                    add_css_class: "dim-label",
                },
            }
        }

        let cover = Cover::new(TILE_PX);
        cover.attach_first(&root);

        let showing: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

        // Secondary button only. A `GtkGestureClick` claiming *any* button
        // swallows the sequence, which is what once silently killed scrubbing on
        // the seek scale — so this never sees the click that opens a tile.
        let menu = gtk::GestureClick::new();
        menu.set_button(gtk::gdk::BUTTON_SECONDARY);
        let asked = showing.clone();
        let over = root.clone();
        menu.connect_pressed(move |gesture, _, x, y| {
            let Some(playlist_id) = asked.borrow().clone() else {
                return; // an album or an artist: nothing to pin
            };
            let Some(request) = TILE_MENU.with(|m| m.borrow().clone()) else {
                return;
            };
            gesture.set_state(gtk::EventSequenceState::Claimed);
            request(TileMenuRequest {
                playlist_id,
                at: (x as i32, y as i32),
                over: over.clone(),
            });
        });
        root.add_controller(menu);

        (
            root,
            GridItemWidgets {
                cover,
                title,
                subtitle,
                showing,
            },
        )
    }

    fn bind(&mut self, widgets: &mut Self::Widgets, _root: &mut Self::Root) {
        // Set unconditionally, including back to `None`: this widget was showing
        // something else a moment ago, and a recycled album that kept a
        // playlist's id would offer to pin the wrong thing entirely.
        *widgets.showing.borrow_mut() = match &self.tile {
            Tile::Playlist(playlist) => Some(playlist.id.clone()),
            Tile::Album(_) | Tile::Artist(_) => None,
        };

        let title = one_line(self.tile.title());
        widgets.title.set_label(&title);
        // The full, untruncated name on hover — the tile always ellipsizes.
        widgets.title.set_tooltip_text(Some(self.tile.title()));

        // Collapsed to one line whatever it is. Ellipsizing caps a label's
        // *width*; an embedded newline still makes it two lines tall, and one
        // tall tile drags its whole row with it.
        let subtitle = one_line(&self.tile.subtitle());
        widgets.subtitle.set_visible(!subtitle.is_empty());
        widgets.subtitle.set_label(&subtitle);

        // Shape first, and unconditionally: this widget was showing a different
        // tile a moment ago, and a recycled artist must not stay round when it
        // comes back as an album.
        if self.tile.round() {
            widgets.cover.round(&title);
        } else {
            widgets.cover.square(self.tile.placeholder());
        }

        if let Some(art) = self.tile.artwork() {
            let key = art.cache_key();
            // **Always ask; never decode here.** A cover already on disk used
            // to be loaded inline, which is a 2.5ms JPEG decode on the GTK
            // thread — fine for one tile, and half a second of frozen UI for
            // the ~385 a grid materialises in a single frame (#27).
            //
            // The request answers off the thread either way: it fetches only
            // if the file is missing, and decodes in both cases. The tile keeps
            // its placeholder for a frame or two and then fills in.
            (self.request)(key.clone(), art.clone());
            register(&mut self.registry.borrow_mut(), key, &widgets.cover);
        }
    }

    fn unbind(&mut self, widgets: &mut Self::Widgets, _root: &mut Self::Root) {
        // Stop claiming a widget this item no longer owns, or a late fetch
        // paints the wrong tile. Only *this* widget goes; other tiles showing
        // the same artwork are still live and still want it.
        if let Some(art) = self.tile.artwork() {
            let key = art.cache_key();
            unregister(&mut self.registry.borrow_mut(), &key, &widgets.cover);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stands in for a `Cover`, which cannot be built without GTK on the main
    /// thread. Identity is the id; two stand-ins can share a key exactly as two
    /// tiles can share one album cover.
    #[derive(Clone, Debug, PartialEq)]
    struct Widget(u32);

    impl SameWidget for Widget {
        fn same(&self, other: &Self) -> bool {
            self.0 == other.0
        }
    }

    #[test]
    fn one_artwork_can_be_claimed_by_several_tiles() {
        // The bug this replaced: the registry held a single widget per key, so
        // a multi-disc set or an EP beside its single — same artwork template,
        // same cache key — meant whichever tile bound second took the entry and
        // the first never got painted at all.
        let mut registry = HashMap::new();
        register(&mut registry, "k".into(), &Widget(1));
        register(&mut registry, "k".into(), &Widget(2));

        assert_eq!(
            registry.get("k"),
            Some(&vec![Widget(1), Widget(2)]),
            "both tiles showing this artwork must be painted"
        );
    }

    #[test]
    fn rebinding_the_same_tile_does_not_claim_it_twice() {
        // `bind` runs again whenever a tile is recycled onto the same item, and
        // a duplicate entry would be one wasted repaint per arrival, for ever.
        let mut registry = HashMap::new();
        register(&mut registry, "k".into(), &Widget(1));
        register(&mut registry, "k".into(), &Widget(1));

        assert_eq!(registry.get("k").map(Vec::len), Some(1));
    }

    #[test]
    fn unbinding_one_tile_leaves_the_others_claiming_it() {
        // The other half of the same bug. With one widget per key there was
        // nothing to leave alone, so unbinding *any* tile unregistered whatever
        // tile happened to hold the entry.
        let mut registry = HashMap::new();
        register(&mut registry, "k".into(), &Widget(1));
        register(&mut registry, "k".into(), &Widget(2));
        unregister(&mut registry, "k", &Widget(1));

        assert_eq!(registry.get("k"), Some(&vec![Widget(2)]));
    }

    #[test]
    fn the_last_tile_to_leave_removes_the_key() {
        // Or the registry grows by one empty `Vec` per cover ever scrolled past.
        let mut registry = HashMap::new();
        register(&mut registry, "k".into(), &Widget(1));
        unregister(&mut registry, "k", &Widget(1));

        assert!(registry.is_empty(), "an empty claim list must not linger");
    }

    #[test]
    fn unregistering_something_never_registered_is_harmless() {
        // `unbind` can fire for a tile whose `bind` never registered — an item
        // with no artwork at all — and must not panic or invent an entry.
        let mut registry: HashMap<String, Vec<Widget>> = HashMap::new();
        unregister(&mut registry, "missing", &Widget(9));
        assert!(registry.is_empty());
    }

    #[test]
    fn a_subtitle_is_always_one_line() {
        // Apple's playlist blurbs contain newlines. A tile is one line of
        // caption, and a tile that grows drags its whole grid row with it.
        assert_eq!(
            one_line("Taken right from\nplaying the game"),
            "Taken right from playing the game"
        );
        assert_eq!(one_line("  spaced \t out \n\n"), "spaced out");
        assert_eq!(one_line(""), "");
    }
}
