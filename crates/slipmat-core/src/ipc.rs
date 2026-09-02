// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The daemon contract, in one file (rule 9's discipline, a second time).
//!
//! Newline-delimited JSON over a Unix socket, the same shape as the sidecar's
//! protocol and for the same reason: one file holds the whole surface, so a
//! request and its handler cannot drift apart across a release.
//!
//! **Field names are snake_case, tag values are camelCase.** The fields match
//! the domain types `Entry` carries verbatim — and those are also
//! `library.json`'s format, so renaming them would silently orphan every
//! cached library on disk. Tags are names rather than fields: `playPause`.
//!
//! **The client sends intent, never state.** `slipmatd` owns the sidecar and
//! the mirror; a frontend asks for things and is told what happened. That is
//! rule 3 one hop further out — MusicKit owns the queue, the daemon mirrors it,
//! and clients mirror the daemon.

use serde::{Deserialize, Serialize};

use crate::sort::SortBy;

use crate::entry::Entry;
use crate::player::protocol::{Item, RepeatMode};
use crate::queue::Start;

/// Where the daemon listens. `$XDG_RUNTIME_DIR` is per-user and cleared on
/// logout, which is what a socket wants — a stale one in `~` outlives the
/// process that made it.
pub fn socket_path() -> Option<std::path::PathBuf> {
    let dir = std::env::var("XDG_RUNTIME_DIR")
        .ok()
        .filter(|d| !d.is_empty())?;
    Some(std::path::PathBuf::from(dir).join("slipmat.sock"))
}

/// Connect to the daemon, starting it if it is not there.
///
/// **Slipmat is an app you open, not a service you enable.** Nobody should have
/// to run `systemctl --user enable` before music works, so the first client to
/// arrive starts the daemon and the rest find it already running. A unit file
/// ships for anyone who wants playback to survive closing every window, but it
/// is an option rather than a step.
///
/// The race is benign: two clients starting at once both spawn, and the second
/// daemon exits on its own — binding the socket is what settles who owns it.
pub fn connect_or_spawn(exe: &std::path::Path) -> std::io::Result<std::os::unix::net::UnixStream> {
    use std::os::unix::net::UnixStream;

    let path = socket_path().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "XDG_RUNTIME_DIR is not set, so there is nowhere to put the socket",
        )
    })?;

    if let Ok(stream) = UnixStream::connect(&path) {
        return Ok(stream);
    }

    tracing::info!(daemon = %exe.display(), "no daemon listening — starting one");
    let mut command = std::process::Command::new(exe);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    // **Its own session, or the terminal takes it with it.** A plain child
    // inherits the client's process group *and* its session, and so does the
    // sidecar one level down — so closing the terminal SIGHUPs all three.
    // Chromium traps on SIGHUP and dumps core, the daemon dies before its
    // SIGTERM handler can save the session or clear the socket, and the music
    // stops. Measured: closing the window left a 7.8 MB Electron core dump and
    // an orphaned socket every time.
    //
    // `setsid` is the whole fix. It is the one thing that has to happen between
    // `fork` and `exec`, which is what `pre_exec` is for — and why it is
    // `unsafe`: only async-signal-safe calls are legal in that window, and
    // `setsid` is one.
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn()?;

    // **Reaped, or it becomes a zombie.** Dropping a `Child` neither kills nor
    // waits, so a daemon that dies while this client is still running would sit
    // in the process table until the client exited — and a daemon that keeps
    // crashing would leave one behind each time. The thread costs nothing: it
    // blocks for the daemon's whole life and then ends.
    let mut child = child;
    std::thread::Builder::new()
        .name("slipmatd-reaper".into())
        .spawn(move || {
            let _ = child.wait();
        })?;

    // It binds the socket before it does anything else, so this is the daemon
    // coming up rather than the sidecar — a fraction of a second, not the
    // seconds Chromium takes.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
        if let Ok(stream) = UnixStream::connect(&path) {
            return Ok(stream);
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "the daemon did not start listening",
    ))
}

/// Client → daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "req")]
pub enum Request {
    /// Drive playback. The daemon translates to the sidecar; nothing here says
    /// what the resulting state will be, because only MusicKit decides that.
    #[serde(rename = "transport")]
    Transport(Transport),

    /// Move within the queue the daemon already holds. **Not** a way to build
    /// one — that would cost the gapless buffer (rule 3).
    #[serde(rename = "jumpTo")]
    JumpTo { index: usize },

    /// Everything the daemon is holding, once. Sent on connect by a client that
    /// needs to draw before anything changes.
    #[serde(rename = "snapshot")]
    Snapshot,

