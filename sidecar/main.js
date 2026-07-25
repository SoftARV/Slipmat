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

const { app, components, BrowserWindow, powerSaveBlocker, shell } = require('electron')
const path = require('node:path')
const readline = require('node:readline')

// `--debug`, or TONEARM_SHOW_SIDECAR=1 from the Rust side. Both keep the
// window on screen. This is the fastest way to tell a frozen renderer from a
// broken command: if playback works with the window visible and not without,
// the problem is Chromium freezing a page it thinks nobody is looking at.
const DEBUG =
  process.argv.includes('--debug') || process.env.TONEARM_SHOW_SIDECAR === '1'
const APPLE_MUSIC = 'https://music.apple.com/'
const READY_TIMEOUT_MS = 60_000
const PROBE_INTERVAL_MS = 500
/// How many times to re-ask for tokens after wiring. The developer token can
/// land just after MusicKit; ten seconds is plenty and then it stops.
const TOKEN_NUDGES = 10

// These four switches are what make a permanently-hidden window actually work.
// All must be set before app.whenReady(). Do not remove any of them without
// re-running the standalone sidecar test — each one was added to fix an
// observed, silent failure.
//
//   autoplay-policy
//     Chromium refuses to start audio until a page has "user activation" — a
//     real click inside it. Our window is hidden and driven entirely over IPC,
//     so it NEVER receives one, and MusicKit's play() resolves without
//     producing sound.
//
//   disable-renderer-backgrounding / disable-background-timer-throttling /
//   disable-backgrounding-occluded-windows
//     A window created with show:false counts as hidden AND occluded, so
//     Chromium freezes its renderer: setTimeout stops firing within a second or
//     two and the page stops making progress. webPreferences.backgroundThrottling
//     alone does NOT cover this. Observed symptom: the MusicKit readiness poll
//     emitted exactly one tick and then went silent for 90s — no hook-ready and
//     no hook-failed, because the loop that would report either had frozen.
app.commandLine.appendSwitch('autoplay-policy', 'no-user-gesture-required')
app.commandLine.appendSwitch('disable-renderer-backgrounding')
app.commandLine.appendSwitch('disable-background-timer-throttling')
app.commandLine.appendSwitch('disable-backgrounding-occluded-windows')

let win = null
/** Queued commands that arrived before the hook was ready. */
let pending = []
let hookReady = false
let suspensionBlocker = null
let probeTimer = null
let tokenTimer = null

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

  // Signing in navigates the page, which tears down the old preload context
  // and builds a new one. Until that new hook reports in, `music` is null over
  // there — so commands must go back to the pending queue rather than being
  // forwarded into a context that will throw. Without this, every command sent
  // between sign-in and the new hook-ready is silently lost.
  // ONLY a real cross-document navigation in the main frame invalidates the
  // hook. `did-start-loading` is the wrong event and was a serious bug: it
  // fires for SPA route changes and subresource loads too, so on
  // music.apple.com it flipped hookReady to false within seconds and it never
  // came back — `hook-ready` is emitted once per document, not once per load.
  // Every command from Rust after that was queued forever, with no error
  // anywhere. Symptom: refreshTokens (sent straight from main.js, bypassing
  // dispatch) kept working while setQueue vanished.
  win.webContents.on('did-start-navigation', (...args) => {
    // Electron has changed this signature across versions: older releases pass
    // (event, url, isInPlace, isMainFrame, …), newer ones a single details
    // object. Accept both rather than silently reading undefined.
    const first = args[0]
    const d =
      first && typeof first === 'object' && 'isMainFrame' in first
        ? first
        : { isMainFrame: args[3], isSameDocument: args[2] }

    if (d.isMainFrame && !d.isSameDocument) {
      log('main-frame navigation — hook invalidated, re-probing')
      hookReady = false
      // Re-arm the probe. The new document gets a fresh preload which will
      // self-poll, but the probe is the backstop that guarantees a re-wire —
      // otherwise an invalidated hook could never recover and every later
      // command would queue forever.
      probeForMusicKit()
    }
  })

  win.on('close', (e) => {
    // Closing the login window must not kill playback; Rust owns our lifetime.
    // Back to concealed rather than hidden, so the renderer keeps running.
    e.preventDefault()
    conceal()
    send({ event: 'window-hidden' })
  })

  await win.loadURL(APPLE_MUSIC)
  log('loaded', APPLE_MUSIC)
  if (!DEBUG) conceal()
  probeForMusicKit()
}

