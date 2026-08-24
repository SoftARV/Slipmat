// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The daemon's half of MPRIS: a `LocalSet` to spawn on, and what each command
//! from the bus means here.
//!
//! `slipmat_core::mpris` exports the interface and reports what a controller
//! asked for. This is the part that knows the answer is a sidecar command
//! rather than a message to a GTK model.

use std::rc::Rc;

use slipmat_core::mpris::{Capabilities, Mpris, MprisCommand, MprisState};
use slipmat_core::player::protocol::Command;

use crate::serve::Daemon;

/// Export the player, driven from this process's own event loop.
pub fn start(daemon: &Rc<Daemon>) -> Mpris {
    let sink = daemon.clone();
    Mpris::start(
        // `spawn_local` returns a handle the task does not need; dropping it
        // does not cancel anything.
        Rc::new(|fut| {
            tokio::task::spawn_local(fut);
        }),
        Rc::new(move |cmd| on_command(&sink, cmd)),
        Capabilities::headless(),
    )
}

fn on_command(daemon: &Rc<Daemon>, cmd: MprisCommand) {
    match cmd {
        MprisCommand::Play => daemon.send(Command::Play),
        MprisCommand::Pause => daemon.send(Command::Pause),
        MprisCommand::PlayPause => daemon.send(Command::PlayPause),
        MprisCommand::Next => daemon.send(Command::Next),
        MprisCommand::Previous => daemon.send(Command::Previous),
        MprisCommand::Seek(position_ms) => daemon.send(Command::Seek { position_ms }),
        MprisCommand::SetShuffle(shuffle) => daemon.send(Command::SetShuffle { shuffle }),
        MprisCommand::SetRepeat(mode) => daemon.send(Command::SetRepeat { mode }),
        MprisCommand::SetVolume(volume) => {
            daemon.model.borrow_mut().volume = volume;
            daemon.send(Command::SetVolume { volume });
        }
        // **There is no window to raise.** A daemon answering `CanRaise: true`
        // would put a button in every bar that does nothing; the frontends own
        // that, and one of them may not have a window either.
        MprisCommand::Raise => tracing::debug!("MPRIS Raise: the daemon has no window"),
        // Quitting the daemon takes playback from every client attached to it,
        // which is not what a media key means.
        MprisCommand::Quit => tracing::info!("MPRIS Quit ignored: clients would lose the player"),
    }
}

/// What the bus should be showing, from the daemon's mirror.
pub fn state(daemon: &Daemon) -> MprisState {
    let model = daemon.model.borrow();
    let item = model.player.now_playing.as_ref();
    MprisState {
        track_id: item.and_then(|i| i.catalog_id.clone().or_else(|| i.id.clone())),
        title: item.map(|i| i.title.clone()).unwrap_or_default(),
        artist: item.map(|i| i.artist.clone()).unwrap_or_default(),
        album: item.map(|i| i.album.clone()).unwrap_or_default(),
        track_number: item.map(|i| i.track_number).unwrap_or_default(),
        // No fetcher here yet, so the bar shows no cover. A `file://` path is
        // the only thing MPRIS accepts, and inventing one would be worse.
        art_path: None,
        length_ms: model.player.duration_ms,
        position_ms: model.player.interpolated_position_ms(),
        playing: model.player.state.is_playing(),
        stopped: model.player.queue.is_empty(),
        can_next: model.player.has_next(),
        can_previous: model.player.has_previous(),
        volume: model.volume,
        shuffle: model.player.shuffle,
        repeat: model.player.repeat,
    }
}
