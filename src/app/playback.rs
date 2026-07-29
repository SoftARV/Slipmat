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
use crate::components::player_view::PlayerViewInput;
use crate::components::queue_view::{QueueEntry, QueueViewInput};
use crate::mpris::MprisState;
use crate::music::types::Artwork;
use crate::player::protocol::{Command, RepeatMode};

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
    ///
    /// **Nothing is sent while the window has focus.** The Now Playing bar is
    /// already on screen saying the same thing, so a banner over it is the app
    /// telling you what you are looking at. Notifications earn their place when
    /// Slipmat is behind something else or has no window at all — which, since
    /// #32, is a state it can be in for hours.
    ///
    /// Checked here rather than in `maybe_notify` because the artwork path
    /// defers this call, and focus can change while a cover is being fetched.
    /// The honest moment to ask is the moment of sending.
    pub(super) fn send_track_notification(&mut self) {
        self.notify_when_art_lands = None;
        let Some(item) = self.player.now_playing.as_ref() else {
            return;
        };

        if relm4::main_application()
            .windows()
            .iter()
            .any(gtk::prelude::GtkWindowExt::is_active)
        {
            tracing::debug!("not notifying: the window has focus");
            return;
        }
        notify::track_changed(
            relm4::main_application().upcast_ref::<gtk::gio::Application>(),
            &item.title,
            &item.artist,
            self.art_path.as_deref(),
        );
    }

    /// What the bar should be showing, which is not always what MusicKit says
    /// is playing *right now*.
    ///
    /// Loading a new queue tears the old one down first, and for a beat
    /// MusicKit reports no current item at all. Rendering that faithfully meant
    /// the bar blanked to its empty state — skeleton, empty sleeve — and then
    /// repopulated, every time you picked a track from a list. Skipping never
    /// showed it, because that moves within a queue already loaded and never
    /// passes through nothing.
    ///
    /// So: while a queue is loaded, keep showing the last track we knew about.
    /// The bar only empties when there is genuinely nothing loaded, which is
    /// also what makes a stopped player keep its last track on screen rather
    /// than wiping itself.
    fn showing(&self) -> Option<&crate::player::protocol::Item> {
        self.player
            .now_playing
            .as_ref()
            // A queue loaded but never started has no *now playing item* —
            // MusicKit only sets one when something begins. That is exactly the
            // state a restored session is in, and rendering it faithfully left
            // the bar empty next to a full queue. The queue's own current entry
            // is the honest answer to "what is this player on".
            .or_else(|| self.player.queue.get(self.player.queue_position))
            .or(self
                .last_item
                .as_ref()
                .filter(|_| !self.player.queue.is_empty()))
    }

    /// Flatten `PlayerState` into what the bar renders, and push it down.
    ///
    /// Called after every event that could change it *and* on each tick, since
    /// the interpolated position moves without any event arriving.
    pub(super) fn push_snapshot(&self) {
        let item = self.showing();
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
            // The same breakpoint the header reads. The bar has less room than
            // the header does, so it stands three controls down rather than
            // one — and each of them is in the drawer.
            narrow: self.narrow_header,
            repeat,
            // Ours, not the sidecar's: MusicKit is told the volume and does not
            // report one back, so `self.volume` is the only record of it.
            volume: self.volume,
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
            // Anything loaded counts, playing or not: a restored queue is
            // paused by design, and greying the transport out would mean you
            // could not press play on it.
            active: item.is_some(),
        };
        // Both shapes of the same player get the same snapshot. Deriving the
        // drawer's state separately is how two views of one thing come to
        // disagree.
        // Shuffle and repeat live in the queue's header, so it needs them too —
        // from the same snapshot as the other two, for the same reason.
        self.queue_view.emit(QueueViewInput::SetModes {
            shuffle: snap.shuffle,
            repeat: snap.repeat,
        });
        self.player_view
            .emit(PlayerViewInput::Sync(Box::new(snap.clone())));
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
                .enumerate()
                .map(|(at, item)| QueueEntry {
                    at,
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

    /// Set the volume, from wherever the request came from.
    ///
    /// The clamp is the whole reason this is shared rather than inlined: the
    /// keyboard steps arrive as `self.volume ± VOLUME_STEP` and will walk off
    /// both ends of the range, and MusicKit takes a `volume` outside 0.0–1.0
    /// without complaining about it.
    pub(super) fn set_volume(&mut self, volume: f64) {
        let volume = volume.clamp(0.0, 1.0);
        if (volume - self.volume).abs() < f64::EPSILON {
            return;
        }
        self.volume = volume;
        self.send(Command::SetVolume { volume });
        self.push_snapshot();
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
        // The same resolved item the bar renders, so the cover does not blank
        // on its own while a queue reloads — see `showing`.
        let template = self.showing().and_then(|i| i.artwork_template.clone());

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
                    // Scaled here, not on the GTK thread (rule 8), and carried
                    // in the same message: the cover and the backdrop taken
                    // from it must never be applied a frame apart.
                    let backdrop = path.as_deref().and_then(artwork::backdrop);
                    CommandMsg::Artwork { path, backdrop }
                });
                true
            }
            None => {
                self.art_path = None;
                self.now_playing.emit(NowPlayingInput::ArtworkReady(None));
                self.player_view.emit(PlayerViewInput::Artwork(None));
                false
            }
        }
    }
}

