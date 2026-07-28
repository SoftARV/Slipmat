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
pub const TILE_PX: i32 = 160;

/// Tiles currently bound, keyed by [`Artwork::cache_key`], so a fetch that
/// finishes late can find the widgets to paint.
///
/// A `Vec` per key, not a single `Cover`: two tiles can legitimately show one
/// artwork — see this module's header.
pub type ArtRegistry = Rc<RefCell<HashMap<String, Vec<Cover>>>>;

/// "Please fetch this artwork." Called from `bind` on a cache miss; the app
/// turns it into a relm4 `Command`.
pub type ArtRequest = Rc<dyn Fn(String, Artwork)>;

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

        (
            root,
            GridItemWidgets {
                cover,
                title,
                subtitle,
            },
        )
    }

    fn bind(&mut self, widgets: &mut Self::Widgets, _root: &mut Self::Root) {
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
