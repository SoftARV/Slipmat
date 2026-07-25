// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Tonearm sidecar — the invisible half of the app.
//
// This process exists for exactly one reason: Widevine. Apple Music full tracks
// are HLS + Widevine, and on Linux the only CDM that exists ships inside
// Chromium. So we run castLabs Electron, load the real music.apple.com in a
// window that is never shown, and drive MusicKit from Rust over stdio.
//
// The window is shown exactly once — for Apple's own sign-in — and then hides
// forever; the session cookie persists in the `persist:tonearm` partition.
//
// PROTOCOL: newline-delimited JSON.
//   stdin  <- commands from Rust   { id?, cmd, ...args }
//   stdout -> events to Rust       { event, ...payload }
//   stderr -> human logs (Chromium's own noise lands here too)
//
// *** NOTHING may write to stdout except send(). A stray console.log corrupts
// *** the channel and the Rust side will drop the connection. Use log().

const { app, components, BrowserWindow, shell } = require('electron')
const path = require('node:path')
const readline = require('node:readline')

const DEBUG = process.argv.includes('--debug')
const APPLE_MUSIC = 'https://music.apple.com/'

let win = null
/** Queued commands that arrived before the hook was ready. */
let pending = []
let hookReady = false

// ---------------------------------------------------------------------------
// Protocol
// ---------------------------------------------------------------------------

function send(msg) {
  process.stdout.write(JSON.stringify(msg) + '\n')
}

function log(...args) {
  // stderr, never stdout — see the header.
  process.stderr.write('[sidecar] ' + args.join(' ') + '\n')
}

function fail(code, detail) {
  log('ERROR', code, detail || '')
  send({ event: 'error', code, detail: String(detail || '') })
}

// ---------------------------------------------------------------------------
// Window
// ---------------------------------------------------------------------------

async function createWindow() {
  win = new BrowserWindow({
    show: DEBUG,
    width: 1100,
    height: 760,
    // No menu bar, no chrome — on the rare occasion this is visible it is
    // Apple's login and nothing else.
    autoHideMenuBar: true,
    webPreferences: {
      preload: path.join(__dirname, 'preload.js'),
      partition: 'persist:tonearm',

      // The hook must touch the page's own `MusicKit` global, which means the
      // preload has to share the page's world. This is the same trade every
      // Apple Music wrapper makes. We compensate by pinning navigation to
      // Apple origins below and refusing every permission request.
      contextIsolation: false,
      nodeIntegration: false,
      sandbox: false,

      // CRITICAL: Chromium throttles timers and media in windows it believes
      // are hidden. Our window is *always* hidden. Without this, playback
      // stutters or stops the moment focus moves elsewhere.
      backgroundThrottling: false,
    },
  })

  // The page never navigates itself anywhere but Apple, and never opens
  // windows. Anything else (a support link, an ad) goes to the real browser.
  const allowed = (url) => {
    try {
      const h = new URL(url).hostname
      return h.endsWith('apple.com') || h.endsWith('mzstatic.com')
    } catch {
      return false
    }
  }

  win.webContents.on('will-navigate', (e, url) => {
    if (!allowed(url)) {
      e.preventDefault()
      shell.openExternal(url)
    }
  })

  win.webContents.setWindowOpenHandler(({ url }) => {
    // Apple's sign-in uses a popup for 2FA in some flows; keep those in-window
    // by allowing apple.com, and push everything else to the browser.
    if (allowed(url)) return { action: 'allow' }
    shell.openExternal(url)
    return { action: 'deny' }
  })

  // We need audio and nothing else. Deny the rest outright.
  win.webContents.session.setPermissionRequestHandler((_wc, permission, cb) => {
    cb(permission === 'media')
  })

  win.on('close', (e) => {
    // Closing the login window must not kill playback; Rust owns our lifetime.
    e.preventDefault()
    win.hide()
    send({ event: 'window-hidden' })
  })

  await win.loadURL(APPLE_MUSIC)
  log('loaded', APPLE_MUSIC)
}

// ---------------------------------------------------------------------------
// Commands from Rust
// ---------------------------------------------------------------------------

function dispatch(msg) {
  // Handled here, not in the page.
  switch (msg.cmd) {
    case 'showLogin':
      if (win) {
        win.show()
        win.focus()
      }
      return
    case 'hide':
      if (win) win.hide()
      return
    case 'quit':
      app.exit(0)
      return
  }

  if (!hookReady) {
    pending.push(msg)
    return
  }
  win.webContents.send('tonearm:command', msg)
}

function drainPending() {
  const queued = pending
  pending = []
  for (const msg of queued) win.webContents.send('tonearm:command', msg)
}

// ---------------------------------------------------------------------------
// Boot
// ---------------------------------------------------------------------------

app.whenReady().then(async () => {
  try {
    // castLabs ECS: the Widevine CDM arrives through Chromium's component
    // updater, so this can take a moment on first run and needs network.
    // Creating a window before it resolves means EME is simply absent.
    await components.whenReady()
    log('widevine ready:', JSON.stringify(components.status()))
    send({ event: 'widevine-ready' })
  } catch (err) {
    fail('widevine-unavailable', err)
    // No CDM means no playback, ever. Say so and exit rather than pretend.
    app.exit(1)
    return
  }

  // The renderer talks back through the same channel name in both directions.
  const { ipcMain } = require('electron')

  ipcMain.on('tonearm:event', (_e, ev) => {
    if (ev && ev.event === 'hook-ready') {
      hookReady = true
      drainPending()
    }
    send(ev)
  })

  await createWindow()

  const rl = readline.createInterface({ input: process.stdin })
  rl.on('line', (line) => {
    if (!line.trim()) return
    let msg
    try {
      msg = JSON.parse(line)
    } catch (err) {
      fail('bad-command', err)
      return
    }
    try {
      dispatch(msg)
    } catch (err) {
      fail('dispatch-failed', err)
    }
  })

  // Rust closing our stdin is the shutdown signal.
  rl.on('close', () => app.exit(0))

  send({ event: 'ready', debug: DEBUG })
})

// The whole point is to outlive a closed window.
app.on('window-all-closed', () => {})

process.on('uncaughtException', (err) => fail('uncaught', err && err.stack))
