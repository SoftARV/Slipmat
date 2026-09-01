// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

const { randomUUID } = require('node:crypto')

const IDENTITY_KEY = /(id|identifier|uuid)$|^playback/i
const SENSITIVE_KEY = /(token|secret|credential|authorization)/i

function propertyNames(value) {
  if (!value || (typeof value !== 'object' && typeof value !== 'function')) return []
  try {
    return Object.getOwnPropertyNames(value).sort()
  } catch {
    return []
  }
}

function prototypeNames(value) {
  try {
    return propertyNames(Object.getPrototypeOf(value)).filter((key) => key !== 'constructor')
  } catch {
    return []
  }
}

function scalarIdentity(item, keys) {
  const identity = {}
  for (const key of keys) {
    if (!IDENTITY_KEY.test(key) || SENSITIVE_KEY.test(key)) continue
    try {
      const value = item[key]
      if (['string', 'number', 'boolean'].includes(typeof value)) identity[key] = value
      if (typeof value === 'bigint') identity[key] = String(value)
    } catch {
      // A throwing getter is evidence about MusicKit's shape, not a probe failure.
    }
  }
  return identity
}

function createOccurrenceId(generation = randomUUID()) {
  const references = new WeakMap()
  let nextReference = 1

  return (item) => {
    if (!item || (typeof item !== 'object' && typeof item !== 'function')) return null
    let reference = references.get(item)
    if (!reference) {
      reference = `${generation}:${nextReference++}`
      references.set(item, reference)
    }
    return reference
  }
}

function createQueueIdentityProbe(occurrenceId = createOccurrenceId()) {
  const shapes = new Map()
  let nextShape = 1

  return (items) => Array.from(items || [], (item, index) => {
    const ownKeys = propertyNames(item)
    const prototypeKeys = prototypeNames(item)
    const keys = [...new Set([...ownKeys, ...prototypeKeys])].sort()
    const signature = JSON.stringify([ownKeys, prototypeKeys])
    let shape = shapes.get(signature)
    const newShape = !shape
    if (!shape) {
      shape = nextShape++
      shapes.set(signature, shape)
    }

    const entry = {
      index,
      reference: occurrenceId(item),
      shape,
      identity: scalarIdentity(item, keys),
    }
    if (newShape) {
      entry.ownKeys = ownKeys
      entry.prototypeKeys = prototypeKeys
    }
    return entry
  })
}

module.exports = { createOccurrenceId, createQueueIdentityProbe }