    /// The queue as the daemon mirrors it.
    #[serde(rename = "queue")]
    Queue,

    /// Where the daemon stands right now.
    ///
    /// **Asked for, because [`Event::Stage`] only fires on a change.** A client
    /// attaching to a daemon that has been ready for an hour would otherwise
    /// never be told it was ready, and would draw its startup screen forever.
    #[serde(rename = "stage")]
    Stage,

    /// Start receiving [`Event`]s on this connection. Idempotent.
    #[serde(rename = "subscribe")]
    Subscribe,

    /// Show Apple's own sign-in.
    ///
    /// **The one thing that needs a window**, and the daemon has none of its
    /// own — it hands this to the sidecar, whose hidden Chromium is shown for
    /// exactly this and hidden again afterwards.
    #[serde(rename = "signIn")]
    SignIn,

    /// End the Apple session. Clears the cookies too, which only MusicKit's own
    /// context can do.
    #[serde(rename = "signOut")]
    SignOut,

    /// A page of the library. `query` filters, `offset`/`limit` window it —
    /// a client draws a screenful, not 535 rows.
    #[serde(rename = "browse")]
    Browse {
        view: View,
        #[serde(default)]
        query: String,
        #[serde(default)]
        offset: usize,
        /// Zero means "the rest".
        #[serde(default)]
        limit: usize,
        /// What to order by, and which way round.
        ///
        /// **The daemon sorts, not the client**, because the sort has to happen
        /// before the window is taken — ordering one page of a library is not
        /// ordering the library, and a client asking for rows 0..40 of an
        /// unsorted list gets the wrong forty.
        #[serde(default)]
        sort: SortBy,
        #[serde(default)]
        reverse: bool,
    },

    /// Search all of Apple Music, not just the library.
    ///
    /// **Here rather than in a client, because this is what needs the tokens**,
    /// and rule 7 keeps those in one process. `offset` pages the same way the
    /// catalog does — a client asks for more as somebody scrolls.
    #[serde(rename = "search")]
    Search {
        query: String,
        #[serde(default)]
        filter: CatalogFilter,
        #[serde(default)]
        offset: usize,
    },

    /// Open an album, artist or playlist. Fetched from Apple if it is not
    /// already known, which is why the answer arrives as an event rather than
    /// a return value.
    #[serde(rename = "open")]
    Open { kind: PageKind, id: String },

    /// Grow the queue MusicKit already holds, without rebuilding it.
    ///
    /// **Not a `Play`**, and that is the point: rebuilding a queue to add a
    /// track restarts playback and discards the gapless buffer (rule 3).
    #[serde(rename = "enqueue")]
    Enqueue {
        ids: Vec<String>,
        /// Right after the current track, rather than at the end.
        #[serde(default)]
        next: bool,
    },

    /// Drop one track from the loaded queue, by its position.
    #[serde(rename = "removeFromQueue")]
    RemoveFromQueue { index: usize },

    /// Move one track within the loaded queue. `to` is where it lands *after*
    /// it has been taken out, which is what a drag naturally means.
    #[serde(rename = "moveInQueue")]
    MoveInQueue { from: usize, to: usize },

    /// Empty the queue and stop.
    #[serde(rename = "clearQueue")]
    ClearQueue,

    /// Change what Apple holds for this account.
    #[serde(rename = "write")]
    Write { action: WriteAction, id: String },

    /// Re-read the library from Apple. Happens on its own once tokens arrive;
    /// this is for a client offering a reload button.
    #[serde(rename = "refresh")]
    Refresh,

    /// Build a queue from these ids and start playing.
    ///
    /// **Ids, not indices into something the daemon remembers.** The client
    /// already holds the rows it drew; sending them back is what keeps the
    /// daemon from having to mirror every client's scroll position.
    #[serde(rename = "play")]
    Play {
        ids: Vec<String>,
        /// Which of `ids` to open on. Ignored when `start` is `shuffled` —
        /// MusicKit reorders as it loads, so the row we name is not the row it
        /// opens on (#152).
        #[serde(default)]
        index: usize,
        #[serde(default)]
        start: PlayMode,
    },

    /// Stop the daemon: stop playing, save the session, exit.
    ///
    /// **Refused while another client is attached.** Quitting takes the player
    /// from every window on this machine, so a client can only ask for it when
    /// it is the last one holding the socket — otherwise the answer is an
    /// [`Event::Error`] and the client should just leave. That is also why
    /// MPRIS `Quit` is ignored outright: a media key must never mean this.
    #[serde(rename = "quit")]
    Quit,
}

