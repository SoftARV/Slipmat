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

const {
  app,
  components,
  BrowserWindow,
  powerSaveBlocker,
  session,
  shell,
} = require('electron')
const path = require('node:path')
const readline = require('node:readline')

// TONEARM_SHOW_SIDECAR=1 keeps the window on screen. This is the fastest way
// to tell a frozen renderer from a broken command: if playback works with the
// window visible and not without, the problem is Chromium freezing a page it
// thinks nobody is looking at.
//
// **The env var, not a flag.** `npm run debug` used to pass `--debug`, which
// never reached here: Electron reads it as Node's long-deprecated `--debug`
// and exits before the app starts ("`node --debug` ... are invalid", make
// Error 9). So the one documented tool for isolating an Apple or DRM problem
// from a Rust one did not run at all. The argv check is kept because it costs
// nothing and still works if the flag is passed somewhere Electron ignores it.
const DEBUG =
  process.argv.includes('--debug') || process.env.TONEARM_SHOW_SIDECAR === '1'

/// Per-command logging. **Off by default, and that is a safety property rather
/// than tidiness.**
///
/// `log()` writes to stderr, which under a .desktop launch is journald, which
/// writes to disk synchronously. One line per command is fine at the handful
/// per minute a person generates — and is the single biggest amplifier when
/// something loops. A two-way binding on the volume button once emitted 5,721
/// commands and the machine had to be power-cycled; those disk writes are a
/// large part of why an app bug became a system one (#37).
///
/// `TONEARM_SIDECAR_TRACE=1` brings it back for the evening you need it. The
/// protocol events it duplicates — `cmd-recv`, `cmd-done`, `cmd-queued` — are
/// unaffected, and are what CLAUDE.md's "diagnose by layer" actually reads.
const TRACE = process.env.TONEARM_SIDECAR_TRACE === '1'
const APPLE_MUSIC = 'https://music.apple.com/'
/// Where the login lives. Named once because sign-out has to clear the very
/// partition the window was created with — two spellings of this string is a
/// sign-out that silently forgets nothing.
const PARTITION = 'persist:tonearm'
/// How the window stays out of the way. Set TONEARM_SIDECAR_WINDOW to override:
///
///   hidden     (default) never mapped. Completely invisible — nothing in the
///              window overview, nothing in the dash. Verified on GNOME/Wayland
///              with playback left running for a long stretch and still
///              responding afterwards.
///   concealed  mapped but 1x1, transparent and click-through. Kept as an
///              escape hatch in case a compositor does freeze the renderer of
///              an unmapped window; the cost is a speck in the overview.
///
/// Note the --disable-renderer-backgrounding family below was already in place
/// when `hidden` was verified, so those switches may well be what makes it
/// viable. Do not remove them and assume this still works.
const WINDOW_MODE = process.env.TONEARM_SIDECAR_WINDOW || 'hidden'

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
//     A window created with show:false counts as hidden AND occluded, and
//     Chromium will freeze such a renderer: timers stop firing and the page
//     stops making progress. webPreferences.backgroundThrottling alone does
//     NOT cover it. These three are almost certainly what lets the sidecar run
//     entirely unmapped (WINDOW_MODE=hidden), which is verified working — so
//     treat them as load-bearing, not as leftovers from a fixed bug.
// Identity. Without these the sidecar shows up in the dash and window list as
// a separate app called "tonearm-sidecar" (from package.json's name), with a
// generic icon. Pointing it at Tonearm's own desktop entry makes the shell
// treat any window it does show as part of Tonearm rather than a stray second
// app. Must be set before app.whenReady().
app.setName('Tonearm')
if (process.platform === 'linux') {
  app.setDesktopName('dev.miguelrincon.Tonearm.desktop')
}

// Chromium publishes its OWN MPRIS player as soon as a page plays media, and
// grabs the hardware media keys with it. Tonearm exports MPRIS itself (see
// src/mpris.rs), so leaving these on gives the shell two identical "Tonearm"
// players — the visible symptom — and lets an invisible Chromium win the race
// for Play/Pause on the keyboard.
//
// MediaSessionService is the MPRIS bridge; HardwareMediaKeyHandling is the key
// grab. Neither has anything to do with decoding audio, so disabling them costs
// nothing and leaves exactly one player on the bus: ours.
app.commandLine.appendSwitch(
  'disable-features',
  'MediaSessionService,HardwareMediaKeyHandling',
)

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
    // Constructor-time, not just setSkipTaskbar() — some shells only honour it
    // at map time, and by then the window has already been listed.
    skipTaskbar: true,
    // No menu bar, no chrome — on the rare occasion this is visible it is
    // Apple's login and nothing else.
    autoHideMenuBar: true,
    webPreferences: {
      preload: path.join(__dirname, 'preload.js'),
      partition: PARTITION,

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
    e.preventDefault()
    conceal()
    send({ event: 'window-hidden' })
  })

  await win.loadURL(APPLE_MUSIC)
  log('loaded', APPLE_MUSIC)
  if (!DEBUG) conceal()
  probeForMusicKit()
}

