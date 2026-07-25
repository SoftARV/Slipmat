// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! One track in the library list.
//!
//! A `RelmListItem` over `gtk::ListView`, **not** a factory over
//! `gtk::ListBox`. A `ListBox` builds a real widget per row, so a 541-track
//! library meant 541 live `AdwActionRow`s — each with its own labels, icon and
//! button — and the whole app felt heavy. `ListView` recycles: it keeps roughly
//! as many widgets as fit on screen and rebinds them as you scroll, so the cost
//! stops depending on the size of the library.
//!
//! The consequence to keep in mind: widgets are **reused**. `bind` must set
//! every property it cares about, because the widget it is handed was showing a
//! different track a moment ago.

use relm4::adw::prelude::*;
use relm4::prelude::*;
use relm4::typed_view::list::RelmListItem;
use relm4::{gtk, view};

use crate::components::{CurrentTrack, RowRegistry};
use crate::music::types::Track;

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

#[derive(Debug, Clone)]
pub struct LibraryItem {
    pub track: Track,
    /// Who is playing, shared with every other row. Read at `bind`, never
    /// stored per-item: a per-item flag has to be changed in the model, and any
    /// model edit costs the scroll position (see `CurrentTrack`).
    pub current: CurrentTrack,
    pub subtitle: String,
    /// Where this row publishes its icon while it is on screen, so the marker
    /// can be moved without touching the model. See `RowRegistry`.
    pub registry: RowRegistry<gtk::Image>,
}

impl LibraryItem {
    pub fn new(track: Track, registry: RowRegistry<gtk::Image>, current: CurrentTrack) -> Self {
        let subtitle = match (track.artist.is_empty(), track.album.is_empty()) {
            (false, false) => format!("{} — {}", track.artist, track.album),
            (false, true) => track.artist.clone(),
            (true, false) => track.album.clone(),
            (true, true) => String::new(),
        };
        Self {
            subtitle,
            current,
            track,
            registry,
        }
    }
}

pub struct LibraryItemWidgets {
    icon: gtk::Image,
    title: gtk::Label,
    subtitle: gtk::Label,
    duration: gtk::Label,
}

impl RelmListItem for LibraryItem {
    type Root = gtk::Box;
    type Widgets = LibraryItemWidgets;

    fn setup(_item: &gtk::ListItem) -> (Self::Root, Self::Widgets) {
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
                        // Track names are plain text. Left as markup, anything
                        // containing `&` fails to render entirely.
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

                #[name = "duration"]
                gtk::Label {
                    set_valign: gtk::Align::Center,
                    add_css_class: "numeric",
                    add_css_class: "dim-label",
                },
            }
        }

        (
            root,
            LibraryItemWidgets {
                icon,
                title,
                subtitle,
                duration,
            },
        )
    }

    fn bind(&mut self, widgets: &mut Self::Widgets, root: &mut Self::Root) {
        // Everything is set unconditionally: this widget was showing another
        // track a moment ago, and anything left unset keeps that track's value.
        widgets.title.set_label(&self.track.title);
        widgets.subtitle.set_label(&self.subtitle);
        widgets.duration.set_label(&self.track.duration_label());

        let playing = self.current.borrow().as_deref() == self.track.catalog_id.as_deref()
            && self.track.catalog_id.is_some();
        let (icon, classes) = row_icon(playing, self.track.playable());
        widgets.icon.set_icon_name(Some(icon));
        widgets.icon.set_css_classes(classes);

        // Publish this row's icon while it is on screen.
        if let Some(id) = &self.track.catalog_id {
            self.registry
                .borrow_mut()
                .insert(id.clone(), widgets.icon.clone());
        }

        // An unplayable track is shown, not hidden — it is in the library, and
        // pretending otherwise is more confusing than dimming it.
        root.set_sensitive(self.track.playable());
        root.set_tooltip_text(if self.track.playable() {
            None
        } else {
            Some("Not available to stream — this track is only in your library")
        });
    }

    fn unbind(&mut self, _widgets: &mut Self::Widgets, _root: &mut Self::Root) {
        // The widget is about to be handed to another track.
        if let Some(id) = &self.track.catalog_id {
            self.registry.borrow_mut().remove(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{current_track, row_registry};
    use crate::music::types::TrackId;

    fn track(artist: &str, album: &str) -> Track {
        Track {
            id: TrackId("i.test".into()),
            catalog_id: Some("1".into()),
            title: "Title".into(),
            artist: artist.into(),
            album: album.into(),
            duration_ms: 200_000,
            track_number: 1,
            artwork: None,
        }
    }

    #[test]
    fn subtitle_collapses_rather_than_showing_a_dangling_dash() {
        assert_eq!(
            LibraryItem::new(
                track("Aitana", "Superestrella"),
                row_registry(),
                current_track()
            )
            .subtitle,
            "Aitana — Superestrella"
        );
        assert_eq!(
            LibraryItem::new(track("Aitana", ""), row_registry(), current_track()).subtitle,
            "Aitana"
        );
        assert_eq!(
            LibraryItem::new(track("", "Album"), row_registry(), current_track()).subtitle,
            "Album"
        );
        assert_eq!(
            LibraryItem::new(track("", ""), row_registry(), current_track()).subtitle,
            ""
        );
    }
}