/// Which kinds a catalog search should answer for.
///
/// Apple answers only for the kinds named, and leaves a key out entirely rather
/// than returning it empty — so this is a filter on the request, not on what
/// comes back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CatalogFilter {
    #[default]
    All,
    Songs,
    Albums,
    Artists,
    Playlists,
}

impl CatalogFilter {
    /// The `types=` value Apple wants.
    /// Every filter, in the order a client should step through them.
    pub const ALL: [Self; 5] = [
        Self::All,
        Self::Songs,
        Self::Albums,
        Self::Artists,
        Self::Playlists,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "everything",
            Self::Songs => "songs",
            Self::Albums => "albums",
            Self::Artists => "artists",
            Self::Playlists => "playlists",
        }
    }

    pub fn types(self) -> &'static str {
        match self {
            Self::All => "songs,albums,artists,playlists",
            Self::Songs => "songs",
            Self::Albums => "albums",
            Self::Artists => "artists",
            Self::Playlists => "playlists",
        }
    }
}

/// Something we can ask Apple to do to this account.
///
/// Adding and favouriting go over REST; removing and un-favouriting can only be
/// done by MusicKit itself, which is why they take different routes out. A
/// client does not need to know that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WriteAction {
    Favorite,
    Unfavorite,
    AddToLibrary,
    RemoveFromLibrary,
}

/// Which library section to browse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum View {
    Songs,
    Albums,
    Artists,
    Playlists,
}

/// What a [`Request::Open`] is opening.
///
/// Catalog and library are separate variants because **the two id spaces are
/// not interchangeable** — a catalog id 404s against `/me/library` and back
/// again — and because an artist page is their *albums* rather than their
/// tracks, which is a different call entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PageKind {
    Album,
    Artist,
    Playlist,
    LibraryAlbum,
    LibraryArtist,
    LibraryPlaylist,
}

/// The mode a queue is *created* in, on the wire.
///
/// Mirrors [`Start`], which is the type the arithmetic uses. Two enums because
/// this one is a contract with clients and that one is internal — and because
/// `Start::Clicked` needs a name a client can understand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PlayMode {
    /// A row click: this list, from here, in order.
    #[default]
    Clicked,
    /// A Play button: in order, whatever mode was on before.
    InOrder,
    /// A Shuffle button.
    Shuffled,
}

impl From<PlayMode> for Start {
    fn from(mode: PlayMode) -> Self {
        match mode {
            PlayMode::Clicked => Start::Clicked,
            PlayMode::InOrder => Start::InOrder,
            PlayMode::Shuffled => Start::Shuffled,
        }
    }
}

/// The transport verbs, kept separate from [`Request`] so a client can pass one
/// around without carrying the rest of the protocol.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(tag = "cmd", rename_all = "camelCase")]
pub enum Transport {
    Play,
    Pause,
    PlayPause,
    Next,
    Previous,
    /// Absolute, in milliseconds.
    Seek {
        position_ms: u64,
    },
    SetVolume {
        volume: f64,
    },
    SetShuffle {
        shuffle: bool,
    },
    SetRepeat {
        mode: RepeatMode,
    },
}

/// Daemon → client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event")]
pub enum Event {
    /// What is playing and how. The one a bar redraws from.
    #[serde(rename = "snapshot")]
    Snapshot(Snapshot),

    /// The queue changed — reordered, replaced, or emptied. Carries the whole
    /// list: it is bounded by what MusicKit will hold, and a diff protocol here
    /// would be a second reconciliation to keep honest.
    #[serde(rename = "queue")]
    Queue {
        items: Vec<QueueItem>,
        position: usize,
    },

    /// Where the daemon is in its own startup, or why it is not playable.
    #[serde(rename = "stage")]
    Stage(Stage),

    /// A window of the library, answering a [`Request::Browse`].
    #[serde(rename = "rows")]
    Rows {
        view: View,
        entries: Vec<Entry>,
        /// How many matched before `offset`/`limit`, so a client can show a
        /// scrollbar without asking for everything.
        total: usize,
    },

    /// Catalog results, answering a [`Request::Search`].
    ///
    /// Carries the query it answers: a client types faster than Apple replies,
    /// and a result for two keystrokes ago must not replace one for the word
    /// that is actually in the box.
    #[serde(rename = "results")]
    Results {
        query: String,
        entries: Vec<Entry>,
        offset: usize,
        /// Whether asking again at a higher offset is worth it.
        more: bool,
    },

    /// An opened album, artist or playlist.
    ///
    /// `header` is the thing itself — the album with its artwork and year, the
    /// artist with their portrait — because a page draws that above its rows
    /// and asking for it separately would be a second round trip for one object
    /// the fetch already had.
    #[serde(rename = "page")]
    Page {
        kind: PageKind,
        id: String,
        header: Entry,
        entries: Vec<Entry>,
    },

