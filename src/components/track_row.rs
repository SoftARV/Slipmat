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
use relm4::binding::{BoolBinding, StringBinding};
use relm4::prelude::*;
use relm4::typed_view::list::RelmListItem;
use relm4::{gtk, view};

use crate::music::types::Track;

#[derive(Debug, Clone)]
pub struct LibraryItem {
    pub track: Track,
    /// Whether this is the track currently playing. A binding rather than a
    /// plain field so a change repaints the bound widget without the list
    /// having to rebuild.
    pub playing: BoolBinding,
    pub title: StringBinding,
    pub subtitle: StringBinding,
}

impl LibraryItem {
    pub fn new(track: Track) -> Self {
        let subtitle = match (track.artist.is_empty(), track.album.is_empty()) {
            (false, false) => format!("{} — {}", track.artist, track.album),
            (false, true) => track.artist.clone(),
            (true, false) => track.album.clone(),
            (true, true) => String::new(),
        };
        Self {
            title: StringBinding::new(track.title.clone()),
            subtitle: StringBinding::new(subtitle),
            playing: BoolBinding::new(false),
            track,
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
        widgets.title.set_label(&self.title.value());
        widgets.subtitle.set_label(&self.subtitle.value());
        widgets.duration.set_label(&self.track.duration_label());

        let playing = self.playing.value();
        widgets.icon.set_icon_name(Some(if playing {
            "media-playback-start-symbolic"
        } else if !self.track.playable() {
            "action-unavailable-symbolic"
        } else {
            "audio-x-generic-symbolic"
        }));
        widgets
            .icon
            .set_css_classes(if playing { &["accent"] } else { &["dim-label"] });

        // An unplayable track is shown, not hidden — it is in the library, and
        // pretending otherwise is more confusing than dimming it.
        root.set_sensitive(self.track.playable());
        root.set_tooltip_text(if self.track.playable() {
            None
        } else {
            Some("Not available to stream — this track is only in your library")
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            LibraryItem::new(track("Aitana", "Superestrella"))
                .subtitle
                .value(),
            "Aitana — Superestrella"
        );
        assert_eq!(
            LibraryItem::new(track("Aitana", "")).subtitle.value(),
            "Aitana"
        );
        assert_eq!(
            LibraryItem::new(track("", "Album")).subtitle.value(),
            "Album"
        );
        assert_eq!(LibraryItem::new(track("", "")).subtitle.value(), "");
    }
}
