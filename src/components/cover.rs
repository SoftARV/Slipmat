// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The picture on a tile or a page header — square for a record, round for a
//! person.
//!
//! Two widgets, one visible at a time, because **a round portrait is an
//! `adw::Avatar`**. The obvious-looking alternative does not work: `circular`
//! is a libadwaita *button* style and has no rule matching a `GtkImage`, so
//! there is no border radius for `overflow: hidden` to clip to and the portrait
//! stays square. Reaching for custom CSS to invent one would be the wrong call
//! twice over — CLAUDE.md asks for a libadwaita widget where one exists, and
//! `AdwAvatar` is exactly that widget. It also draws initials when there is no
//! picture, which is what a library artist with no catalog twin needs.

use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;

use relm4::gtk::prelude::*;
use relm4::{adw, gtk};

#[derive(Clone)]
pub struct Cover {
    image: gtk::Image,
    avatar: adw::Avatar,
    /// Which of the two is currently showing. Held in a `Cell` behind the `Rc`
    /// that `Clone` shares, so the copy sitting in a registry — waiting for a
    /// download to land — still knows which widget to paint.
    round: Rc<Cell<bool>>,
}

impl Cover {
    pub fn new(size: i32) -> Self {
        // `halign: Center` and an explicit size are both load-bearing on the
        // image: `GtkImage` otherwise fills its allocation and centres the
        // picture inside it, which is invisible until the `card` background
        // paints that allocation and the cover ends up in a grey slab.
        let image = gtk::Image::builder()
            .pixel_size(size)
            .width_request(size)
            .height_request(size)
            .halign(gtk::Align::Center)
            .css_classes(["card"])
            .overflow(gtk::Overflow::Hidden)
            .build();

        let avatar = adw::Avatar::builder()
            .size(size)
            .show_initials(true)
            .halign(gtk::Align::Center)
            .visible(false)
            .build();

        Self {
            image,
            avatar,
            round: Rc::new(Cell::new(false)),
        }
    }

    /// Change how big the cover is drawn.
    ///
    /// All three of `pixel_size`, `width_request` and `height_request`, for the
    /// reason `new` gives: `GtkImage` fills its allocation and centres the
    /// picture inside it, so setting only one leaves the cover floating in a
    /// slab of `card` background.
    pub fn resize(&self, size: i32) {
        self.image.set_pixel_size(size);
        self.image.set_width_request(size);
        self.image.set_height_request(size);
        self.avatar.set_size(size);
    }

    /// Put both widgets at the **start** of a container. Only one is ever
    /// visible. Prepended in reverse so the picture lands above whatever the
    /// `view!` macro already built below it.
    pub fn attach_first(&self, parent: &gtk::Box) {
        parent.prepend(&self.avatar);
        parent.prepend(&self.image);
    }

    /// A record: square, with `icon` until the cover arrives.
    pub fn square(&self, icon: &str) {
        self.round.set(false);
        self.avatar.set_visible(false);
        self.image.set_visible(true);
        self.image.set_from_file(None::<&Path>);
        self.image.set_icon_name(Some(icon));
    }

    /// A person: round, with their initials until the portrait arrives.
    pub fn round(&self, name: &str) {
        self.round.set(true);
        self.image.set_visible(false);
        self.avatar.set_visible(true);
        // Cleared explicitly — this widget was very likely showing somebody
        // else a moment ago, and a stale portrait under a new name is worse
        // than no portrait.
        self.avatar.set_custom_image(gtk::gdk::Paintable::NONE);
        self.avatar.set_text(Some(name));
    }

    /// Show a picture that is now on disk.
    pub fn set_file(&self, path: &Path) {
        if self.round.get() {
            // `AdwAvatar` takes a paintable, not a path, and clips it to the
            // circle itself.
            match gtk::gdk::Texture::from_filename(path) {
                Ok(texture) => self.avatar.set_custom_image(Some(&texture)),
                // Cosmetic: the initials stay.
                Err(err) => tracing::debug!(?err, "decoding portrait"),
            }
        } else {
            self.image.set_from_file(Some(path));
        }
    }

    /// Widget identity, for a registry deciding whether an entry is still its
    /// own.
    pub fn is(&self, other: &Cover) -> bool {
        self.image == other.image
    }
}
