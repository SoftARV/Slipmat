// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Playback: the sidecar contract, the child's lifetime, and our mirror of its
//! state. Nothing here draws anything.

pub mod protocol;
pub mod sidecar;
pub mod state;

pub use sidecar::Incoming;
pub use state::PlayerState;
