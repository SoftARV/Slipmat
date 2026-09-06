// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

const assert = require('node:assert/strict')
const { EventEmitter } = require('node:events')
const fs = require('node:fs')
const path = require('node:path')
const test = require('node:test')
const vm = require('node:vm')

async function boot() {
  const blockers = new Set()
  let nextId = 0
  const ipcMain = new EventEmitter()
  let window
  class BrowserWindow extends EventEmitter {
    constructor() {
      super()
      window = this
      this.webContents = Object.assign(new EventEmitter(), {
        setWindowOpenHandler() {},
        session: { setPermissionRequestHandler() {} },
        send() {},
      })
    }
    async loadURL() {}
    hide() {}
  }
  const app = Object.assign(new EventEmitter(), {
    setName() {},
    setDesktopName() {},
    commandLine: { appendSwitch() {}, getSwitchValue() { return '' } },
    whenReady: () => Promise.resolve(),
  })
  const electron = {
    app, BrowserWindow, ipcMain,
    components: { whenReady: async () => {}, status: () => ({}) },
    powerSaveBlocker: {
      start(type) {
        assert.equal(type, 'prevent-app-suspension')
        blockers.add(++nextId)
        return nextId
      },
      stop(id) { assert.equal(blockers.delete(id), true) },
    },
  }
  vm.runInNewContext(fs.readFileSync(path.join(__dirname, 'main.js'), 'utf8'), {
    require: (name) => name === 'electron' ? electron
      : name === 'node:readline' ? { createInterface: () => new EventEmitter() }
        : require(name),
    __dirname,
    process: { argv: [], env: {}, platform: 'linux', stdout: { write() {} },
      stderr: { write() {} }, on() {} },
    setInterval() {}, clearInterval() {},
  })
  await new Promise(setImmediate)
  return { blockers, window, event: (ev) => ipcMain.emit('slipmat:event', {}, ev) }
}

test('sleep is inhibited only while playback needs it', async () => {
  const { blockers, window, event } = await boot()
  assert.equal(blockers.size, 0, 'hidden startup must allow sleep')
  for (const state of ['loading', 'playing', 'playing', 'seeking', 'waiting', 'stalled']) {
    event({ event: 'playbackState', state })
    assert.equal(blockers.size, 1, state)
  }
  for (const state of ['paused', 'stopped', 'ended', 'completed', 'none', 'unknown']) {
    event({ event: 'playbackState', state: 'playing' })
    event({ event: 'playbackState', state })
    assert.equal(blockers.size, 0, state)
  }
  event({ event: 'playbackState', state: 'playing' })
  window.webContents.emit('did-start-navigation', { isMainFrame: true, isSameDocument: true })
  assert.equal(blockers.size, 1, 'SPA navigation must preserve playback')
  window.webContents.emit('did-start-navigation', { isMainFrame: true, isSameDocument: false })
  assert.equal(blockers.size, 0, 'document replacement releases the inhibitor')
  event({ event: 'playbackState', state: 'playing' })
  event({ event: 'hook-failed' })
  assert.equal(blockers.size, 0, 'failed hook must allow sleep')
  event({ event: 'playbackState', state: 'playing' })
  window.webContents.emit('render-process-gone')
  assert.equal(blockers.size, 0, 'dead renderer must allow sleep')
})