/// Put the window away. See WINDOW_MODE.
///
/// Default is a plain hide — genuinely unmapped, so it appears nowhere in the
/// shell. `concealed` is the fallback: mapped but 1x1, transparent and
/// click-through, for a compositor that freezes unmapped renderers.
function conceal() {
  // Tell the OS this process must not be suspended. On its own this does not
  // stop Chromium's per-page freezing, but without it a laptop on battery can
  // suspend the whole sidecar mid-track.
  if (suspensionBlocker === null) {
    suspensionBlocker = powerSaveBlocker.start('prevent-app-suspension')
  }

  if (WINDOW_MODE === 'hidden') {
    // Truly invisible: nothing in the overview, nothing in the dash.
    win.hide()
    log('window mode: hidden (not mapped)')
    return
  }

  win.setOpacity(0)
  win.setIgnoreMouseEvents(true)
  win.setSkipTaskbar(true)
  win.setSize(1, 1)
  win.showInactive()
  log('window mode: concealed (mapped, 1x1, transparent)')
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

    // Electron defers executeJavaScript until the page stops loading, and it
    // implements that by attaching a `did-stop-loading` listener per call. So
    // probing a still-loading document queues one listener per tick and trips
    // "MaxListenersExceededWarning: 11 did-stop-loading listeners added".
    //
    // Skipping the tick is the fix rather than raising maxListeners: there is
    // nothing to find on a document that has not finished loading, so those
    // calls were never going to answer anything. Deliberately checked *after*
    // the deadline above, so a page that never finishes still times out and
    // reports `hook-failed` instead of probing silently forever.
    if (win.webContents.isLoadingMainFrame()) return

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

/// Actually sign out: drop Apple's session, not just MusicKit's token.
///
/// `music.unauthorize()` was the whole of sign-out, and it is a MusicKit call —
/// it clears the Music User Token and nothing else. The login itself is an
/// ordinary browser session in the `persist:tonearm` partition, so it survived,
/// and signing back in silently reused it. Measured after a sign-out and a
/// sign-in: a `.idmsa.apple.com` cookie two days older than the others was
/// still there, i.e. the same Apple identity was being re-presented. A user who
/// signs out to recover a broken session could not.
///
/// Best-effort and unordered on purpose. `unauthorize` is a courtesy to
/// MusicKit — clearing the storage underneath it is what actually ends the
/// session, so nothing here waits on the renderer, which may be mid-navigation
/// or gone.
async function signOut() {
  if (win && !win.isDestroyed()) {
    try {
      win.webContents.send('tonearm:command', { cmd: 'unauthorize' })
    } catch (err) {
      log('unauthorize could not be delivered:', err && err.message)
    }
  }

  try {
    // Cookies and every web storage that can hold an identity. The HTTP cache
    // is deliberately *not* cleared: it holds Apple's static assets rather than
    // credentials, and dropping it would cost a slow re-fetch for no privacy
    // gain. The Widevine CDM is outside the partition entirely, so it is
    // untouched and does not re-download.
    await session.fromPartition(PARTITION).clearStorageData({
      storages: [
        'cookies',
        'localstorage',
        'sessionstorage',
        'indexdb',
        'websql',
        'serviceworkers',
        'cachestorage',
      ],
    })
    log('session cleared')
  } catch (err) {
    // Say so rather than reporting a sign-out that did not happen — this is the
    // failure the whole function exists to stop being silent.
    log('clearing the session FAILED:', err && err.message)
    send({ event: 'error', code: 'sign-out-failed', detail: String(err) })
    return
  }

  // Reload so the next sign-in starts from a document that never saw the old
  // account. Without this the page keeps running with its in-memory MusicKit
  // instance and looks signed in until something forces a navigation.
  if (win && !win.isDestroyed()) {
    hookReady = false
    try {
      await win.loadURL(APPLE_MUSIC)
      probeForMusicKit()
    } catch (err) {
      log('reload after sign-out failed:', err && err.message)
    }
  }
  send({ event: 'signed-out' })
}

function dispatch(msg) {
  // Handled here, not in the page.
  switch (msg.cmd) {
    case 'showLogin':
      if (win) reveal()
      return
    case 'hide':
      // Always conceal(), never win.hide() directly — conceal() is what
      // honours WINDOW_MODE.
      if (win) conceal()
      return
    case 'quit':
      app.exit(0)
      return
    case 'signOut':
      // Main process, not the page: `session.clearStorageData` is a
      // main-process API, and the page cannot delete the cookies that keep it
      // logged in. That is precisely why sign-out used to leave them behind.
      signOut()
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
  if (TRACE) {
    log('dispatch', msg.cmd, 'visible=', win.isVisible(), 'crashed=', win.webContents.isCrashed())
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
