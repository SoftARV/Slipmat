// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Locate, spawn and supervise the Electron child.
//!
//! This is the piece Dockyard and Pitwall never needed. Both talked to
//! something that already existed (a socket, an API); Slipmat *owns a process*.
//! So the module's job is lifetime: start it, read its stdout forever, notice
//! when it dies, and let `app/mod.rs` restart it (CLAUDE.md rule 6).
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

/// What the reader task pushes up to `app/mod.rs`.
#[derive(Debug)]
pub enum Incoming {
    Event(Event),
    /// A line we couldn't parse. Kept as a distinct case rather than silently
    /// dropped — it usually means preload.js and protocol.rs drifted.
    Unparsed(String),
    /// The process exited. Always the last message on the channel.
    Died(String),
}

/// The most commands of one kind Slipmat will send in a second.
///
/// A ceiling, not a budget, and deliberately generous.
///
/// The number has to sit above anything a person can produce and far below what
/// hurts. The upper bound on human input is the pointer's event rate: GTK emits
/// `value-changed` per motion event, so dragging a slider on a 165Hz display
/// can plausibly reach a couple of hundred a second. A ceiling that clipped a
/// real drag would be worse than the bug it guards against — so this is set
/// clear of that, not snug against it.
///
/// The failure it catches is nothing like a drag: a runaway `update()` managed
/// **5,721 dispatches** before the desktop stopped responding (#37), and reached
/// this ceiling in the first fraction of a second.
const MAX_PER_SECOND: u32 = 250;

/// A cheap, cloneable handle for sending commands to the sidecar.
#[derive(Debug, Clone)]
pub struct Handle {
    tx: mpsc::UnboundedSender<Command>,
    /// Per-command-kind rate window, shared by every clone of this handle.
    ///
    /// `Arc<Mutex<_>>` rather than `Rc<RefCell<_>>`: the handle is delivered to
    /// the model through `CommandMsg::Spawned`, which crosses a thread, so it
    /// has to be `Send`. Sends themselves all happen on the GTK thread, so the
    /// lock is uncontended in practice.
    rate: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<&'static str, Window>>>,
    /// The child's pid, which is also its process-group id — see `spawn`.
    ///
    /// Only [`Handle::kill`] uses it, and only when the child has stopped
    /// listening. `None` for a handle with no child behind it.
    pgid: Option<u32>,
}

#[derive(Debug)]
struct Window {
    started: std::time::Instant,
    sent: u32,
    /// So a storm says so once rather than once per dropped command — the
    /// logging *is* the amplifier this exists to stop.
    warned: bool,
}

impl Handle {
    /// Kill the sidecar and everything it spawned, immediately.
    ///
    /// **Dropping the handle is not enough for a sidecar that has stopped
    /// listening.** A drop closes the child's stdin and waits for `main.js` to
    /// notice, which is exactly what a healthy child does and exactly what a
    /// wedged one cannot — `kill_on_drop` lives on the `Child`, and the `Child`
    /// is owned by the reader task, which sits blocked on a pipe that will
    /// never reach EOF. Measured: a frozen sidecar survived the drop
    /// indefinitely and the supervisor never woke up.
    ///
    /// So this is for the fault path only. The polite close still handles the
    /// idle drop, where the child is well and simply not wanted.
    pub fn kill(&self) {
        let Some(pgid) = self.pgid else { return };
        tracing::warn!(pgid, "killing the sidecar process group");
        // Negative pid means the group. SIGKILL because a process that stopped
        // answering will not act on anything catchable.
        unsafe { libc::kill(-(pgid as i32), libc::SIGKILL) };
    }

    /// Fire-and-forget, up to a ceiling.
    ///
    /// A closed channel means the child already died; the `Died` message is
    /// already on its way, so dropping there is correct rather than an error the
    /// UI has to handle twice.
    ///
    /// The ceiling is the other half. Rule 6 says a dead sidecar must not
    /// present as a healthy player; this is the same argument pointed the other
    /// way — **a runaway client must not be able to take the session with it.**
    /// A two-way binding on the volume button once cycled at a few thousand
    /// commands, each one an NDJSON write, a journald record and a D-Bus
    /// property change, and the machine had to be power-cycled. Nothing between
    /// `update()` and the desktop pushed back.
    pub fn send(&self, cmd: Command) {
        if !self.allow(cmd.name()) {
            return;
        }
        if self.tx.send(cmd).is_err() {
            tracing::debug!("sidecar command dropped: channel closed (child is gone)");
        }
    }