/// Make the window invisible to the user but *mapped* as far as Chromium is
/// concerned.
///
/// This is the load-bearing trick of the whole sidecar. A window that is never
/// shown has its renderer frozen: page timers stop, and even
/// executeJavaScript() from the main process never resolves. Nothing you can
/// pass in webPreferences or on the command line prevents it — the page has to
/// actually be on screen.
///
/// So it *is* on screen: fully transparent, click-through, off the taskbar, one
/// pixel. The user never sees it and can never interact with it, but the
/// compositor has it mapped, so the renderer stays alive and decodes audio.
///
/// Do NOT "fix" this back to win.hide() — that reintroduces the freeze, and the
/// symptom is a player that goes silent with no error anywhere.
function conceal() {
  // Tell the OS this process must not be suspended. On its own this does not
  // stop Chromium's per-page freezing, but without it a laptop on battery can
  // suspend the whole sidecar mid-track.
  if (suspensionBlocker === null) {
    suspensionBlocker = powerSaveBlocker.start('prevent-app-suspension')
  }
  win.setOpacity(0)
  win.setIgnoreMouseEvents(true)
  win.setSkipTaskbar(true)
  win.setSize(1, 1)
  win.showInactive()
}

/// The inverse, for Apple's sign-in — the one time the user sees this window.
function reveal() {
  win.setOpacity(1)
  win.setIgnoreMouseEvents(false)
  win.setSkipTaskbar(false)
  win.setSize(1100, 760)
  win.center()
  win.show()
  win.focus()
}

/// Poll the renderer until MusicKit exists, then tell the preload to wire up.
///
/// This runs in the MAIN process on purpose. A show:false window has its
/// renderer frozen by Chromium — a setTimeout loop inside the page fires once
/// and then stops — so readiness cannot be detected from in there.
/// executeJavaScript still runs, so we drive it from out here.
function probeForMusicKit() {
  // Probes and token nudges are module-level and always cleared first.
  // They used to be per-call locals, so every re-probe (one per main-frame
  // navigation) leaked another probe AND another 10-shot token nudger — which
  // is why refreshTokens arrived several times a second, forever, instead of
  // ten times at startup.
  clearInterval(probeTimer)
  clearInterval(tokenTimer)

  const deadline = Date.now() + READY_TIMEOUT_MS
  let wired = false

  probeTimer = setInterval(async () => {
    if (wired || !win || win.isDestroyed()) return clearInterval(probeTimer)

    // Deadline is checked BEFORE the await on purpose. If the renderer is
    // frozen, executeJavaScript never settles — and a deadline check placed
    // after the await would then be unreachable, which is exactly how the
    // freeze first presented: no hook-ready, no hook-failed, no error at all.
    if (Date.now() > deadline) {
      clearInterval(probeTimer)
      return send({
        event: 'hook-failed',
        detail: 'MusicKit never appeared on music.apple.com',
      })
    }

    let ready = false
    try {
      ready = await win.webContents.executeJavaScript(
        'window.__tonearmReady ? window.__tonearmReady() : false',
      )
    } catch (err) {
      log('probe failed:', err && err.message)
    }

    if (ready) {
      wired = true
      clearInterval(probeTimer)
      win.webContents.send('tonearm:wire')
      // The developer token can appear slightly after MusicKit, so nudge a few
      // times. `tokenTimer` is module-level and cleared at the top of this
      // function — a `const` here shadowed it, so the nudger was unstoppable
      // and every re-probe added another one.
      let nudges = 0
      tokenTimer = setInterval(() => {
        if (++nudges > TOKEN_NUDGES || !win || win.isDestroyed()) {
          return clearInterval(tokenTimer)
        }
        win.webContents.send('tonearm:command', { cmd: 'refreshTokens' })
      }, 1000)
    }
  }, PROBE_INTERVAL_MS)
}

// ---------------------------------------------------------------------------
// Commands from Rust
// ---------------------------------------------------------------------------

function dispatch(msg) {
  // Handled here, not in the page.
  switch (msg.cmd) {
    case 'showLogin':
      if (win) reveal()
      return
    case 'hide':
      // conceal(), never win.hide() — see the note on conceal().
      if (win) conceal()
      return
    case 'quit':
      app.exit(0)
      return
  }

  if (!hookReady) {
    // Never queue silently. A command that is waiting is indistinguishable
    // from one that was dropped unless it says so, and that ambiguity cost
    // three debugging rounds.
    pending.push(msg)
    log('queued (hook not ready):', msg.cmd, 'depth=', pending.length)
    send({ event: 'cmd-queued', cmd: msg.cmd, depth: pending.length })
    return
  }
  // `visible` is the diagnostic that matters when a command produces no
  // sound and no error: a window Chromium considers hidden has a frozen
  // renderer that will never run the handler.
  log('dispatch', msg.cmd, 'visible=', win.isVisible(), 'crashed=', win.webContents.isCrashed())
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
