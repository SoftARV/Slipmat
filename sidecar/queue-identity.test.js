// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

const test = require('node:test')
const assert = require('node:assert/strict')

const { createOccurrenceId, createQueueIdentityProbe } = require('./queue-identity')

test('assigns one process-local id to each MusicKit queue object', () => {
  const occurrenceId = createOccurrenceId('generation-a')
  const first = { id: 'song-a' }
  const second = { id: 'song-a' }

  assert.equal(occurrenceId(first), occurrenceId(first))
  assert.notEqual(occurrenceId(first), occurrenceId(second))
})

test('does not reuse ids after a preload context is replaced', () => {
  const beforeNavigation = createOccurrenceId('generation-a')
  const afterNavigation = createOccurrenceId('generation-b')

  assert.notEqual(beforeNavigation({}), afterNavigation({}))
})

test('labels duplicate queue objects separately and preserves surviving references', () => {
  const probe = createQueueIdentityProbe()
  const first = { id: 'song-a', playbackId: 'playback-a' }
  const second = { id: 'song-a', playbackId: 'playback-a' }

  const before = probe([first, second])
  const after = probe([second])

  assert.notEqual(before[0].reference, before[1].reference)
  assert.equal(after[0].reference, before[1].reference)
})

test('reports identity-like scalar fields without exposing tokens or nested values', () => {
  const probe = createQueueIdentityProbe()
  const item = {
    id: 'song-a',
    queueItemId: 'occurrence-7',
    licenseKey: 'must-not-leak',
    secretToken: 'must-not-leak',
    metadata: { id: 'nested-id' },
  }

  const [entry] = probe([item])

  assert.deepEqual(entry.identity, {
    id: 'song-a',
    queueItemId: 'occurrence-7',
  })
})

test('assigns a new label when MusicKit replaces an object wrapper', () => {
  const probe = createQueueIdentityProbe()
  const before = probe([{ id: 'song-a' }])
  const after = probe([{ id: 'song-a' }])

  assert.notEqual(after[0].reference, before[0].reference)
})

test('survives an opaque MusicKit object whose prototype cannot be inspected', () => {
  const probe = createQueueIdentityProbe()
  const item = new Proxy({ id: 'song-a' }, {
    getPrototypeOf() {
      throw new Error('opaque')
    },
  })

  assert.doesNotThrow(() => probe([item]))
})

test('reports each object shape once instead of repeating keys for the whole queue', () => {
  const probe = createQueueIdentityProbe()
  const entries = probe([{ id: 'song-a' }, { id: 'song-b' }])

  assert.equal(entries[0].shape, entries[1].shape)
  assert.deepEqual(entries[0].ownKeys, ['id'])
  assert.equal(entries[1].ownKeys, undefined)
})
