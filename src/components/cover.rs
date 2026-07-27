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

/// How much of an empty sleeve the disc inside it takes up.
///
/// The bar draws a 22px disc in a 48px case and that proportion is what makes
/// it read as a sleeve rather than as a large icon, so the drawer scales the
/// same ratio up rather than picking a second number.
fn disc_px(size: i32) -> i32 {
    (size * 22 / 48).max(1)
}

/// How long a cover takes to fade into what replaces it.
///
/// `pub` so the bar and the drawer fade their words over the same span. Two
/// halves of one state change finishing apart is more noticeable than either
/// being slightly wrong on its own — the same argument the artwork's colour
/// and its backdrop already settled.
pub const SWAP_MS: u32 = 220;

#[derive(Clone)]
pub struct Cover {
    /// The three faces, one showing. A `GtkStack` rather than a `GtkBox` with
    /// visibility flipped: a stack cross-fades between its pages, so a record
    /// becoming an empty sleeve — or a sleeve becoming a record — is a
    /// dissolve rather than a cut. Nothing else changes; only one page is ever
    /// visible either way.
    stack: gtk::Stack,
    image: gtk::Image,
    avatar: adw::Avatar,
    /// The empty sleeve, kept as a page of its own so it can fade rather than
    /// being the same widget with its icon swapped underneath you.
    sleeve: gtk::Image,
    /// Whether the picture is currently an **empty sleeve** rather than a
    /// cover. Behind the same `Rc` as `round`, and for the same reason: a
    /// clone in a registry has to agree about what it is drawing.
    empty: Rc<Cell<bool>>,
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
            .build();

        let sleeve = gtk::Image::builder()
            .icon_name("media-optical-symbolic")
            .pixel_size(disc_px(size))
            .width_request(size)
            .height_request(size)
            .halign(gtk::Align::Center)
            .css_classes(["card", "np-cover-empty"])
            .build();

        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(SWAP_MS)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        stack.add_named(&image, Some("image"));
        stack.add_named(&avatar, Some("avatar"));
        stack.add_named(&sleeve, Some("sleeve"));
        // Said out loud rather than left to add-order. A `GtkStack` shows
        // whichever child went in first, and relying on that is how the bar
        // launched with a hole where its sleeve should be — the first page
        // there was an image with no file and no icon, which draws nothing.
        // Every caller sets a face before this is seen, so "image" only has to
        // be a sane default; it must not be an accidental one.
        stack.set_visible_child_name("image");

        Self {
            stack,
            image,
            avatar,
            sleeve,
            round: Rc::new(Cell::new(false)),
            empty: Rc::new(Cell::new(false)),
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
        // The sleeve draws its disc *inside* the case, so its icon stays
        // proportionally smaller than the widget while the case matches.
        self.sleeve.set_pixel_size(disc_px(size));
        self.sleeve.set_width_request(size);
        self.sleeve.set_height_request(size);
        self.avatar.set_size(size);
    }

    /// Put both widgets at the **start** of a container. Only one is ever
    /// visible. Prepended in reverse so the picture lands above whatever the
    /// `view!` macro already built below it.
    pub fn attach_first(&self, parent: &gtk::Box) {
        parent.prepend(&self.stack);
    }

    /// An empty sleeve: the case, with a disc sitting inside it.
    ///
    /// Not the same as [`Cover::square`] with a disc icon. That draws a
    /// floating glyph; this draws a *place the artwork goes*, which is what
    /// the Now Playing bar has always done and what the drawer was missing —
    /// with nothing playing it showed a bare generic icon in the middle of a
    /// 260px square, which reads as a failure rather than as an empty player.
    ///
    /// `.np-cover-empty` is the same rule the bar uses, so the two states
    /// cannot drift apart.
    pub fn empty_sleeve(&self, size: i32) {
        self.round.set(false);
        self.empty.set(true);
        self.resize(size);
        self.stack.set_visible_child_name("sleeve");
    }

    /// A record: square, with `icon` until the cover arrives.
    pub fn square(&self, icon: &str) {
        self.round.set(false);
        self.empty.set(false);
        self.image.set_from_file(None::<&Path>);
        self.image.set_icon_name(Some(icon));
        self.stack.set_visible_child_name("image");
    }

    /// A person: round, with their initials until the portrait arrives.
    pub fn round(&self, name: &str) {
        self.round.set(true);
        self.empty.set(false);
        self.stack.set_visible_child_name("avatar");
        // Cleared explicitly — this widget was very likely showing somebody
        // else a moment ago, and a stale portrait under a new name is worse
        // than no portrait.
        self.avatar.set_custom_image(gtk::gdk::Paintable::NONE);
        self.avatar.set_text(Some(name));
    }

    /// Show a cover that was decoded off the GTK thread.
    ///
    /// The counterpart to [`Cover::set_file`], and the one a grid tile uses:
    /// `set_file` decodes where it is called, which for a grid means hundreds
    /// of decodes in the frame that fills it (#27).
    pub fn set_decoded(&self, cover: crate::components::artwork::Decoded) {
        self.empty.set(false);
        let texture = cover.into_texture();
        // **Shape first, same as `set_file`.** A round cover is an `AdwAvatar`,
        // not an image with a radius — see this module's header for why. Going
        // straight to the image page put every artist who *has* a portrait in a
        // square, while the ones without kept their circle, which is a strange
        // enough result to look like two different bugs.
        if self.round.get() {
            self.avatar.set_custom_image(Some(&texture));
            self.stack.set_visible_child_name("avatar");
        } else {
            self.image.set_paintable(Some(&texture));
            self.stack.set_visible_child_name("image");
        }
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
            self.empty.set(false);
            self.image.set_from_file(Some(path));
            self.stack.set_visible_child_name("image");
        }
    }

    /// Widget identity, for a registry deciding whether an entry is still its
    /// own.
    pub fn is(&self, other: &Cover) -> bool {
        self.image == other.image
    }
}
