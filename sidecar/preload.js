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

function waitForMusicKit() {
  return new Promise((resolve, reject) => {
    const deadline = Date.now() + READY_TIMEOUT_MS
    const tick = () => {
      const inst = getInstance()
      if (inst) return resolve(inst)
      if (Date.now() > deadline) {
        return reject(new Error('MusicKit.getInstance() never appeared'))
      }
      setTimeout(tick, READY_POLL_MS)
    }
    tick()
  })
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

function pushTokens() {
  const t = readTokens()
  if (t) emit('tokens', t)
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
  const fn = commands[msg.cmd]
  if (!fn) return emit('error', { code: 'unknown-command', detail: msg.cmd })
  try {
    await fn(msg)
    if (msg.id) emit('ack', { id: msg.id })
  } catch (err) {
    emit('error', { code: 'command-failed', detail: `${msg.cmd}: ${err && err.message}` })
  }
})

// ---------------------------------------------------------------------------
// Boot
// ---------------------------------------------------------------------------

window.addEventListener('DOMContentLoaded', async () => {
  try {
    music = await waitForMusicKit()
  } catch (err) {
    // The loud failure demanded by rule 4. Rust turns this into a toast that
    // names the fix; it must never look like "still loading".
    return emit('hook-failed', { detail: String(err && err.message) })
  }

  wireEvents()

  const t = pushTokens()
  // The developer token can land a beat after MusicKit itself. Retry briefly
  // rather than declaring failure.
  if (!t) {
    let tries = 0
    tokenTimer = setInterval(() => {
      if (pushTokens() || ++tries > 40) clearInterval(tokenTimer)
    }, 500)
  }

  emit('hook-ready', {
    authorized: !!pick(() => music.isAuthorized),
    version: pick(() => window.MusicKit.version) || 'unknown',
  })
})
