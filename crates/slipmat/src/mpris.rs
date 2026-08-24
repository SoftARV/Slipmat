// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The GTK client's half of MPRIS: relm4's executor, and what each command
//! means here.
//!
//! [`slipmat_core::mpris`] exports the interface and says what a controller
//! asked for. It deliberately does not know that this app has an `AppMsg`, or
//! that relm4 owns the main loop — a daemon will answer the same commands by
//! writing straight to the sidecar.

use std::rc::Rc;

use relm4::ComponentSender;
pub use slipmat_core::mpris::{Mpris, MprisCommand, MprisState};

use crate::app::{AppModel, AppMsg};

/// Export the player, driving it from this app's message loop.
pub fn start(sender: ComponentSender<AppModel>) -> Mpris {
    Mpris::start(
        // `spawn_local` hands back a JoinHandle; the task is fire-and-forget
        // here, and dropping the handle does not cancel it.
        Rc::new(|fut| {
            relm4::spawn_local(fut);
        }),
        Rc::new(move |cmd| {
            // Quit goes straight to the shared exit rather than through the
            // model: it runs on the main thread like every other route out, and
            // quitting has nothing to ask first.
            if cmd == MprisCommand::Quit {
                crate::notify::quit_cleanly();
                return;
            }
            sender.input(into_msg(cmd));
        }),
    )
}

fn into_msg(cmd: MprisCommand) -> AppMsg {
    match cmd {
        MprisCommand::Play => AppMsg::Play,
        MprisCommand::Pause | MprisCommand::Quit => AppMsg::Pause,
        MprisCommand::PlayPause => AppMsg::PlayPause,
        MprisCommand::Next => AppMsg::Next,
        MprisCommand::Previous => AppMsg::Previous,
        MprisCommand::Seek(ms) => AppMsg::Seek(ms),
        MprisCommand::SetShuffle(on) => AppMsg::SetShuffle(on),
        MprisCommand::SetVolume(v) => AppMsg::SetVolume(v),
        MprisCommand::Raise => AppMsg::Raise,
        MprisCommand::SetRepeat(mode) => AppMsg::SetRepeat(mode),
    }
}
