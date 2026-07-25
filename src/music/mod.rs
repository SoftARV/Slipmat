// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! `api.music.apple.com` — everything the app *shows*, as opposed to plays.
//!
//! The split is the whole architecture in one line: metadata comes down this
//! path as JSON and is rendered with native widgets; only the audio goes
//! through the sidecar.
//!
//! M1 is the playback handshake, so nothing here is wired to the UI yet — the
//! types and the client are proven by their unit tests until M5 calls them.
//! Remove this allow the moment `app.rs` makes its first request.
#![allow(dead_code)]

pub mod client;
pub mod types;
