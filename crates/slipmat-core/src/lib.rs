// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Everything a client of Apple Music needs that is not drawing.
//!
//! No toolkit reaches in here — that is the point of the split. A frontend that
//! draws nothing at all still needs all of it: the sidecar contract, the child's
//! lifetime, our mirror of its state, and the catalog client.

pub mod entry;
pub mod ipc;
pub mod library_cache;
pub mod mpris;
pub mod music;
pub mod paths;
pub mod player;
pub mod queue;
pub mod session;
pub mod unplayable;

/// The application id. It must match the `.desktop` file name, the GResource
/// prefix, `RelmApp::new()` and the MPRIS bus name suffix.
pub const APP_ID: &str = "dev.miguelrincon.Slipmat";
