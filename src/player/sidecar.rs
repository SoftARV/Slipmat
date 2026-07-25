// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Locate, spawn and supervise the Electron child.
//!
//! This is the piece Dockyard and Pitwall never needed. Both talked to
//! something that already existed (a socket, an API); Tonearm *owns a process*.
//! So the module's job is lifetime: start it, read its stdout forever, notice
//! when it dies, and let `app.rs` restart it (CLAUDE.md rule 6).
//!
//! ## Ownership note (Rust, for a React brain)
//!
//! The child's stdin and stdout are two halves that need to live in different
//! places: stdout is read by a background task that runs for as long as the
//! process does, while stdin is written to from `update()` in response to
//! clicks. We can't hand both to one owner without making every send `await` a
//! lock, so we split them:
//!
//!   - stdout is *moved* into a spawned tokio task that owns it outright;
//!   - stdin is *moved* into a second task fed by an unbounded channel.
//!
//! `Handle` then holds only a channel sender, which is cheap to clone and
//! `Send` — that's why the UI can keep one in the model and clone it into
//! closures without a `Mutex` anywhere. The channel *is* the synchronisation.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, anyhow};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as TokioCommand;
use tokio::sync::mpsc;

use super::protocol::{Command, Event};

/// What the reader task pushes up to `app.rs`.
#[derive(Debug)]
pub enum Incoming {
    Event(Event),
    /// A line we couldn't parse. Kept as a distinct case rather than silently
    /// dropped — it usually means preload.js and protocol.rs drifted.
    Unparsed(String),
    /// The process exited. Always the last message on the channel.
    Died(String),
}

/// A cheap, cloneable handle for sending commands to the sidecar.
#[derive(Debug, Clone)]
pub struct Handle {
    tx: mpsc::UnboundedSender<Command>,
}

impl Handle {
    /// Fire-and-forget. A closed channel means the child already died; the
    /// `Died` message is already on its way, so dropping here is correct
    /// rather than an error the UI has to handle twice.
    pub fn send(&self, cmd: Command) {
        if self.tx.send(cmd).is_err() {
            tracing::debug!("sidecar command dropped: channel closed (child is gone)");
        }
    }
}

