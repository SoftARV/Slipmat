// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later
//
// The hook. This is the ONE fragile surface in Slipmat (CLAUDE.md rule 4):
// it reaches into a page Apple can change without warning.
//
// Rules for editing this file:
//   - Feature-detect everything. Never assume a property exists.
//   - Never scrape the DOM. Only MusicKit.getInstance() and its events.
//     DOM scraping is why wrappers break monthly.
//   - Fail LOUDLY (`hook-failed`) rather than degrade into a dead player.
//   - Keep it small. Every line here is a line that isn't native.

const { ipcRenderer } = require('electron')

const READY_TIMEOUT_MS = 60_000
const READY_POLL_MS = 250

let music = null
let tokenTimer = null

const emit = (event, payload) => ipcRenderer.send('slipmat:event', { event, ...payload })

// Proof-of-life, sent before anything can go wrong. If Rust sees no
// `hook-boot` at all, the preload is not running and no amount of debugging
// inside it will help — check webPreferences.preload and sandbox instead.
emit('hook-boot', {
  readyState: (typeof document !== 'undefined' && document.readyState) || 'no-document',
  href: (typeof location !== 'undefined' && location.href) || 'unknown',
})

/** Try a list of accessors and return the first that yields something. */
function pick(...getters) {
  for (const get of getters) {
    try {
      const v = get()
      if (v !== undefined && v !== null && v !== '') return v
    } catch {
      /* keep trying — a throwing getter just means that shape is gone */
    }
  }
  return null
}

// ---------------------------------------------------------------------------
// Readiness
// ---------------------------------------------------------------------------

function getInstance() {
  return pick(() => window.MusicKit && window.MusicKit.getInstance())
}

/// Is MusicKit up? Called BY THE MAIN PROCESS via executeJavaScript, because a
/// timer in here cannot be trusted to run (see the wiring note below).
window.__slipmatReady = () => {
  try {
    return !!(window.MusicKit && window.MusicKit.getInstance())
  } catch {
    return false
  }
}

// ---------------------------------------------------------------------------
// Tokens
//
// Harvested live, never cached (CLAUDE.md rule 7). The developer token is
// whatever music.apple.com is using right now, so if Apple rotates it we
// follow automatically. Apple has moved where it hangs these more than once —
// hence pick().
// ---------------------------------------------------------------------------

function readTokens() {
  if (!music) return null
  const developerToken = pick(
    () => music.developerToken,
    () => music.api && music.api.developerToken,
    () => music._api && music._api.developerToken,
    () => window.MusicKit._instance && window.MusicKit._instance.developerToken,
  )
  const musicUserToken = pick(
    () => music.musicUserToken,
    () => music.api && music.api.userToken,
  )
  const storefront = pick(
    () => music.storefrontId,
    () => music.api && music.api.storefrontId,
    () => music.storefrontCountryCode,
  )
  if (!developerToken) return null
  return {
    developerToken,
    musicUserToken: musicUserToken || null,
    storefront: storefront || 'us',
    authorized: !!pick(() => music.isAuthorized),
  }
}

let lastTokens = ''

/// Emit tokens only when they actually change.
///
/// main.js nudges this once a second for the first ten seconds (the developer
/// token can land after MusicKit), and unconditional emitting turned that into
/// ten identical log lines that buried everything else.
function pushTokens() {
  const t = readTokens()
  if (!t) return null
  const fingerprint = JSON.stringify(t)
  if (fingerprint === lastTokens) return t
  lastTokens = fingerprint
  emit('tokens', t)
  return t
}

// ---------------------------------------------------------------------------
// State serialisation — our types stop at the Rust boundary, but keep the
// payload small and stable so player/protocol.rs has a narrow contract.
// ---------------------------------------------------------------------------

const STATES = [
  'none', 'loading', 'playing', 'paused', 'stopped',
  'ended', 'seeking', 'unknown', 'waiting', 'stalled', 'completed',
]

const stateName = (n) => STATES[n] || 'unknown'

