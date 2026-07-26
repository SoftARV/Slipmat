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
//! GTK thread): `bind` consults a shared cache of already-downloaded files, and
//! on a miss asks the app — through a callback — to fetch it as a `Command`.
//! When the file lands, the app writes it into the cache and repaints the tile
//! through [`ArtRegistry`], exactly as the play marker is repainted.
//!
//! The consequence of recycling: a tile that requested art may be showing a
//! different album by the time the file arrives. The registry is keyed by the
//! artwork's own cache key, so a late arrival lands on whichever tile is
//! showing that artwork *now* — or on none, which is correct.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use relm4::RelmWidgetExt;
use relm4::gtk::prelude::*;
use relm4::typed_view::grid::RelmGridItem;
use relm4::{gtk, view};

use crate::music::types::{Album, Artist, Artwork, Playlist};

/// Tile artwork, in logical pixels. Big enough to read, small enough that a
/// library of a few hundred albums is a few hundred small JPEGs.
pub const TILE_PX: i32 = 160;

/// Artwork files already on disk, keyed by [`Artwork::cache_key`]. Shared by
/// both grids: an album's cover and that album's artist tile are often the same
/// image, and downloading it twice would be silly.
pub type ArtCache = Rc<RefCell<HashMap<String, PathBuf>>>;

/// Tiles currently on screen, keyed the same way, so a fetch that finishes late
/// can find the widget to paint.
pub type ArtRegistry = Rc<RefCell<HashMap<String, gtk::Image>>>;

/// "Please fetch this artwork." Called from `bind` on a cache miss; the app
/// turns it into a relm4 `Command`.
pub type ArtRequest = Rc<dyn Fn(String, Artwork)>;

pub fn art_cache() -> ArtCache {
    Rc::new(RefCell::new(HashMap::new()))
}

pub fn art_registry() -> ArtRegistry {
    Rc::new(RefCell::new(HashMap::new()))
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
            // Your own playlists have no curator and no blurb, which is most of
            // a library. Whichever exists, or nothing.
            Self::Playlist(p) => {
                if p.curator.is_empty() {
                    p.description.clone()
                } else {
                    p.curator.clone()
                }
            }
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

pub struct GridItem {
    pub tile: Tile,
    art: ArtCache,
    registry: ArtRegistry,
    request: ArtRequest,
}

impl GridItem {
    pub fn new(tile: Tile, art: ArtCache, registry: ArtRegistry, request: ArtRequest) -> Self {
        Self {
            tile,
            art,
            registry,
            request,
        }
    }
}

pub struct GridItemWidgets {
    image: gtk::Image,
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

                #[name = "image"]
                gtk::Image {
                    set_pixel_size: TILE_PX,
                    set_width_request: TILE_PX,
                    set_height_request: TILE_PX,
                    set_halign: gtk::Align::Center,
                    // Clip the picture to whatever shape the CSS draws — GTK4
                    // rounds the background but not the content on its own.
                    set_overflow: gtk::Overflow::Hidden,
                },

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

        (
            root,
            GridItemWidgets {
                image,
                title,
                subtitle,
            },
        )
    }

    fn bind(&mut self, widgets: &mut Self::Widgets, _root: &mut Self::Root) {
        widgets.title.set_label(self.tile.title());
        widgets.title.set_tooltip_text(Some(self.tile.title()));

        let subtitle = self.tile.subtitle();
        widgets.subtitle.set_visible(!subtitle.is_empty());
        widgets.subtitle.set_label(&subtitle);

        // Shape first, so a recycled artist tile does not stay round when it is
        // reused for an album.
        widgets.image.set_css_classes(if self.tile.round() {
            &["circular"]
        } else {
            &["card"]
        });

        match self.tile.artwork() {
            Some(art) => {
                let key = art.cache_key();
                match self.art.borrow().get(&key) {
                    // Already on disk from an earlier bind, or from the Now
                    // Playing bar having played something off this album.
                    Some(path) if path.is_file() => widgets.image.set_from_file(Some(path)),
                    _ => {
                        widgets.image.set_icon_name(Some(self.tile.placeholder()));
                        (self.request)(key.clone(), art.clone());
                    }
                }
                self.registry
                    .borrow_mut()
                    .insert(key, widgets.image.clone());
            }
            None => widgets.image.set_icon_name(Some(self.tile.placeholder())),
        }
    }

    fn unbind(&mut self, widgets: &mut Self::Widgets, _root: &mut Self::Root) {
        // Stop claiming a widget this item no longer owns, or a late fetch
        // paints the wrong tile.
        if let Some(art) = self.tile.artwork() {
            let key = art.cache_key();
            let mut registry = self.registry.borrow_mut();
            if registry.get(&key).is_some_and(|w| w == &widgets.image) {
                registry.remove(&key);
            }
        }
    }
}
