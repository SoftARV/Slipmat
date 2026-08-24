// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Talking to `slipmatd`.
//!
//! **The client does not own a sidecar any more.** One process holds the
//! Chromium — the profile lock says so — and this app is one of its clients.
//! What used to be a command down a pipe is now a request over a socket, and
//! what used to be a MusicKit event is now the daemon's own snapshot.
//!
//! The daemon is started here if it is not running: Slipmat is an app somebody
//! opens, not a service they enable.

use std::path::PathBuf;

use slipmat_core::ipc::{Event, Request};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

/// A cheap, cloneable handle for sending requests.
///
/// Sends are fire-and-forget into a channel a writer task drains, for the same
/// reason the sidecar's handle worked that way: `update()` must not await, and
/// a socket write can block.
#[derive(Debug, Clone)]
pub struct Handle(mpsc::UnboundedSender<Request>);

impl Handle {
    pub fn send(&self, request: Request) {
        if self.0.send(request).is_err() {
            tracing::debug!("dropped: no daemon connection");
        }
    }
}

/// What the connection reports upward.
#[derive(Debug)]
pub enum Incoming {
    Connected(Handle),
    /// Boxed: an `Event::Rows` can carry hundreds of entries, and an enum is
    /// as large as its largest variant wherever it is passed.
    Event(Box<Event>),
    /// A line we could not read. Kept distinct rather than dropped — it means
    /// `ipc.rs` and this build disagree, which is a version skew rather than a
    /// transient error.
    Unparsed(String),
    /// The connection ended. Always the last message.
    Lost(String),
}

/// Where the daemon binary is.
///
/// Beside this one in an install, and beside it in a dev tree too — `cargo
/// build` puts both in the same directory, so the search that works for a
/// package works for `cargo run` without a special case.
fn daemon_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("slipmatd")))
        .unwrap_or_else(|| PathBuf::from("slipmatd"))
}

/// Connect, subscribe, and stream events until the connection ends.
///
/// Blocking `connect_or_spawn` runs on a worker thread: it may start a process
/// and wait for a socket, and neither belongs on the GTK thread (rule 8).
pub async fn connect(out: mpsc::UnboundedSender<Incoming>) {
    let stream =
        match tokio::task::spawn_blocking(|| slipmat_core::ipc::connect_or_spawn(&daemon_path()))
            .await
        {
            Ok(Ok(stream)) => stream,
            Ok(Err(err)) => {
                let _ = out.send(Incoming::Lost(err.to_string()));
                return;
            }
            Err(err) => {
                let _ = out.send(Incoming::Lost(err.to_string()));
                return;
            }
        };

    let stream = match stream
        .set_nonblocking(true)
        .and_then(|()| tokio::net::UnixStream::from_std(stream))
    {
        Ok(stream) => stream,
        Err(err) => {
            let _ = out.send(Incoming::Lost(err.to_string()));
            return;
        }
    };

    let (read, mut write) = stream.into_split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Request>();

    // Writer task, owning the write half.
    tokio::spawn(async move {
        while let Some(request) = rx.recv().await {
            let Ok(mut line) = serde_json::to_vec(&request) else {
                continue;
            };
            line.push(b'\n');
            if write.write_all(&line).await.is_err() {
                break;
            }
        }
    });

    let handle = Handle(tx);
    // Subscribed before the handle goes up, so nothing the model sends can
    // arrive before the events it will be answered by.
    handle.send(Request::Subscribe);
    if out.send(Incoming::Connected(handle)).is_err() {
        return;
    }

    let mut lines = BufReader::new(read).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                let message = match serde_json::from_str::<Event>(&line) {
                    Ok(event) => Incoming::Event(Box::new(event)),
                    Err(_) => Incoming::Unparsed(line),
                };
                if out.send(message).is_err() {
                    return; // the component is gone
                }
            }
            Ok(None) => {
                let _ = out.send(Incoming::Lost("the daemon closed the connection".into()));
                return;
            }
            Err(err) => {
                let _ = out.send(Incoming::Lost(err.to_string()));
                return;
            }
        }
    }
}
