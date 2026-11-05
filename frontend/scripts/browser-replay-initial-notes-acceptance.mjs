import assert from 'node:assert/strict'
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { chromium } from '@playwright/test'
import { seedSourceAccount } from './replay-source-seed-fixture.mjs'

const origin = new URL(process.argv[2]).origin, backend = new URL(process.argv[3]).origin
assert.equal(new URL(origin).hostname, '127.0.0.1'); assert.equal(new URL(backend).hostname, '127.0.0.1')
const directory = await mkdtemp(join(tmpdir(), 'worta-initial-notes-'))
const checks = [], requests = [], errors = []
let context, phase = 'setup', fault = null
async function launch() {
  context = await chromium.launchPersistentContext(directory, { headless: true })
  await context.route(url => url.pathname.startsWith('/api/'), async route => {
    const request = route.request(), url = new URL(request.url())
    if (phase === 'owner' && !url.pathname.startsWith('/api/v1/auth/')) requests.push({ method: request.method(), path: url.pathname })
    if (phase === 'owner' && fault === 'offline') return route.abort('failed')
    const response = await route.fetch({ url: backend + url.pathname + url.search })
    if (phase === 'owner' && fault === 'registry' && url.pathname === '/api/v1/cloud/projects/bootstrap') {
      const value = await response.json(); value.current_cursor++; return route.fulfill({ response, json: value })
    }
    await route.fulfill({ response })
  })
  const page = await context.newPage(); page.on('pageerror', error => errors.push(error.message))
  await page.goto(`${origin}/scripts/browser-cloud-account-harness.html`); await page.waitForFunction(() => !!window.harness)
  return page
}
async function login(page, register = false) {
  phase = 'setup'
  return page.evaluate(async register => {
    const { BrowserCloudAccountSession } = await import('/src/platform/web/cloudAccountSession.ts')
    window.reader = new BrowserCloudAccountSession()
    await reader.login('InitialNotes', 'correct horse battery staple')
    await reader.unlock('encryption-password')
    if (register) await reader.registerCloudReadDevice()
    return reader.currentIdentity
  }, register)
}
async function cache(page, database = 'worta-web-replay') {
  return page.evaluate(async database => {
    const request = indexedDB.open(database)
    await new Promise((resolve, reject) => { request.onsuccess = resolve; request.onerror = () => reject(request.error) })
    const db = request.result, rows = {}, version = db.version
    try { await new Promise((resolve, reject) => {
      const tx = db.transaction([...db.objectStoreNames], 'readonly')
      for (const name of db.objectStoreNames) tx.objectStore(name).getAll().onsuccess = event => { rows[name] = event.target.result }
      tx.oncomplete = resolve; tx.onabort = () => reject(tx.error)
    }); return { version, rows } } finally { db.close() }
  }, database)
}
const apply = page => page.evaluate(async () => { try { return await reader.applyCloudInitialNotes(new AbortController().signal) } catch (error) { return { error: error.code ?? error.message } } })
async function putRow(page, store, row) {
  await page.evaluate(async ({ store, row }) => {
    const request = indexedDB.open('worta-web-replay', 7)
    await new Promise((resolve, reject) => { request.onsuccess = resolve; request.onerror = () => reject(request.error) })
    const db = request.result
    try { await new Promise((resolve, reject) => {
      const tx = db.transaction(store, 'readwrite'); tx.objectStore(store).put(row)
      tx.oncomplete = resolve; tx.onabort = () => reject(tx.error)
    }) } finally { db.close() }
  }, { store, row })
}
async function deleteInbox(page, row) {
  await page.evaluate(async row => {
    const request = indexedDB.open('worta-web-replay', 7)
    await new Promise((resolve, reject) => { request.onsuccess = resolve; request.onerror = () => reject(request.error) })
    const db = request.result
    try { await new Promise((resolve, reject) => {
      const tx = db.transaction('inbox', 'readwrite')
      tx.objectStore('inbox').delete([row.account_id, row.server_sequence])
      tx.oncomplete = resolve; tx.onabort = () => reject(tx.error)
    }) } finally { db.close() }
  }, row)
}
async function opened(page, projects) {
  return page.evaluate(async projects => {
    const { BrowserLocalAuthority } = await import('/src/platform/web/localAuthority.ts')
    const { runBrowserTransaction } = await import('/src/platform/web/persistence.ts')
    const identity = reader.currentIdentity, scope = { accountId: identity.local_account_id, deviceId: identity.device_id,
      canonicalUserId: reader.auth.requireContext().userId }
    const request = indexedDB.open('worta-web-replay', 7)
    await new Promise((resolve, reject) => { request.onsuccess = resolve; request.onerror = () => reject(request.error) })
    const db = request.result
    const transaction = (_scope, stores, mode, schedule) => runBrowserTransaction(db, stores, mode, (tx, result, abort) => schedule(tx, result, abort))
    try {
      const local = new BrowserLocalAuthority(transaction).bind(reader.auth, reader.bindings, reader.keys), result = []
      for (const project of projects) {
        const source = await local.readSnapshot(scope, project.project_id)
        result.push(await local.withDecryptedSnapshot(scope, source.token, async value => ({ project: value.project.name,
          revision: value.source_revision, notes: value.notes.map(note => ({ id: note.note.id, title: note.note.title, content: note.note.content })) })))
      }
      return result
    } finally { db.close() }
  }, projects)
}
try {
  let page = await launch(); await page.evaluate(() => harness.createRecords(true))
  const fixture = await seedSourceAccount(page, 'InitialNotes', { secondProjectNote: true, metadataCount: 2, stage: true, stageOrder: true })
  await login(page, true)
  const legacy = await cache(page, 'worta-web')
  phase = 'owner'
  const receipt = await page.evaluate(() => reader.receiveCloudAccountHistory(new AbortController().signal))
  assert.equal(receipt.ownerProofs, 0)
  assert.equal((await cache(page)).rows.inbox.length, fixture.count)
  const quota = await page.evaluate(async () => {
    const original = IDBObjectStore.prototype.put
    IDBObjectStore.prototype.put = function (value, ...args) {
      if (this.name === 'localNotes' && value.note_id === 'initial-1') throw new DOMException('fixture', 'QuotaExceededError')
      return original.call(this, value, ...args)
    }
    try { await reader.applyCloudInitialNotes(new AbortController().signal); return null }
    catch (error) { return error.code ?? error.message }
    finally { IDBObjectStore.prototype.put = original }
  })
  assert.equal(quota, 'quota_exceeded')
  const partial = await cache(page)
  assert.equal(partial.rows.replayManifest[0].applied_cursor, 1)
  assert.equal(partial.rows.inbox.filter(row => row.state === 'note_proven').length, 1)
  assert.equal(partial.rows.localNotes.length, 1)
  assert.equal(partial.rows.legacyNoteHistory.length, 2)
  checks.push('quota-rolls-back-second-note-source-history-proof')
  const second = partial.rows.inbox[1]
  await deleteInbox(page, second)
  let bad = await cache(page)
  assert.ok((await apply(page)).error)
  assert.deepEqual(await cache(page), bad)
  await putRow(page, 'inbox', second)
  checks.push('missing-second-event-no-cursor-or-proof')
  for (const kind of ['ciphertext', 'device', 'revision', 'note_id']) {
    const changed = structuredClone(second), wire = JSON.parse(changed.wire_item)
    if (kind === 'ciphertext') wire.object.ciphertext = (wire.object.ciphertext[0] === 'A' ? 'B' : 'A') + wire.object.ciphertext.slice(1)
    if (kind === 'device') wire.event.device_id = crypto.randomUUID()
    if (kind === 'revision') wire.event.revision = 2
    if (kind === 'note_id') wire.event.entity_id = 'wrong-note-id'
    changed.wire_item = JSON.stringify(wire)
    await putRow(page, 'inbox', changed)
    bad = await cache(page)
    assert.ok((await apply(page)).error)
    assert.deepEqual(await cache(page), bad)
    await putRow(page, 'inbox', second)
    checks.push(`tampered-${kind}-no-false-proof`)
  }
  fault = 'registry'
  assert.ok((await apply(page)).error)
  assert.deepEqual(await cache(page), partial)
  fault = null; checks.push('changed-server-registry-no-partial-write')
  const revoked = await page.evaluate(async () => {
    const original = IDBObjectStore.prototype.put
    IDBObjectStore.prototype.put = function (value, ...args) {
      if (this.name === 'localNotes' && value.note_id === 'initial-1') void reader.lock()
      return original.call(this, value, ...args)
    }
    try { await reader.applyCloudInitialNotes(new AbortController().signal); return null }
    catch (error) { return error.code ?? error.message }
    finally { IDBObjectStore.prototype.put = original }
  })
  assert.ok(revoked)
  assert.deepEqual(await cache(page), partial)
  checks.push('mid-commit-amk-revocation-rolls-back-note-and-proof')
  await page.evaluate(() => reader.unlock('encryption-password'))
  phase = 'owner'
  await page.evaluate(() => reader.lock())
  assert.ok((await apply(page)).error)
  assert.deepEqual(await cache(page), partial)
  await page.evaluate(() => reader.unlock('encryption-password'))
  phase = 'owner'; checks.push('amk-lock-no-late-proof')
  await context.close(); page = await launch(); await login(page)
  phase = 'owner'
  assert.deepEqual(await cache(page), partial)
  checks.push('chromium-restart-between-initial-notes')
  const lost = await page.evaluate(async () => {
    const { BrowserLegacyNoteApplyRuntime } = await import('/src/platform/web/legacyNoteApply.ts')
    const original = BrowserLegacyNoteApplyRuntime.prototype.commit
    BrowserLegacyNoteApplyRuntime.prototype.commit = async function (...args) {
      const outcome = await original.apply(this, args)
      if (outcome === 'applied') throw Error('lost_commit_response')
      return outcome
    }
    try { await reader.applyCloudInitialNotes(new AbortController().signal); return null }
    catch (error) { return error.code ?? error.message }
    finally { BrowserLegacyNoteApplyRuntime.prototype.commit = original }
  })
  assert.ok(lost)
  const committed = await cache(page)
  assert.equal(committed.rows.replayManifest[0].applied_cursor, 2)
  assert.equal(committed.rows.localNotes.length, 2)
  checks.push('lost-response-retains-second-real-note-proof')
  const result = await apply(page)
  assert.ok(!result.error, JSON.stringify(result))
  assert.equal(result.projectOpened, false)
  const after = await cache(page)
  assert.equal(after.version, 7)
  assert.equal(after.rows.inbox.filter(row => row.state === 'note_proven').length, 3)
  assert.equal(after.rows.inbox.filter(row => row.state === 'applied').length, 2)
  assert.equal(after.rows.inbox.filter(row => row.state === 'stage_proven').length, 1)
  assert.equal(after.rows.inbox.filter(row => row.state === 'stage_order_proven').length, 1)
  assert.equal(after.rows.replayManifest[0].applied_cursor, fixture.count)
  assert.equal(after.rows.localNotes.length, 3)
  assert.equal(after.rows.legacyNoteHistory.length, 6)
  assert.equal(after.rows.projectBindings.length, 2)
  assert.deepEqual(after.rows.inbox.map(row => row.server_sequence), Array.from({ length: fixture.count }, (_, i) => i + 1))
  const values = await opened(page, fixture.projects)
  assert.equal(values[0].revision, 3); assert.equal(values[1].revision, 2)
  assert.deepEqual(values.flatMap(value => value.notes.map(note => note.content)).sort(),
    ['PRIVATE_SEED_CONTENT_0', 'PRIVATE_SEED_CONTENT_1', 'PRIVATE_SEED_CONTENT_2'])
  assert.ok(!JSON.stringify(after).includes('PRIVATE_SEED_CONTENT_'))
  assert.ok(!JSON.stringify(after).includes('PRIVATE_SEED_PROJECT_'))
  assert.deepEqual(await cache(page, 'worta-web'), legacy)
  checks.push('genuine-notes-before-metadata-stage-and-order', 'three-encrypted-notes-two-projects', 'real-note-history-and-proofs', 'readonly-source-exact-text', 'developer-v12-unchanged')
  for (const project of fixture.projects) {
    const seed = await page.evaluate(async id => reader.seedCloudNoteSource(id, new AbortController().signal), project.project_id)
    assert.equal(seed.status, 'already_seeded')
  }
  assert.deepEqual(await cache(page), after); checks.push('seed-reauthentication-after-genuine-note-apply')
  for (const kind of ['source', 'history', 'proof']) {
    const store = kind === 'source' ? 'localProjects' : kind === 'history' ? 'legacyNoteHistory' : 'inbox'
    const row = structuredClone(kind === 'source' ? after.rows.localProjects[0]
      : kind === 'history' ? after.rows.legacyNoteHistory.find(value => value.record_kind === 'candidate')
        : after.rows.inbox.find(value => value.state === 'note_proven'))
    const damaged = structuredClone(row)
    if (kind === 'source') damaged.envelope.ciphertext = (damaged.envelope.ciphertext[0] === 'A' ? 'B' : 'A') + damaged.envelope.ciphertext.slice(1)
    if (kind === 'history') damaged.record_digest = '0'.repeat(64)
    if (kind === 'proof') damaged.note_proof.outcome_digest = '0'.repeat(64)
    await putRow(page, store, damaged)
    const bad = await cache(page)
    assert.ok((await apply(page)).error)
    assert.deepEqual(await cache(page), bad)
    await putRow(page, store, row)
    assert.deepEqual(await cache(page), after)
    checks.push(`damaged-${kind}-refuses-without-repair`)
  }
  assert.ok(!((await apply(page)).error))
  assert.deepEqual(await cache(page), after); checks.push('exact-replay-no-duplicate-write')
  await context.close(); page = await launch(); await login(page)
  phase = 'owner'
  assert.ok(!((await apply(page)).error))
  assert.deepEqual(await cache(page), after)
  assert.deepEqual(await opened(page, fixture.projects), values)
  checks.push('real-chromium-restart-server-amk-proof-reauthentication')
  fault = 'offline'; assert.ok((await apply(page)).error); assert.deepEqual(await cache(page), after)
  checks.push('restart-offline-refusal-no-write')
  assert.ok(requests.every(request => request.method === 'GET'), JSON.stringify(requests.filter(request => request.method !== 'GET')))
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ pass: true, scenarios: checks.length, checks, noteProofs: 3,
    structuralProofs: 4, readerWrites: requests.filter(request => request.method !== 'GET').length }))
} finally { await context?.close().catch(() => undefined); await rm(directory, { recursive: true, force: true }) }
