// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! One track in the library list.
//!
//! A `FactoryComponent` over `adw::ActionRow`, so the list is reconciled by
//! index rather than rebuilt — the same discipline as Pitwall's run rows.

use relm4::adw::prelude::*;
use relm4::factory::FactoryComponent;
use relm4::prelude::DynamicIndex;
use relm4::{FactorySender, adw, gtk};

use crate::music::types::Track;

#[derive(Debug)]
pub struct TrackRow {
    pub track: Track,
    /// Index in the *visible* list, which is what gets enqueued. Not the index
    /// in the full library — filtering changes it.
    index: usize,
    playing: bool,
}

#[derive(Debug, Clone)]
pub enum TrackRowInput {
    /// The now-playing track changed; highlight or un-highlight this row.
    NowPlaying(Option<String>),
    /// MusicKit has rejected these ids. Dim the row in place rather than
    /// rebuilding the list, which would scroll the user back to the top
    /// mid-playback.
    MarkDead(std::rc::Rc<std::collections::HashSet<String>>),
}

#[derive(Debug)]
pub enum TrackRowOutput {
    /// Play the visible list starting here. The *whole* list is enqueued by
    /// `app.rs`, not just this track — MusicKit owns the queue (rule 3).
    Activated(usize),
}

pub struct TrackRowInit {
    pub track: Track,
    pub index: usize,
}

#[relm4::factory(pub)]
impl FactoryComponent for TrackRow {
    type Init = TrackRowInit;
    type Input = TrackRowInput;
    type Output = TrackRowOutput;
    type CommandOutput = ();
    type ParentWidget = gtk::ListBox;

    view! {
        adw::ActionRow {
            // AdwPreferencesRow parses title and subtitle as Pango markup by
            // default, so a track called "Blood, Sweat & 3 Years" fails to
            // render at all and warns. These are plain text — say so, rather
            // than escaping into markup we never wanted.
            set_use_markup: false,
            set_title: &self.track.title,
            set_subtitle: &self.subtitle(),
            set_activatable: true,
            // An unplayable track is shown, not hidden — it is in the library
            // and pretending otherwise is more confusing than dimming it.
            // Watched, because a track can be discovered to be dead later.
            #[watch]
            set_sensitive: self.track.playable(),
            #[watch]
            set_tooltip_text: self.tooltip(),

            add_prefix = &gtk::Image {
                set_pixel_size: 16,
                #[watch]
                set_icon_name: Some(if self.playing {
                    "media-playback-start-symbolic"
                } else if !self.track.playable() {
                    "action-unavailable-symbolic"
                } else {
                    "audio-x-generic-symbolic"
                }),
                #[watch]
                set_css_classes: if self.playing { &["accent"] } else { &["dim-label"] },
            },

            add_suffix = &gtk::Label {
                set_label: &self.track.duration_label(),
                set_margin_start: 6,
                add_css_class: "numeric",
                add_css_class: "dim-label",
            },

            connect_activated[sender, index = self.index] => move |_| {
                sender.output(TrackRowOutput::Activated(index)).ok();
            },
        }
    }

    fn init_model(init: Self::Init, _index: &DynamicIndex, _sender: FactorySender<Self>) -> Self {
        Self {
            track: init.track,
            index: init.index,
            playing: false,
        }
    }

    fn update(&mut self, msg: Self::Input, _sender: FactorySender<Self>) {
        match msg {
            TrackRowInput::NowPlaying(catalog_id) => {
                self.playing = catalog_id.is_some() && catalog_id == self.track.catalog_id;
            }
            TrackRowInput::MarkDead(dead) => {
                if let Some(id) = &self.track.catalog_id
                    && dead.contains(id)
                {
                    self.track.catalog_id = None;
                }
            }
        }
    }
}

impl TrackRow {
    fn subtitle(&self) -> String {
        match (self.track.artist.is_empty(), self.track.album.is_empty()) {
            (false, false) => format!("{} — {}", self.track.artist, self.track.album),
            (false, true) => self.track.artist.clone(),
            (true, false) => self.track.album.clone(),
            (true, true) => String::new(),
        }
    }

    fn tooltip(&self) -> Option<&'static str> {
        (!self.track.playable())
            .then_some("Not available to stream — this track is only in your library")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::music::types::TrackId;

    fn track(title: &str, artist: &str, album: &str, catalog: Option<&str>) -> Track {
        Track {
            id: TrackId("i.test".into()),
            catalog_id: catalog.map(str::to_owned),
            title: title.into(),
            artist: artist.into(),
            album: album.into(),
            duration_ms: 200_000,
            track_number: 1,
            artwork: None,
        }
    }

    fn row(track: Track) -> TrackRow {
        TrackRow {
            track,
            index: 0,
            playing: false,
        }
    }

    #[test]
    fn subtitle_collapses_rather_than_showing_a_dangling_dash() {
        let r = row(track("t", "Aitana", "Superestrella", None));
        assert_eq!(r.subtitle(), "Aitana — Superestrella");
        assert_eq!(row(track("t", "Aitana", "", None)).subtitle(), "Aitana");
        assert_eq!(row(track("t", "", "Album", None)).subtitle(), "Album");
        assert_eq!(row(track("t", "", "", None)).subtitle(), "");
    }

    #[test]
    fn unplayable_tracks_explain_themselves() {
        assert!(row(track("t", "a", "b", None)).tooltip().is_some());
        assert!(row(track("t", "a", "b", Some("123"))).tooltip().is_none());
    }
}
