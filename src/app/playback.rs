// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Pushing the mirrored player state outwards — to the Now Playing bar, to
//! MPRIS, to the notification, and to the cover on disk.
//!
//! Everything here is downstream of `PlayerState`, which is a projection of the
//! sidecar's own state (rule 3). Nothing in this file decides anything about
//! playback; it decides how playback is *shown*.

use relm4::ComponentSender;

use relm4::gtk;
use relm4::gtk::prelude::*;
use relm4::prelude::*;

use super::{ART_SIZE, AppModel, AppMsg, CommandMsg, TICK_MS, artwork, notify};
use crate::components::now_playing::{NowPlayingInput, Repeat, Snapshot};
use crate::components::queue_view::{QueueEntry, QueueViewInput};
use crate::mpris::MprisState;
use crate::music::types::Artwork;
use crate::player::protocol::RepeatMode;

impl AppModel {
    /// Tell the rows which one is playing, so the list shows a play marker.
    /// Notify about a new track, if the user asked for that.
    ///
    /// Keyed on the track id rather than on "metadata changed": a queue echo,
    /// a seek or an artwork arrival all count as metadata changes, and none of
    /// them is a new song. Without this you get several notifications per
    /// track.
    pub(super) fn maybe_notify(&mut self, artwork_in_flight: bool) {
        if !self.settings.notify_track_change {
            // Still track what is playing, so switching the preference on
            // mid-song does not immediately fire for the song already playing.
            self.notified_for = self.playing_catalog_id();
            return;
        }

        let current = self.playing_catalog_id();
        if current.is_none() || current == self.notified_for {
            return;
        }
        self.notified_for = current.clone();

        if artwork_in_flight {
            // `art_path` still holds the PREVIOUS track's cover: the fetch is
            // async and has not landed yet. Notifying now shows the wrong
            // album. Wait for CommandMsg::Artwork, which always arrives — with
            // None if the fetch failed.
            self.notify_when_art_lands = current;
            return;
        }
        self.send_track_notification();
    }

    /// Post the notification for whatever is playing now.
    pub(super) fn send_track_notification(&mut self) {
        self.notify_when_art_lands = None;
        let Some(item) = self.player.now_playing.as_ref() else {
            return;
        };
        notify::track_changed(
            relm4::main_application().upcast_ref::<gtk::gio::Application>(),
            &item.title,
            &item.artist,
            self.art_path.as_deref(),
        );
    }

    /// Flatten `PlayerState` into what the bar renders, and push it down.
    ///
    /// Called after every event that could change it *and* on each tick, since
    /// the interpolated position moves without any event arriving.
    pub(super) fn push_snapshot(&self) {
        let item = self.player.now_playing.as_ref();
        // Protocol type in, ours out — `components/` never sees `RepeatMode`
        // (rule 9). The mapping lives here because this is the boundary.
        let repeat = match self.player.repeat {
            RepeatMode::None => Repeat::Off,
            RepeatMode::All => Repeat::All,
            RepeatMode::One => Repeat::One,
        };
        let snap = Snapshot {
            shuffle: self.player.shuffle,
            queue_open: self.show_queue,
            repeat,
            title: item.map(|i| i.title.clone()).unwrap_or_default(),
            artist: item.map(|i| i.artist.clone()).unwrap_or_default(),
            album: item.map(|i| i.album.clone()).unwrap_or_default(),
            // **Raw**, not interpolated. The bar carries the position forward
            // itself between snapshots; sending an already-extrapolated value
            // meant two extrapolators stacked on one clock, so the slider ran
            // ahead and then lurched backwards every time a real position event
            // reset the truth underneath it.
            position_ms: self.player.position_ms,
            duration_ms: self.player.duration_ms,
            playing: self.player.state.is_playing(),
            busy: self.player.state.is_busy(),
            has_next: self.player.has_next(),
            has_previous: self.player.has_previous(),
            active: item.is_some(),
        };
        self.now_playing.emit(NowPlayingInput::Sync(Box::new(snap)));

        // The queue dialog reads MusicKit's queue, not our library list. The
        // playing track is identified by id rather than position: after a
        // removal the positions shift, and marking by index put the indicator
        // on whichever track slid into the old slot.
        let queue_id = |item: &crate::player::protocol::Item| {
            item.catalog_id
                .clone()
                .or_else(|| item.id.clone())
                .unwrap_or_default()
        };
        self.queue_view.emit(QueueViewInput::Sync {
            entries: self
                .player
                .queue
                .iter()
                .map(|item| QueueEntry {
                    id: queue_id(item),
                    title: item.title.clone(),
                    artist: item.artist.clone(),
                    duration_ms: item.duration_ms,
                })
                .collect(),
            playing: item.map(queue_id),
        });

        // Same state, second consumer. MPRIS diffs internally, so calling this
        // on every tick costs one property write and no bus traffic.
        self.mpris.update(MprisState {
            track_id: item.and_then(|i| i.catalog_id.clone().or_else(|| i.id.clone())),
            title: item.map(|i| i.title.clone()).unwrap_or_default(),
            artist: item.map(|i| i.artist.clone()).unwrap_or_default(),
            album: item.map(|i| i.album.clone()).unwrap_or_default(),
            track_number: item.map(|i| i.track_number).unwrap_or(0),
            art_path: self.art_path.clone(),
            length_ms: self.player.duration_ms,
            position_ms: self.player.interpolated_position_ms(),
            playing: self.player.state.is_playing(),
            stopped: item.is_none(),
            can_next: self.player.has_next(),
            can_previous: self.player.has_previous(),
            volume: self.volume,
        });
    }

    /// Start the repaint timer while playing, drop it otherwise.
    ///
    /// `glib::SourceId` must be removed exactly once — holding it in an
    /// `Option` and `take()`ing is what makes that safe, since removing an
    /// already-removed source aborts.
    pub(super) fn sync_tick(&mut self, sender: &ComponentSender<Self>) {
        let want = self.player.state.is_playing();
        match (want, self.tick.is_some()) {
            (true, false) => {
                let sender = sender.clone();
                self.tick = Some(gtk::glib::timeout_add_local(
                    std::time::Duration::from_millis(TICK_MS as u64),
                    move || {
                        sender.input(AppMsg::Tick);
                        gtk::glib::ControlFlow::Continue
                    },
                ));
            }
            (false, true) => {
                if let Some(id) = self.tick.take() {
                    id.remove();
                }
            }
            _ => {}
        }
    }

    /// Fetch cover art for the current track, at most once per template.
    /// Returns whether a fetch is now in flight, so the caller knows that
    /// `art_path` is stale until `CommandMsg::Artwork` arrives.
    pub(super) fn sync_artwork(&mut self, sender: &ComponentSender<Self>) -> bool {
        let template = self
            .player
            .now_playing
            .as_ref()
            .and_then(|i| i.artwork_template.clone());

        if template == self.art_for {
            // Same cover as the last track — usually the next song on the same
            // album. `art_path` is already correct.
            return false;
        }
        self.art_for = template.clone();

        match template {
            Some(t) => {
                let art = Artwork::new(t);
                sender.oneshot_command(async move {
                    let path = artwork::fetch(art, ART_SIZE).await.ok();
                    // Read here, not on the GTK thread (rule 8), and carried in
                    // the same message: the cover and the colour taken from it
                    // must never be applied a frame apart.
                    let tint = path.as_deref().and_then(artwork::tint);
                    CommandMsg::Artwork { path, tint }
                });
                true
            }
            None => {
                self.art_path = None;
                self.now_playing.emit(NowPlayingInput::ArtworkReady(None));
                false
            }
        }
    }
}