    /// Whether this command is within the ceiling for its kind.
    fn allow(&self, name: &'static str) -> bool {
        let now = std::time::Instant::now();
        // A poisoned lock would mean a panic while holding it — nothing here
        // panics, and refusing to send commands because of it would be worse
        // than the storm.
        let Ok(mut rate) = self.rate.lock() else {
            return true;
        };
        let window = rate.entry(name).or_insert(Window {
            started: now,
            sent: 0,
            warned: false,
        });

        if now.duration_since(window.started) >= std::time::Duration::from_secs(1) {
            window.started = now;
            window.sent = 0;
            window.warned = false;
        }

        window.sent += 1;
        if window.sent <= MAX_PER_SECOND {
            return true;
        }

        if !window.warned {
            window.warned = true;
            tracing::error!(
                cmd = name,
                ceiling = MAX_PER_SECOND,
                "command storm: dropping the rest of this second — something is looping"
            );
        }
        false
    }
}

/// Find the sidecar directory: an explicit override, the per-user install, the
/// system install, then the dev tree.
/// Say so when an installed sidecar is about to shadow the one beside the code.
///
/// **This is the trap CLAUDE.md warns about, made audible.** `locate` prefers
/// an installed sidecar over the build tree, so once anything has been
/// installed, `cargo run` runs fresh Rust against stale JavaScript and says
/// nothing. It fails in the most misleading way available: the command goes
/// out, the optimistic UI updates, and only MusicKit disagrees.
///
/// It cost an afternoon on `removeFromLibrary` and then a whole test round on
/// `moveInQueue` — fourteen `unknown-command` errors read as a broken feature
/// rather than a stale file. A build has a `sidecar/` next to its manifest and
/// an installed copy does not, so the two are distinguishable, and the line
/// costs nothing on a real install because the check cannot fire there.
#[cfg(debug_assertions)]
fn warn_if_shadowing_a_build_tree(chosen: &Path) {
    let dev = PathBuf::from(env!("SLIPMAT_WORKSPACE_ROOT")).join("sidecar");
    if dev.join("main.js").is_file() {
        tracing::warn!(
            using = %chosen.display(),
            ignoring = %dev.display(),
            "an installed sidecar is shadowing this build tree — \
             run with SLIPMAT_SIDECAR=$PWD/sidecar, or `make install-sidecar`"
        );
    }
}

#[cfg(not(debug_assertions))]
fn warn_if_shadowing_a_build_tree(_chosen: &Path) {}