/// How far into a track "Previous" restarts it rather than going back one.
///
/// Three seconds is the convention every mainstream player follows, and it is
/// a convention rather than a guess: past it, a listener pressing Previous
/// almost always means "play that again", and before it they mean "I overshot".
const RESTART_BEFORE_MS: u64 = 3_000;

/// What a press of Previous should actually do.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Previous {
    /// Back to the start of what is playing.
    Restart,
    /// Back one track in the queue.
    Track,
}

impl AppModel {
    /// Handle a press of Previous, from wherever it came — the bar, the drawer,
    /// the shell, or a media key. All four arrive as one message, so the rule
    /// lives here once.
    ///
    /// **First press restarts, second goes back.** The convention every
    /// mainstream player follows, and Slipmat did not: it jumped to the
    /// previous track immediately, so overshooting a song you were enjoying
    /// meant seeking back by hand.
    ///
    /// No timer, and no "was that a double press" state to keep. Restarting
    /// puts the position at zero, which is already the case that means *go back
    /// one* — so the second press falls into it by itself.
    pub(super) fn go_previous(&self) {
        match previous_means(self.player.interpolated_position_ms()) {
            Previous::Restart => {
                self.send(Command::Seek { position_ms: 0 });
                // Same reasoning as `AppMsg::Seek`: a discontinuous move has to
                // be announced, or controllers keep extrapolating from the old
                // position and their progress bars drift.
                self.mpris.seeked(0);
            }
            Previous::Track => self.send(Command::Previous),
        }
    }
}

/// Which of the two a press means, from how far into the track it lands.
///
/// Pure and separate from the reducer so the rule can be tested without a
/// player: the whole feature is this one comparison, and it is the sort of
/// thing that is easy to get backwards.
pub(super) fn previous_means(position_ms: u64) -> Previous {
    if position_ms >= RESTART_BEFORE_MS {
        Previous::Restart
    } else {
        Previous::Track
    }
}

#[cfg(test)]
mod previous_tests {
    use super::*;

    #[test]
    fn a_press_early_in_a_track_goes_back_one() {
        // The overshoot case: you meant the track before this one.
        assert_eq!(previous_means(0), Previous::Track);
        assert_eq!(previous_means(2_999), Previous::Track);
    }

    #[test]
    fn a_press_later_in_a_track_restarts_it() {
        // And pressing again immediately lands in the case above, which is how
        // the second press reaches the previous track — no timer, no state.
        assert_eq!(previous_means(3_000), Previous::Restart);
        assert_eq!(previous_means(120_000), Previous::Restart);
    }
}
