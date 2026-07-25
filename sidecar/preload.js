// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later
//
// The hook. This is the ONE fragile surface in Tonearm (CLAUDE.md rule 4):
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

const emit = (event, payload) => ipcRenderer.send('tonearm:event', { event, ...payload })

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
window.__tonearmReady = () => {
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

function currentQueue() {
  const items = pick(() => music.queue && music.queue.items) || []
  return {
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
    ipcRenderer.send('tonearm:event', { event: 'hook-warning', detail: `no event ${name}` })
  }
}

function wireEvents() {
  on('playbackStateDidChange', () =>
    emit('playbackState', { state: stateName(pick(() => music.playbackState) ?? 0) }))

  on('nowPlayingItemDidChange', () =>
    emit('nowPlaying', {
      item: serializeItem(pick(() => music.nowPlayingItem)),
      queue: currentQueue(),
    }))

  on('playbackTimeDidChange', () =>
    emit('position', {
      positionMs: Math.round((pick(() => music.currentPlaybackTime) || 0) * 1000),
      durationMs: Math.round((pick(() => music.currentPlaybackDuration) || 0) * 1000),
    }))

  on('queueItemsDidChange', () => emit('queue', currentQueue()))
  on('queuePositionDidChange', () => emit('queue', currentQueue()))

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

const commands = {
  async setQueue({ songs, startPosition = 0 }) {
    await music.setQueue({ songs, startPosition, startPlaying: true })
  },
  play: () => music.play(),
  pause: () => music.pause(),
  playPause: () => (music.isPlaying ? music.pause() : music.play()),
  next: () => music.skipToNextItem(),
  previous: () => music.skipToPreviousItem(),
  changeToIndex: ({ index }) => music.changeToMediaAtIndex(index),
  seek: ({ positionMs }) => music.seekToTime(positionMs / 1000),
  setVolume: ({ volume }) => {
    music.volume = volume
  },
  setShuffle: ({ shuffle }) => {
    music.shuffleMode = shuffle ? 1 : 0
  },
  setRepeat: ({ mode }) => {
    // MusicKit: 0 none, 1 one, 2 all
    music.repeatMode = mode === 'one' ? 1 : mode === 'all' ? 2 : 0
  },
  authorize: () => music.authorize(),
  unauthorize: () => music.unauthorize(),
  refreshTokens: () => pushTokens(),
}

ipcRenderer.on('tonearm:command', async (_e, msg) => {
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
// So main.js polls window.__tonearmReady() over executeJavaScript, which runs
// regardless of renderer timer state, and sends `tonearm:wire` when MusicKit is
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
//   2. main.js probing window.__tonearmReady() over executeJavaScript, which
//      keeps working in situations where the renderer's own timers stall.
//
// Whichever wins calls wire(); the `if (music) return` guard makes the loser a
// no-op. Belt and braces on purpose — this handshake failing silently is the
// worst failure mode the sidecar has.
ipcRenderer.on('tonearm:wire', () => {
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