/// Find the sidecar directory: an explicit override, the installed location,
/// then the dev tree. Matches the search order documented in the Makefile.
pub fn locate() -> Result<PathBuf> {
    let mut tried = Vec::new();

    if let Ok(dir) = std::env::var("TONEARM_SIDECAR") {
        let p = PathBuf::from(dir);
        if p.join("main.js").is_file() {
            return Ok(p);
        }
        tried.push(p);
    }

    if let Some(data) = dirs_data_home() {
        let p = data.join("tonearm/sidecar");
        if p.join("main.js").is_file() {
            return Ok(p);
        }
        tried.push(p);
    }

    let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("sidecar");
    if dev.join("main.js").is_file() {
        return Ok(dev);
    }
    tried.push(dev);

    Err(anyhow!(
        "sidecar not found (looked in: {}). Run `make sidecar` to install it.",
        tried
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// `$XDG_DATA_HOME`, else `~/.local/share`. Small enough not to warrant a crate.
fn dirs_data_home() -> Option<PathBuf> {
    if let Ok(x) = std::env::var("XDG_DATA_HOME")
        && !x.is_empty()
    {
        return Some(PathBuf::from(x));
    }
    std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join(".local/share"))
}

/// The Electron binary inside the sidecar's `node_modules`.
///
/// We deliberately use `electron/dist/electron` rather than `.bin/electron`:
/// the latter is a Node shim, which adds a process between us and the child and
/// can print to stdout — and stdout is protocol (CLAUDE.md).
fn electron_binary(sidecar: &Path) -> Result<PathBuf> {
    let direct = sidecar.join("node_modules/electron/dist/electron");
    if direct.is_file() {
        return Ok(direct);
    }
    Err(anyhow!(
        "Electron not installed at {}. Run `make sidecar`.",
        direct.display()
    ))
}

/// Start the sidecar. Returns a handle for commands and a receiver of events.
///
/// The receiver ends with exactly one `Incoming::Died`, which is `app.rs`'s cue
/// to restart with backoff.
pub fn spawn() -> Result<(Handle, mpsc::UnboundedReceiver<Incoming>)> {
    let dir = locate()?;
    let bin = electron_binary(&dir)?;
    tracing::info!(sidecar = %dir.display(), "spawning");

    let mut child = TokioCommand::new(&bin)
        .arg(".")
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Chromium is loud. Let it inherit stderr so its noise shows up in the
        // terminal next to our tracing output, and never on stdout.
        .stderr(Stdio::inherit())
        // Electron re-executes itself for its zygote/GPU processes; killing the
        // parent on drop keeps a crashed run from leaving Chromium behind.
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("failed to start {}", bin.display()))?;

    let stdout = child.stdout.take().context("child stdout was not piped")?;
    let mut stdin = child.stdin.take().context("child stdin was not piped")?;

    let (evt_tx, evt_rx) = mpsc::unbounded_channel();
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<Command>();

    // Writer task — owns stdin.
    tokio::spawn(async move {
        while let Some(cmd) = cmd_rx.recv().await {
            let mut line = match serde_json::to_vec(&cmd) {
                Ok(v) => v,
                Err(err) => {
                    tracing::error!(?err, "failed to serialise command");
                    continue;
                }
            };
            line.push(b'\n');
            if let Err(err) = stdin.write_all(&line).await {
                tracing::warn!(?err, "sidecar stdin closed");
                break;
            }
            if let Err(err) = stdin.flush().await {
                tracing::warn!(?err, "sidecar stdin flush failed");
                break;
            }
        }
    });

    // Reader task — owns stdout, and owns waiting on the child so the exit
    // status is reported on the same channel, strictly after the last event.
    let died_tx = evt_tx.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let msg = match serde_json::from_str::<Event>(&line) {
                        Ok(ev) => Incoming::Event(ev),
                        Err(err) => {
                            tracing::warn!(?err, %line, "unparsed sidecar line");
                            Incoming::Unparsed(line)
                        }
                    };
                    if evt_tx.send(msg).is_err() {
                        break; // app is gone
                    }
                }
                Ok(None) => break, // EOF: the child closed stdout
                Err(err) => {
                    tracing::warn!(?err, "sidecar stdout read failed");
                    break;
                }
            }
        }

        let reason = match child.wait().await {
            Ok(status) => format!("sidecar exited: {status}"),
            Err(err) => format!("sidecar wait failed: {err}"),
        };
        let _ = died_tx.send(Incoming::Died(reason));
    });

    Ok((Handle { tx: cmd_tx }, evt_rx))
}

/// Backoff for supervised restarts (rule 6): 1s, 2s, 4s, 8s, capped at 30s.
/// Capped rather than unbounded because a laptop that wakes from suspend
/// should recover promptly, not sit in a 20-minute backoff.
pub fn restart_delay(attempt: u32) -> std::time::Duration {
    let secs = 1u64 << attempt.min(5);
    std::time::Duration::from_secs(secs.min(30))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_then_caps() {
        assert_eq!(restart_delay(0).as_secs(), 1);
        assert_eq!(restart_delay(1).as_secs(), 2);
        assert_eq!(restart_delay(3).as_secs(), 8);
        assert_eq!(restart_delay(5).as_secs(), 30);
        assert_eq!(restart_delay(99).as_secs(), 30, "must stay bounded");
    }

    #[test]
    fn a_missing_electron_names_the_fix() {
        // CLAUDE.md: errors name the fix. "Electron not installed" on its own
        // sends you to a search engine; the command to run does not.
        // (Deliberately not testing `locate()` via env vars — `set_var` is
        // process-global and tests run in parallel threads.)
        let err = electron_binary(Path::new("/nonexistent/tonearm")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("make sidecar"), "unhelpful error: {msg}");
        assert!(
            msg.contains("/nonexistent/tonearm"),
            "should say where it looked: {msg}"
        );
    }
}