function serializeItem(item) {
  if (!item) return null
  return {
    id: pick(() => item.id, () => item.playbackId) || null,
    catalogId: pick(() => item.catalogId, () => item.container && item.container.id) || null,
    title: pick(() => item.title, () => item.attributes && item.attributes.name) || '',
    artist: pick(() => item.artistName, () => item.attributes && item.attributes.artistName) || '',
    album: pick(() => item.albumName, () => item.attributes && item.attributes.albumName) || '',
    durationMs: pick(
      () => item.playbackDuration,
      () => item.attributes && item.attributes.durationInMillis,
    ) || 0,
    trackNumber: pick(() => item.trackNumber, () => item.attributes && item.attributes.trackNumber) || 0,
    // A TEMPLATE url containing {w}/{h}/{f} — Rust substitutes the size it
    // wants and caches to disk, because MPRIS needs a file:// path.
    artworkTemplate: pick(
      () => item.artwork && item.artwork.url,
      () => item.attributes && item.attributes.artwork && item.attributes.artwork.url,
    ) || null,
  }
}

// `reason` is the one thing Rust cannot work out for itself, and the two values
// mean opposite things:
//
//   'items'     the queue was EDITED. MusicKit does not re-index its own
//               position afterwards, so it has to be told where the current
//               track went (#117, #118).
//   'position'  MusicKit moved its own cursor. Correcting that is fighting it —
//               including the pre-advance it does a few hundred ms before every
//               track boundary, which is what makes gapless seamless (#121).
//
// Every caller passes it explicitly. A default here would be a guess at the one
// question this argument exists to answer.
function currentQueue(reason) {
  const items = pick(() => music.queue && music.queue.items) || []
  return {
    reason,
    position: pick(() => music.queue && music.queue.position) ?? 0,
    items: items.map(serializeItem),
  }
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

function on(name, fn) {
  try {
    music.addEventListener(name, fn)
  } catch {
    // An event this MusicKit version doesn't know about is survivable; a
    // missing *critical* one shows up as a player that never updates.
    ipcRenderer.send('slipmat:event', { event: 'hook-warning', detail: `no event ${name}` })
  }
}

// MusicKit: shuffleMode 0 off / 1 on; repeatMode 0 none / 1 one / 2 all.
function emitModes() {
  emit('modes', {
    shuffle: (pick(() => music.shuffleMode) ?? 0) === 1,
    repeat: ['none', 'one', 'all'][pick(() => music.repeatMode) ?? 0] ?? 'none',
  })
}

function wireEvents() {
  on('playbackStateDidChange', () =>
    emit('playbackState', { state: stateName(pick(() => music.playbackState) ?? 0) }))

  on('nowPlayingItemDidChange', () =>
    emit('nowPlaying', {
      item: serializeItem(pick(() => music.nowPlayingItem)),
      queue: currentQueue('position'),
    }))

  on('playbackTimeDidChange', () =>
    emit('position', {
      positionMs: Math.round((pick(() => music.currentPlaybackTime) || 0) * 1000),
      durationMs: Math.round((pick(() => music.currentPlaybackDuration) || 0) * 1000),
    }))

  // Shuffle and repeat. Without these the Rust mirror never learns the mode,
  // so its toggle reads false forever and every click sends "on" again.
  on('shuffleModeDidChange', emitModes)
  on('repeatModeDidChange', emitModes)

  // Subscribed separately and reported separately. Collapsing them into one
  // event is what blinded the gapless check: see `currentQueue`.
  on('queueItemsDidChange', () => emit('queue', currentQueue('items')))
  on('queuePositionDidChange', () => emit('queue', currentQueue('position')))

  on('authorizationStatusDidChange', () => {
    const t = pushTokens()
    emit('authorization', { authorized: !!(t && t.authorized) })
  })
}

// ---------------------------------------------------------------------------
// Commands from Rust
//
// Note the absence of any per-track play: rule 3 says MusicKit owns the queue.
// `setQueue` is sent ONCE with the whole list; moving within it is
// changeToMediaAtIndex, never a fresh setQueue.
// ---------------------------------------------------------------------------

// `playNext` and `playLater` are documented on MusicKit v3, but this page ships
// whichever version it likes (rule 4), so feature-detect and fail loudly rather
// than throwing a bare TypeError at somebody who clicked a menu item.
async function enqueue(method, songs) {
  if (typeof music[method] !== 'function') {
    throw new Error(`this MusicKit build has no ${method}`)
  }
  if (!Array.isArray(songs) || songs.length === 0) {
    throw new Error(`${method} called with no songs`)
  }

  const before = pick(() => music.queue?.items?.length) ?? 0
  await music[method]({ songs })
  const after = pick(() => music.queue?.items?.length) ?? 0

  // `queueItemsDidChange` does not fire for playNext/playLater in this
  // MusicKit build, so the mirror would keep showing the old queue and the
  // insert would look like it did nothing. Push it ourselves.
  emit('queue', currentQueue('items'))

  // And say so if the queue genuinely did not grow — silently doing nothing is
  // the failure this project keeps refusing to ship.
  if (after <= before) {
    throw new Error(`${method} did not change the queue (still ${after} items)`)
  }
}

/// Run a library write and treat an empty response body as success.
///
/// These endpoints answer `202 Accepted` with **no content**, and MusicKit's
/// client parses every response as JSON — so success arrives as
/// `SyntaxError: Unexpected end of JSON input`. Rethrowing that would report a
/// write that actually happened as a failure, which is how the first working
/// call looked broken.
///
/// Anything else is a real error and still throws, so `dispatch` reports it.
async function accepted(fn) {
  try {
    return await fn()
  } catch (err) {
    // A library write answers 202 with **no body**, and MusicKit's client parses
    // every response as JSON — so success arrives as a SyntaxError.
    //
    // Matched on the error *type* plus an empty body, not on message text. The
    // text alone would also swallow a genuine failure whose body happened to be
    // truncated or malformed, and report it as an accepted write — which is the
    // exact failure this whole path exists to stop being silent.
    const empty = !err || err.body === undefined || err.body === null || err.body === ''
    if (err instanceof SyntaxError && empty) return null
    throw err
  }
}

/// Run a library write and report its outcome **against the id it was for**.
///
/// `cmd-done` carries only the command name, and this dispatch is async, so two
/// removals can finish out of order — correlating by name lets one command's
/// completion be attributed to another's row. These carry the id so Rust can
/// match exactly.
async function libraryWrite(kind, id, fn) {
  try {
    await accepted(fn)
    emit('library-write', { kind, id, ok: true, detail: '' })
  } catch (err) {
    const detail = String((err && err.message) || err)
    log('library write failed:', kind, id, detail)
    emit('library-write', { kind, id, ok: false, detail })
  }
}

const commands = {
  async setQueue({ songs, startPosition = 0, startPlaying = true, startTimeMs = 0 }) {
    // BOTH keys, deliberately. MusicKit v3's setQueue forwards only
    // `startWith` to the queue descriptor:
    //
    //   startPlaying: e.startPlaying, startTime: e.startTime,
    //   startWith: e.startWith, context: e.context, ...
    //
    // so a lone `startPosition` is silently dropped and playback always begins
    // at index 0 — the queue is correct, just started in the wrong place.
    // Deeper down the descriptor does accept either (`startWith ?? startPosition`),
    // so sending both is harmless and survives whichever layer a future
    // MusicKit build hands the options to.
    // `startTime` is seconds, and it is how a restored queue comes back where
    // it was left. Seeking afterwards does not work while nothing is playing:
    // there is no current item to seek within.
    await music.setQueue({
      songs,
      startWith: startPosition,
      startPosition,
      startPlaying,
      startTime: startTimeMs / 1000,
    })
  },
  play: () => music.play(),
  pause: () => music.pause(),
  playPause: () => (music.isPlaying ? music.pause() : music.play()),
  next: () => music.skipToNextItem(),
  previous: () => music.skipToPreviousItem(),
  changeToIndex: ({ index }) => music.changeToMediaAtIndex(index),
  // Move one item within the queue MusicKit already holds.
  //
  // `splice` is undocumented — feature-detected rather than assumed, like
  // `remove` beside it. Its own source gives away the shape:
  //
  //     splice(e, n, d = []) {
  //       return toMediaItems(this.spliceQueueItems(e, n, toQueueItems(d)))
  //     }
  //
  // so it is `Array.prototype.splice` semantics, and the removed items come
  // back as media items ready to be handed straight to the insert.
  // Tell MusicKit where the current track ended up.
  //
  // **A splice does not re-index the position.** Measured: playing index 36,
  // two drags across it, and MusicKit still reports 36 — so `skipToNextItem`
  // advances 36 -> 37 and plays whatever now sits there, which is not the
  // track after the one playing. The queue looks right and playback follows a
  // number that no longer means anything.
  //
  // `position` has a real setter, and `_updatePosition` returns early when the
  // value is unchanged, so this is safe to send after every move.
  syncQueuePosition: ({ index }) => {
    const q = music.queue
    if (!q) throw new Error('no queue to reposition')
    const len = q.items?.length ?? 0
    if (!Number.isInteger(index) || index < 0 || index >= len) {
      throw new Error(`queue position ${index} out of range (queue holds ${len})`)
    }
    q.position = index
  },
  moveInQueue: ({ from, to }) => {
    if (typeof music.queue?.splice !== 'function') {
      throw new Error('this MusicKit build cannot reorder the queue')
    }
    const len = music.queue.items?.length ?? 0
    for (const [name, i] of [['from', from], ['to', to]]) {
      if (!Number.isInteger(i) || i < 0 || i >= len) {
        throw new Error(`queue index ${name}=${i} out of range (queue holds ${len})`)
      }
    }
    if (from === to) return
    const moved = music.queue.splice(from, 1)
    if (!moved || moved.length !== 1) {
      throw new Error(`splice removed ${moved ? moved.length : 0} items, expected 1`)
    }
    music.queue.splice(to, 0, moved)
  },
  removeFromQueue: ({ index }) => {
    // `queue.remove` is not in MusicKit's documented surface, so treat it as
    // load-bearing-but-unowned: check it exists rather than throwing a
    // TypeError at the user, and let the queue event report the real result.
    if (typeof music.queue?.remove !== 'function') {
      throw new Error('this MusicKit build cannot remove queue items')
    }
    // Bounds-check here too. MusicKit answers an out-of-range index with a bare
    // `[mk-007] INVALID_ARGUMENTS`, which says nothing about what was wrong;
    // this at least names the numbers.
    const len = music.queue.items?.length ?? 0
    if (!Number.isInteger(index) || index < 0 || index >= len) {
      throw new Error(`queue index ${index} out of range (queue holds ${len})`)
    }
    music.queue.remove(index)
  },
  // Insert into the queue MusicKit already holds, rather than rebuilding it.
  // A fresh setQueue would restart playback and throw away the gapless buffer,
  // which is the whole reason rule 3 exists — these two are the *only*
  // sanctioned way to grow a queue that is already playing.
  playNext: ({ songs }) => enqueue('playNext', songs),
  playLater: ({ songs }) => enqueue('playLater', songs),
  // Emptying the queue is not one documented call, so try the ways it might
  // be spelled and fall back to the one that always exists (rule 4). Stopping
  // first matters: an empty queue with a track still playing is a player in a
  // state nothing else expects.
  async clearQueue() {
    await music.stop()
    if (typeof music.clearQueue === 'function') {
      await music.clearQueue()
    } else if (typeof music.queue?.splice === 'function') {
      const len = music.queue.items?.length ?? 0
      music.queue.splice(0, len)
    } else {
      await music.setQueue({ songs: [] })
    }
    // `queueItemsDidChange` is not reliable for this either — same as
    // playNext/playLater.
    emit('queue', currentQueue('items'))
    const left = pick(() => music.queue?.items?.length) ?? 0
    if (left > 0) {
      throw new Error(`could not clear the queue (${left} items remain)`)
    }
  },
  seek: ({ positionMs }) => music.seekToTime(positionMs / 1000),
  setVolume: ({ volume }) => {
    music.volume = volume
  },
  setShuffle: ({ shuffle }) => {
    music.shuffleMode = shuffle ? 1 : 0
    // Echoed explicitly. MusicKit does not reliably fire
    // shuffleModeDidChange for a *programmatic* change, and a mode the Rust
    // side never hears about is a toggle that springs back.
    emitModes()
    // Turning shuffle off restores the queue's original order, so the queue
    // itself has changed even though no item was added or removed.
    emit('queue', currentQueue('items'))
  },
  setRepeat: ({ mode }) => {
    // MusicKit: 0 none, 1 one, 2 all
    music.repeatMode = mode === 'one' ? 1 : mode === 'all' ? 2 : 0
    emitModes()
  },
  // Removing things. **Only MusicKit can do these**, which is why they are here
  // and not in music/client.rs with their add counterparts.
  //
  // Verified against a real account: over REST with our harvested token,
  // `DELETE /v1/me/favorites?ids[songs]=…` answers `400 Insufficient
  // Permissions` and library removal has no documented endpoint at all. Issued
  // through MusicKit's own client, from the page and its session, both are
  // accepted. See issue #34 for the measurements.
  //
  // Two traps live in these four lines:
  //
  //   * `music.api.music(path, {}, {fetchOptions: {method: 'DELETE'}})`
  //     **silently performs a GET.** The verb helpers are the only way to send
  //     one, and a probe that uses the wrong one reports "Resource Not Found"
  //     for a route that exists.
  //   * Only the *per-resource* path works for the library. The collection
  //     forms fail the same way they do over REST: `?ids[songs]=` gives 405 and
  //     `/songs?ids=` gives 400. Favourites are the other way round — there it
  //     is the query form that works.
  removeFromLibrary: ({ id }) =>
    libraryWrite('remove', id, () =>
      music.api.delete('/v1/me/library/songs/' + encodeURIComponent(id))),
  unfavorite: ({ id }) =>
    libraryWrite('unfavorite', id, () =>
      music.api.delete('/v1/me/favorites?ids[songs]=' + encodeURIComponent(id))),

  authorize: () => music.authorize(),
  unauthorize: () => music.unauthorize(),
  refreshTokens: () => pushTokens(),
}

ipcRenderer.on('slipmat:command', async (_e, msg) => {
  // Report arrival BEFORE doing anything. If Rust sends a command and no
  // `cmd-recv` comes back, the renderer never ran the handler at all — which
  // is a completely different problem from the command failing, and the two
  // are indistinguishable without this.
  emit('cmd-recv', { cmd: msg.cmd })

  const fn = commands[msg.cmd]
  if (!fn) return emit('error', { code: 'unknown-command', detail: msg.cmd })
  try {
    await fn(msg)
    // Always report completion, not just when an id was supplied: a command
    // that resolves without producing any MusicKit event is the signature of
    // playback being blocked rather than failing.
    emit('cmd-done', {
      cmd: msg.cmd,
      state: pick(() => music.playbackState) ?? -1,
      queueLen: pick(() => music.queue && music.queue.items && music.queue.items.length) ?? -1,
    })
  } catch (err) {
    emit('error', { code: 'command-failed', detail: `${msg.cmd}: ${err && err.message}` })
  }
})

// ---------------------------------------------------------------------------
// Boot
// ---------------------------------------------------------------------------

// Wiring is triggered by the MAIN process, not by a timer in here.
//
// A window created with show:false has its renderer frozen by Chromium:
// setTimeout fires once or twice and then stops for good. Neither
// webPreferences.backgroundThrottling nor the --disable-renderer-backgrounding
// family prevents it. The old self-polling loop emitted exactly one tick and
// then went silent for 90 seconds — no hook-ready and no hook-failed, because
// the loop that would have reported either had itself frozen.
//
// So main.js polls window.__slipmatReady() over executeJavaScript, which runs
// regardless of renderer timer state, and sends `slipmat:wire` when MusicKit is
// up. Everything after that point is event-driven, and Chromium does not freeze
// a page that is playing audio — so once playback starts the renderer stays
// awake on its own.
function wire(trigger) {
  if (music) return true // already wired; a duplicate trigger is harmless
  music = getInstance()
  if (!music) return false

  wireEvents()

  // The developer token can land a beat after MusicKit itself. main.js also
  // re-sends `refreshTokens` on a main-process timer, because a renderer timer
  // cannot be relied on here.
  pushTokens()

  emit('hook-ready', {
    trigger,
    authorized: !!pick(() => music.isAuthorized),
    version: pick(() => window.MusicKit.version) || 'unknown',
  })
  return true
}

// Two independent triggers, because neither is reliable alone:
//
//   1. The renderer self-poll below. Works when the page is live, and is what
//      succeeds on a normal desktop session.
//   2. main.js probing window.__slipmatReady() over executeJavaScript, which
//      keeps working in situations where the renderer's own timers stall.
//
// Whichever wins calls wire(); the `if (music) return` guard makes the loser a
// no-op. Belt and braces on purpose — this handshake failing silently is the
// worst failure mode the sidecar has.
ipcRenderer.on('slipmat:wire', () => {
  if (!wire('main-probe') && !music) {
    emit('hook-failed', { detail: 'MusicKit vanished between probe and wire' })
  }
})

function selfPoll() {
  const deadline = Date.now() + READY_TIMEOUT_MS
  const tick = () => {
    if (wire('self-poll')) return
    if (Date.now() > deadline) return // main.js owns the timeout report
    setTimeout(tick, READY_POLL_MS)
  }
  tick()
}

// Guard on readyState: the preload usually runs before the document parses, but
// on a warm cache it can already be past `loading`, and then DOMContentLoaded
// never fires again.
if (document.readyState === 'loading') {
  window.addEventListener('DOMContentLoaded', selfPoll, { once: true })
} else {
  selfPoll()
}
