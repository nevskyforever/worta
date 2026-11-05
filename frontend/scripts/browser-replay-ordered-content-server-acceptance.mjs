import assert from 'node:assert/strict'
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { chromium } from '@playwright/test'
import { seedSourceAccount } from './replay-source-seed-fixture.mjs'

const origin = new URL(process.argv[2]).origin, backend = new URL(process.argv[3]).origin
assert.equal(new URL(origin).hostname, '127.0.0.1'); assert.equal(new URL(backend).hostname, '127.0.0.1')
const directory = await mkdtemp(join(tmpdir(), 'worta-ordered-content-server-'))
const requests = [], errors = []
let context, phase = 'setup', fault = null
async function launch() {
  context = await chromium.launchPersistentContext(directory, { headless: true })
  await context.route(url => url.pathname.startsWith('/api/'), async route => {
    const request = route.request(), url = new URL(request.url())
    if (phase === 'owner' && !url.pathname.startsWith('/api/v1/auth/')) requests.push({ method: request.method(), path: url.pathname })
    if (phase === 'owner' && fault === 'interrupt' && url.pathname === '/api/v3/sync/encrypted/pull' && url.searchParams.get('since') === '100') return route.abort('failed')
    if (phase === 'owner' && fault === 'lock' && url.pathname === '/api/v3/sync/encrypted/pull' && url.searchParams.get('since') === '100') {
      await context.pages().at(-1).evaluate(() => reader.lock())
      return route.abort('failed')
    }
    const response = await route.fetch({ url: backend + url.pathname + url.search })
    if (phase === 'owner' && url.pathname === '/api/v3/sync/encrypted/pull') requests.push({ method: request.method(), path: url.pathname, status: response.status(), since: url.searchParams.get('since') })
    if (phase === 'owner' && fault === 'registry' && url.pathname === '/api/v1/cloud/projects/bootstrap') {
      const value = await response.json(); value.current_cursor++
      return route.fulfill({ response, json: value })
    }
    if (phase === 'owner' && fault === 'mode' && url.pathname === '/api/v2/sync/encrypted/capabilities') {
      const value = await response.json(); value.writer_transport_version = 2
      return route.fulfill({ response, json: value })
    }
    if (phase === 'owner' && ['gap', 'duplicate', 'order', 'wire'].includes(fault) && url.pathname === '/api/v3/sync/encrypted/pull' && url.searchParams.get('since') === '0') {
      const value = await response.json()
      if (fault === 'gap') value.items[1].event.server_sequence++
      if (fault === 'duplicate') value.items[1] = structuredClone(value.items[0])
      if (fault === 'order') [value.items[0], value.items[1]] = [value.items[1], value.items[0]]
      if (fault === 'wire') value.items[0].object.ciphertext = 'AA'
      return route.fulfill({ response, json: value })
    }
    await route.fulfill({ response })
  })
  const page = await context.newPage(); page.on('pageerror', error => errors.push(error.message))
  await page.goto(`${origin}/scripts/browser-cloud-account-harness.html`)
  await page.waitForFunction(() => !!window.harness)
  return page
}
const attempt = (page, method) => page.evaluate(async method => {
  try { return await reader[method](new AbortController().signal) }
  catch (error) { return { error: error.code ?? error.message } }
}, method)
const cache = page => page.evaluate(async () => {
  const request = indexedDB.open('worta-web-replay', 7)
  await new Promise((resolve, reject) => { request.onsuccess = resolve; request.onerror = () => reject(request.error) })
  const db = request.result, rows = {}
  try { await new Promise((resolve, reject) => {
    const tx = db.transaction([...db.objectStoreNames], 'readonly')
    for (const name of db.objectStoreNames) tx.objectStore(name).getAll().onsuccess = event => { rows[name] = event.target.result }
    tx.oncomplete = resolve; tx.onabort = () => reject(tx.error)
  }); return rows } finally { db.close() }
})
async function putInbox(page, row) {
  await page.evaluate(async row => {
    const request = indexedDB.open('worta-web-replay', 7)
    await new Promise((resolve, reject) => { request.onsuccess = resolve; request.onerror = () => reject(request.error) })
    const db = request.result
    try { await new Promise((resolve, reject) => {
      const tx = db.transaction('inbox', 'readwrite'); tx.objectStore('inbox').put(row)
      tx.oncomplete = resolve; tx.onabort = () => reject(tx.error)
    }) } finally { db.close() }
  }, row)
}
async function login(page, register = false) {
  phase = 'setup'
  await page.evaluate(async register => {
    const { BrowserCloudAccountSession } = await import('/src/platform/web/cloudAccountSession.ts')
    window.reader = new BrowserCloudAccountSession()
    await reader.login('OrderedContentServer', 'correct horse battery staple')
    await reader.unlock('encryption-password')
    if (register) await reader.registerCloudReadDevice()
  }, register)
  phase = 'owner'
}
try {
  let page = await launch(); await page.evaluate(() => harness.createRecords(true))
  const fixture = await seedSourceAccount(page, 'OrderedContentServer', { metadataCount: 102, stage: true, stageOrder: true, contentUpdate: true })
  assert.equal(fixture.content.server_sequence, fixture.count)
  await login(page, true)
  const refused = await attempt(page, 'receiveCloudAccountHistory')
  assert.equal(refused.error, 'content_note_reader_required')
  assert.ok(requests.some(row => row.path === '/api/v3/sync/encrypted/pull' && row.status === 409))
  assert.equal((await cache(page)).inbox.length, 0)

  // Isolated test account only: the existing server capability API qualifies its real registered recipient.
  phase = 'setup'
  const recipient = await page.evaluate(async () => {
    const login = await fetch('/api/v1/auth/login', { method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username: 'OrderedContentServer', password: 'correct horse battery staple' }) })
    if (!login.ok) throw Error('fixture_login')
    const token = (await login.json()).access_token, device = reader.currentIdentity.device_id
    const headers = { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' }
    for (const [path, method, body] of [
      ['/api/v3/sync/encrypted/reader-ready', 'POST', { device_id: device, reader_transport_version: 3 }],
      ['/api/v3/sync/encrypted/note-reader-capabilities', 'PUT', { device_id: device, reader_transport_version: 3,
        frame_version: 1, codec8_version: 1, compression_zero: true, ordinary_reader_version: 1, resolution_reader_version: 2 }],
    ]) {
      const response = await fetch(path, { method, headers, body: JSON.stringify(body) })
      if (!response.ok) throw Error(`fixture_capability_${response.status}`)
    }
    return device
  })
  phase = 'owner'
  fault = 'interrupt'; assert.ok((await attempt(page, 'receiveCloudAccountHistory')).error)
  fault = null
  const partial = await cache(page)
  assert.equal(partial.inbox.length, 100)
  assert.equal(partial.replayManifest[0].receive_cursor, 100)
  assert.equal(partial.replayManifest[0].applied_cursor, 0)
  await context.close(); page = await launch(); await login(page)
  assert.deepEqual(await cache(page), partial)
  for (const kind of ['gap', 'duplicate', 'order', 'wire', 'mode', 'lock']) {
    fault = kind
    assert.ok((await attempt(page, 'receiveCloudAccountHistory')).error, kind)
    assert.deepEqual(await cache(page), partial, kind)
    if (kind === 'lock') await page.evaluate(() => reader.unlock('encryption-password'))
  }
  fault = null
  const seed = await page.evaluate(async project => reader.seedCloudNoteSource(project, new AbortController().signal), fixture.projects[0].project_id)
  assert.equal(seed.status, 'seeded')
  const before = await cache(page)
  assert.equal(before.replayManifest[0].applied_cursor, 0)
  assert.equal(before.inbox.length, fixture.count)
  assert.equal(before.inbox.at(-1).state, 'received')
  assert.deepEqual(before.inbox.map(row => row.server_sequence), Array.from({ length: fixture.count }, (_, index) => index + 1))
  assert.ok(requests.some(row => row.status === 200 && row.since === '0'))
  assert.ok(requests.some(row => row.status === 200 && row.since === '100'))
  const original = before.inbox.at(-1), damaged = structuredClone(original), wire = JSON.parse(damaged.wire_item)
  wire.object.ciphertext = 'AA'; damaged.wire_item = JSON.stringify(wire)
  await putInbox(page, damaged)
  assert.ok((await attempt(page, 'applyCloudOrderedContentUpdate')).error)
  assert.equal((await cache(page)).replayManifest[0].applied_cursor, 0)
  await putInbox(page, original)
  const applied = await attempt(page, 'applyCloudOrderedContentUpdate')
  assert.equal(applied.kind, 'bounded_ordered_content_update_applied', JSON.stringify({ applied, cursor: (await cache(page)).replayManifest[0].applied_cursor }))
  assert.equal(applied.contentProofs, 1)
  const after = await cache(page)
  assert.equal(after.replayManifest[0].applied_cursor, fixture.count)
  assert.equal(after.inbox.at(-1).state, 'content_note_proven')
  assert.equal(after.inbox.filter(row => row.state === 'stage_proven').length, 1)
  assert.equal(after.inbox.filter(row => row.state === 'stage_order_proven').length, 1)
  assert.equal(after.inbox.filter(row => row.state === 'note_proven').length, 2)
  assert.ok(!JSON.stringify(after).includes('PRIVATE_CONTENT_UPDATE_BODY'))
  assert.equal((await attempt(page, 'applyCloudOrderedContentUpdate')).contentProofs, 1)
  assert.deepEqual(await cache(page), after)
  await context.close(); page = await launch(); await login(page)
  assert.equal((await attempt(page, 'applyCloudOrderedContentUpdate')).contentProofs, 1)
  assert.deepEqual(await cache(page), after)
  fault = 'registry'; assert.ok((await attempt(page, 'applyCloudOrderedContentUpdate')).error)
  assert.deepEqual(await cache(page), after)
  fault = null
  phase = 'setup'
  const appended = await page.evaluate(async ({ source, item }) => {
    const login = await fetch('/api/v1/auth/login', { method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username: 'OrderedContentServer', password: 'correct horse battery staple' }) })
    if (!login.ok) throw Error('fixture_login')
    const token = (await login.json()).access_token
    const pushed = await fetch('/api/v3/sync/encrypted/push', { method: 'POST',
      headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
      body: JSON.stringify({ protocol_version: 3, encrypted_sync_version: 3, device_id: source, items: [item] }) })
    if (!pushed.ok) throw Error(`fixture_append_${pushed.status}`)
    return (await pushed.json()).results[0].server_sequence
  }, { source: fixture.source, item: fixture.laterEvent })
  assert.equal(appended, fixture.count + 1)
  phase = 'owner'
  assert.ok((await attempt(page, 'applyCloudOrderedContentUpdate')).error)
  assert.deepEqual(await cache(page), after)
  assert.equal(errors.length, 0, errors.join('\n'))
  assert.ok(requests.some(row => row.path === '/api/v3/sync/encrypted/pull'))
  assert.equal(requests.some(row => row.method !== 'GET'), false)
  console.log(JSON.stringify({ pass: true, scenarios: 16, events: fixture.count, recipient, readerWrites: 0 }))
} finally { await context?.close(); await rm(directory, { recursive: true, force: true }) }