pub fn locate() -> Result<PathBuf> {
    let mut tried = Vec::new();

    if let Ok(dir) = std::env::var("SLIPMAT_SIDECAR") {
        let p = PathBuf::from(dir);
        if p.join("main.js").is_file() {
            return Ok(p);
        }
        tried.push(p);
    }

    // Per-user first, then system-wide. `XDG_DATA_DIRS` is what makes a
    // packaged install work at all: `make install` puts the sidecar under
    // `~/.local/share`, but a distribution package puts it in
    // `/usr/share/slipmat/sidecar`, which nothing here used to look at.
    for data in dirs_data_home().into_iter().chain(dirs_data_dirs()) {
        let p = data.join("slipmat/sidecar");
        if p.join("main.js").is_file() {
            warn_if_shadowing_a_build_tree(&p);
            return Ok(p);
        }
        tried.push(p);
    }

    // The dev tree, and only in a dev build: the workspace root is where this
    // was compiled, which for a package is a build root that will not exist on
    // the machine running it. Baking it into a release binary is both useless
    // and what makes `makepkg` warn about a reference to `$srcdir`.
    #[cfg(debug_assertions)]
    {
        let dev = PathBuf::from(env!("SLIPMAT_WORKSPACE_ROOT")).join("sidecar");
        if dev.join("main.js").is_file() {
            return Ok(dev);
        }
        tried.push(dev);
    }

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
/// The system data directories, in preference order. Defaults to the values the
/// XDG spec mandates when the variable is unset, which is the common case.
fn dirs_data_dirs() -> Vec<PathBuf> {
    let raw = match std::env::var("XDG_DATA_DIRS") {
        Ok(x) if !x.is_empty() => x,
        _ => "/usr/local/share:/usr/share".to_owned(),
    };
    raw.split(':')
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .collect()
}

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
/// The receiver ends with exactly one `Incoming::Died`, which is `app/mod.rs`'s cue
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
        // **Piped, not inherited, and that is not about tidiness.**
        //
        // Inherited, Chromium holds the real terminal — and its children can
        // outlive us by a few milliseconds. Bytes landing after the shell has
        // taken the terminal back interleave with the shell's own setup: on
        // fish 4.8 and ghostty that truncated the kitty-keyboard negotiation
        // and left the terminal echoing `^[[27u` for every Escape and arrow
        // key until it was cleared. Confirmed by `cargo run 2>&1 | cat`, which
        // gives Chromium no terminal and does not break.
        //
        // Reading it ourselves also puts Chromium's noise behind `RUST_LOG`
        // and gives it a timestamp, which inheriting never could.
        .stderr(Stdio::piped())
        // Electron re-executes itself for its zygote/GPU processes; killing the
        // parent on drop keeps a crashed run from leaving Chromium behind.
        .kill_on_drop(true)
        // **Its own process group**, for two reasons. A signal aimed at the
        // daemon no longer reaches Chromium by accident, and — the one that
        // matters — `Handle::kill` can take the whole tree down by group
        // rather than killing a parent and orphaning nine renderers.
        .process_group(0)
        .spawn()
        .with_context(|| format!("failed to start {}", bin.display()))?;

    // Read before the child is moved into the reader task. `process_group(0)`
    // makes the group id equal the child's own pid.
    let pgid = child.id();

    let stdout = child.stdout.take().context("child stdout was not piped")?;
    let mut stdin = child.stdin.take().context("child stdin was not piped")?;
    let stderr = child.stderr.take().context("child stderr was not piped")?;

    // Logger task — owns stderr, and outlives nothing: when the pipe closes
    // this ends, which is what keeps Chromium's parting words off a terminal
    // somebody else now owns.
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            // Our own `log()` in main.js prefixes its lines; everything else is
            // Chromium talking to itself. The first is worth seeing by default,
            // the second only when something is being diagnosed.
            match line.strip_prefix("[sidecar] ") {
                Some(msg) => tracing::info!(%msg, "sidecar"),
                None if line.trim().is_empty() => {}
                None => tracing::debug!(%line, "chromium"),
            }
        }
    });

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

    Ok((
        Handle {
            tx: cmd_tx,
            rate: Default::default(),
            pgid,
        },
        evt_rx,
    ))
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

    /// A handle with no child on the other end. The channel is what `send`
    /// writes to; for the ceiling only the counting matters.
    fn handle() -> (Handle, mpsc::UnboundedReceiver<Command>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Handle {
                tx,
                rate: Default::default(),
                pgid: None,
            },
            rx,
        )
    }

    #[test]
    fn a_command_storm_is_cut_off_at_the_ceiling() {
        // The failure this exists to stop: a loop in `update()` emitted 5,721
        // commands, each an NDJSON write, a journald record and a D-Bus
        // property change, and the desktop stopped responding (#37).
        let (h, mut rx) = handle();
        for _ in 0..5_000 {
            h.send(Command::Seek { position_ms: 500 });
        }
        let mut delivered = 0;
        while rx.try_recv().is_ok() {
            delivered += 1;
        }
        assert_eq!(
            delivered, MAX_PER_SECOND as usize,
            "the ceiling should have cut this off"
        );
    }

    #[test]
    fn the_ceiling_is_per_command_kind() {
        // One runaway command must not silence the rest. A seek loop that
        // also blocked `pause` would leave the user unable to stop the noise.
        let (h, mut rx) = handle();
        for _ in 0..5_000 {
            h.send(Command::Seek { position_ms: 500 });
        }
        h.send(Command::Pause);
        let mut pauses = 0;
        while let Ok(cmd) = rx.try_recv() {
            if matches!(cmd, Command::Pause) {
                pauses += 1;
            }
        }
        assert_eq!(pauses, 1, "pause was collateral damage");
    }

    #[test]
    fn ordinary_use_never_reaches_it() {
        // The ceiling must sit above the fastest thing a person can do, which
        // is a pointer drag at the display's refresh rate. Clipping that would
        // be worse than the bug it guards against.
        let (h, mut rx) = handle();
        // 200 in a second is already beyond a 165Hz pointer dragging flat out.
        for _ in 0..200 {
            h.send(Command::Seek { position_ms: 500 });
        }
        let mut delivered = 0;
        while rx.try_recv().is_ok() {
            delivered += 1;
        }
        assert_eq!(delivered, 200, "a real drag must not be clipped");
    }

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
        let err = electron_binary(Path::new("/nonexistent/slipmat")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("make sidecar"), "unhelpful error: {msg}");
        assert!(
            msg.contains("/nonexistent/slipmat"),
            "should say where it looked: {msg}"
        );
    }
}

#[cfg(test)]
mod data_dirs_tests {
    use super::*;

    #[test]
    fn the_system_data_dirs_default_to_what_xdg_mandates() {
        // A packaged install lands in one of these. If this ever returns
        // nothing, `/usr/share/slipmat/sidecar` becomes unreachable and every
        // distribution package silently stops working.
        //
        // Read from the environment, so this asserts on the shape rather than
        // on the values: a test that mutates the environment is a test that
        // breaks whichever other test runs beside it.
        let dirs = dirs_data_dirs();
        assert!(!dirs.is_empty(), "there must always be somewhere to look");
        assert!(dirs.iter().all(|p| p.is_absolute()));
    }
}
