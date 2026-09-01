// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The line to the daemon.
//!
//! **One connection, subscribed, treated as a stream.** Requests go out and
//! events come back; the answer to a request arrives as an event like any
//! other, and nothing is paired up. That is not a simplification — a subscribed
//! connection has broadcasts queued in it, so "write a request, read the next
//! line" returns whichever event happened to be waiting rather than the answer.
//! Learned by watching a probe report that nothing worked while the daemon was
//! perfectly fine.

use std::path::PathBuf;

use slipmat_core::ipc::{Event, Request};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

/// Sends requests. Cheap to clone; the writer task owns the socket.
#[derive(Clone)]
pub struct Link(mpsc::UnboundedSender<Request>);

impl Link {
    pub fn send(&self, request: Request) {
        // A closed channel means the connection is gone, which the event stream
        // reports on its own. Nothing useful to do here.
        let _ = self.0.send(request);
    }

    #[cfg(test)]
    pub(crate) fn channel() -> (Self, mpsc::UnboundedReceiver<Request>) {
        let (send, receive) = mpsc::unbounded_channel();
        (Self(send), receive)
    }
}

/// What arrives from the daemon.
pub enum Incoming {
    Event(Box<Event>),
    /// The connection ended, with why. Always last.
    Lost(String),
}

/// Where `slipmatd` is: beside this binary, which is true of an install and of
/// `cargo build` alike.
fn daemon_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("slipmatd")))
        .unwrap_or_else(|| PathBuf::from("slipmatd"))
}

/// Connect — starting a daemon if none is listening — and stream its events.
pub async fn connect() -> anyhow::Result<(Link, mpsc::UnboundedReceiver<Incoming>)> {
    // Blocking, and it may start a process: off the async thread.
    let stream =
        tokio::task::spawn_blocking(|| slipmat_core::ipc::connect_or_spawn(&daemon_path()))
            .await??;
    stream.set_nonblocking(true)?;
    let (read, mut write) = tokio::net::UnixStream::from_std(stream)?.into_split();

    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Request>();
    tokio::spawn(async move {
        while let Some(request) = out_rx.recv().await {
            let Ok(mut line) = serde_json::to_vec(&request) else {
                continue;
            };
            line.push(b'\n');
            if write.write_all(&line).await.is_err() {
                break;
            }
        }
    });

    let (in_tx, in_rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut lines = BufReader::new(read).lines();
        loop {
            let message = match lines.next_line().await {
                Ok(Some(line)) => match serde_json::from_str::<Event>(&line) {
                    Ok(event) => Incoming::Event(Box::new(event)),
                    // Loudly, per rule 4: this build and the daemon's `ipc.rs`
                    // disagree, which a retry will not fix.
                    Err(err) => Incoming::Lost(format!("cannot read the daemon: {err}")),
                },
                Ok(None) => Incoming::Lost("the daemon closed the connection".into()),
                Err(err) => Incoming::Lost(err.to_string()),
            };
            let last = matches!(message, Incoming::Lost(_));
            if in_tx.send(message).is_err() || last {
                return;
            }
        }
    });

    let link = Link(out_tx);
    // Subscribed first, then asked for everything: `stage` and `queue` are only
    // broadcast when they change, so a client attaching to a daemon that has
    // been ready for an hour has to ask or it draws a startup screen for ever.
    link.send(Request::Subscribe);
    link.send(Request::Stage);
    link.send(Request::Snapshot);
    link.send(Request::Queue);
    Ok((link, in_rx))
}