    /// The library changed under a client — a refresh landed, or a write
    /// settled. **An invalidation, not the rows**: a client asks for the page
    /// it is drawing rather than having 535 pushed at it.
    #[serde(rename = "libraryChanged")]
    LibraryChanged,

    /// Whether the daemon is replacing the complete library cache.
    #[serde(rename = "libraryRefreshing")]
    LibraryRefreshing { refreshing: bool },

    /// Something went wrong that a person should see.
    #[serde(rename = "error")]
    Error { detail: String },
}

/// One row of the queue.
///
/// Not `protocol::Item`, whose doc says it stays inside `player/` — and it
/// should: that is MusicKit's shape, carrying two id spaces and an artwork
/// template a client has no business resolving. This is what a row needs to
/// draw itself and to be jumped to.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct QueueItem {
    /// Whichever id the daemon can act on — catalog if there is one, else the
    /// library id. Opaque to the client; it comes back in `JumpTo`.
    pub id: Option<String>,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u64,
}

impl From<&Item> for QueueItem {
    fn from(item: &Item) -> Self {
        Self {
            id: item.catalog_id.clone().or_else(|| item.id.clone()),
            title: item.title.clone(),
            artist: item.artist.clone(),
            album: item.album.clone(),
            duration_ms: item.duration_ms,
        }
    }
}

/// What is playing, flattened for a client that only draws.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Snapshot {
    /// Which track this is, so a client can tell one snapshot from the next
    /// without consulting the queue. **They arrive as separate events**, and a
    /// client that asks the queue "what is playing" while holding a snapshot
    /// from before the change gets one answer from each.
    pub track_id: Option<String>,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// A local file, already fetched. Clients do not talk to Apple for art.
    pub art_path: Option<String>,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub playing: bool,
    /// Still working towards audio. A client should not read this as paused.
    pub busy: bool,
    pub volume: f64,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub can_next: bool,
    pub can_previous: bool,
}

/// How far along the daemon is. A client draws something different for each.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(tag = "stage", rename_all = "camelCase")]
pub enum Stage {
    /// The sidecar is up but MusicKit has not attached yet. Also what a client
    /// assumes before it has asked.
    #[default]
    Connecting,
    /// Ready to play.
    Ready,
    /// No Apple session. A client cannot fix this — sign-in needs a window.
    SignedOut,
    /// Broken in a way a restart will not fix.
    Broken { detail: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire names are the contract. A rename here is a breaking change for
    /// every client, so it fails a test rather than a user's bar.
    #[test]
    fn requests_carry_the_names_clients_send() {
        let seek =
            serde_json::to_string(&Request::Transport(Transport::Seek { position_ms: 4200 }))
                .unwrap();
        assert_eq!(
            seek,
            r#"{"req":"transport","cmd":"seek","position_ms":4200}"#
        );

        let sub = serde_json::to_string(&Request::Subscribe).unwrap();
        assert_eq!(sub, r#"{"req":"subscribe"}"#);
    }

    #[test]
    fn a_request_round_trips_through_its_own_wire_form() {
        let sent = Request::Transport(Transport::SetRepeat {
            mode: RepeatMode::All,
        });
        let line = serde_json::to_string(&sent).unwrap();
        let back: Request = serde_json::from_str(&line).unwrap();
        assert!(matches!(
            back,
            Request::Transport(Transport::SetRepeat {
                mode: RepeatMode::All
            })
        ));
    }

    #[test]
    fn library_refresh_state_is_explicit_on_the_wire() {
        let started =
            serde_json::to_string(&Event::LibraryRefreshing { refreshing: true }).unwrap();
        let finished =
            serde_json::to_string(&Event::LibraryRefreshing { refreshing: false }).unwrap();

        assert_eq!(
            started,
            r#"{"event":"libraryRefreshing","refreshing":true}"#
        );
        assert_eq!(
            finished,
            r#"{"event":"libraryRefreshing","refreshing":false}"#
        );
    }

    #[test]
    fn an_unknown_request_is_an_error_rather_than_a_default() {
        // A client from a newer version must fail loudly here, not be silently
        // read as something else — which is what an untagged enum would do.
        let bad = serde_json::from_str::<Request>(r#"{"req":"teleport"}"#);
        assert!(bad.is_err());
    }

    #[test]
    fn the_socket_lives_under_the_runtime_dir() {
        // Not `~`: a socket that outlives the session is a socket that answers
        // for a daemon which is not running.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000") };
        assert_eq!(
            socket_path().unwrap(),
            std::path::PathBuf::from("/run/user/1000/slipmat.sock")
        );
    }
}
