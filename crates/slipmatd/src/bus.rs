// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The daemon's half of MPRIS: a `LocalSet` to spawn on, and what each command
//! from the bus means here.
//!
//! `slipmat_core::mpris` exports the interface and reports what a controller
//! asked for. This routes playback through the same transport path as every
//! connected client.

use std::rc::Rc;

use slipmat_core::ipc::Transport;
use slipmat_core::mpris::{Capabilities, Mpris, MprisCommand, MprisState};

use crate::serve::Daemon;
use crate::state::Model;

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
    if let Some(transport) = mpris_transport(cmd) {
        crate::serve::route_transport(daemon, transport);
        return;
    }
    match cmd {
        // **There is no window to raise.** A daemon answering `CanRaise: true`
        // would put a button in every bar that does nothing; the frontends own
        // that, and one of them may not have a window either.
        MprisCommand::Raise => tracing::debug!("MPRIS Raise: the daemon has no window"),
        // Quitting the daemon takes playback from every client attached to it,
        // which is not what a media key means.
        MprisCommand::Quit => tracing::info!("MPRIS Quit ignored: clients would lose the player"),
        _ => {}
    }
}

fn mpris_transport(cmd: MprisCommand) -> Option<Transport> {
    Some(match cmd {
        MprisCommand::Play => Transport::Play,
        MprisCommand::Pause => Transport::Pause,
        MprisCommand::PlayPause => Transport::PlayPause,
        MprisCommand::Next => Transport::Next,
        MprisCommand::Previous => Transport::Previous,
        MprisCommand::Seek(position_ms) => Transport::Seek { position_ms },
        MprisCommand::SetShuffle(shuffle) => Transport::SetShuffle { shuffle },
        MprisCommand::SetRepeat(mode) => Transport::SetRepeat { mode },
        MprisCommand::SetVolume(volume) => Transport::SetVolume { volume },
        MprisCommand::Raise | MprisCommand::Quit => return None,
    })
}

/// What the bus should be showing, from the daemon's mirror.
pub fn state(daemon: &Daemon) -> MprisState {
    let model = daemon.model.borrow();
    state_from_model(&model)
}

fn state_from_model(model: &Model) -> MprisState {
    let item = model.player.now_playing.as_ref();
    MprisState {
        track_id: item.and_then(|i| i.catalog_id.clone().or_else(|| i.id.clone())),
        current_item: item.cloned(),
        queue: model.player.queue.clone(),
        queue_position: model.player.queue_position,
        title: item.map(|i| i.title.clone()).unwrap_or_default(),
        artist: item.map(|i| i.artist.clone()).unwrap_or_default(),
        album: item.map(|i| i.album.clone()).unwrap_or_default(),
        track_number: item.map(|i| i.track_number).unwrap_or_default(),
        art_path: model.art_path.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use slipmat_core::player::protocol::Item;

    #[test]
    fn mpris_volume_uses_the_shared_stream_transport() {
        assert_eq!(
            mpris_transport(MprisCommand::SetVolume(0.5)),
            Some(Transport::SetVolume { volume: 0.5 })
        );
    }

    #[test]
    fn mpris_state_includes_exact_queue_occurrence_facts() {
        let current = Item {
            occurrence_id: "run:2".into(),
            id: Some("song-a".into()),
            title: "Duplicate".into(),
            ..Default::default()
        };
        let mut model = Model::new();
        model.player.queue = vec![
            Item {
                occurrence_id: "run:1".into(),
                ..current.clone()
            },
            current.clone(),
        ];
        model.player.queue_position = 1;
        model.player.now_playing = Some(current.clone());

        let state = state_from_model(&model);

        assert_eq!(state.queue.len(), 2);
        assert_eq!(state.queue_position, 1);
        assert_eq!(
            state
                .current_item
                .as_ref()
                .map(|item| item.occurrence_id.as_str()),
            Some("run:2")
        );
    }
}
