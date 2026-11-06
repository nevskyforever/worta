import assert from 'node:assert/strict'
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { chromium } from '@playwright/test'
import { seedSourceAccount } from './replay-source-seed-fixture.mjs'

const origin = new URL(process.argv[2]).origin, backend = new URL(process.argv[3]).origin
assert.equal(new URL(origin).hostname, '127.0.0.1'); assert.equal(new URL(backend).hostname, '127.0.0.1')
const directory = await mkdtemp(join(tmpdir(), 'worta-note-updates-'))
const checks = [], requests = [], errors = []
let context, phase = 'setup', fault = null
async function launch() {
  context = await chromium.launchPersistentContext(directory, { headless: true })
  await context.route(url => url.pathname.startsWith('/api/'), async route => {
    const request = route.request(), url = new URL(request.url())
    if (phase === 'owner' && !url.pathname.startsWith('/api/v1/auth/')) requests.push({ method: request.method(), path: url.pathname })
    if (phase === 'owner' && fault === 'offline') return route.abort('failed')
    const response = await route.fetch({ url: backend + url.pathname + url.search })
    await route.fulfill({ response })
  })
  const page = await context.newPage(); page.on('pageerror', error => errors.push(error.message))
  await page.goto(`${origin}/scripts/browser-cloud-account-harness.html`); await page.waitForFunction(() => !!window.harness)
  return page
}
async function login(page, register = false, username = 'NoteUpdates') {
  phase = 'setup'
  await page.evaluate(async username => {
    const { BrowserCloudAccountSession } = await import('/src/platform/web/cloudAccountSession.ts')
    window.reader = new BrowserCloudAccountSession()
    await reader.login(username, 'correct horse battery staple')
    await reader.unlock('encryption-password')
  }, username)
  if (register) await page.evaluate(() => reader.registerCloudReadDevice())
  phase = 'owner'
}
const apply = page => page.evaluate(async () => {
  try { return await reader.applyCloudInitialNotes(new AbortController().signal) }
  catch (error) { return { error: error.code ?? error.message } }
})
async function cache(page) {
  return page.evaluate(async () => {
    const request = indexedDB.open('worta-web-replay', 8)
    await new Promise((resolve, reject) => { request.onsuccess = resolve; request.onerror = () => reject(request.error) })
    const db = request.result, rows = {}
    try { await new Promise((resolve, reject) => {
      const tx = db.transaction([...db.objectStoreNames], 'readonly')
      for (const name of db.objectStoreNames) tx.objectStore(name).getAll().onsuccess = event => { rows[name] = event.target.result }
      tx.oncomplete = resolve; tx.onabort = () => reject(tx.error)
    }); return { version: db.version, rows } } finally { db.close() }
  })
}
async function putRow(page, store, row) {
  await page.evaluate(async ({ store, row }) => {
    const request = indexedDB.open('worta-web-replay', 8)
    await new Promise((resolve, reject) => { request.onsuccess = resolve; request.onerror = () => reject(request.error) })
    const db = request.result
    try { await new Promise((resolve, reject) => {
      const tx = db.transaction(store, 'readwrite'); tx.objectStore(store).put(row)
      tx.oncomplete = resolve; tx.onabort = () => reject(tx.error)
    }) } finally { db.close() }
  }, { store, row })
}
async function opened(page, projects) {
  return page.evaluate(async projects => {
    const { BrowserLocalAuthority } = await import('/src/platform/web/localAuthority.ts')
    const { runBrowserTransaction } = await import('/src/platform/web/persistence.ts')
    const identity = reader.currentIdentity, scope = { accountId: identity.local_account_id, deviceId: identity.device_id,
      canonicalUserId: reader.auth.requireContext().userId }
    const request = indexedDB.open('worta-web-replay', 8)
    await new Promise((resolve, reject) => { request.onsuccess = resolve; request.onerror = () => reject(request.error) })
    const db = request.result
    const transaction = (_scope, stores, mode, schedule) => runBrowserTransaction(db, stores, mode, (tx, result, abort) => schedule(tx, result, abort))
    try {
      const local = new BrowserLocalAuthority(transaction).bind(reader.auth, reader.bindings, reader.keys), result = []
      for (const project of projects) {
        const source = await local.readSnapshot(scope, project.project_id)
        result.push(await local.withDecryptedSnapshot(scope, source.token, async value => ({ revision: value.source_revision,
          notes: value.notes.map(note => ({ id: note.note.id, content: note.note.content })) })))
      }
      return result
    } finally { db.close() }
  }, projects)
}
try {
  let page = await launch(); await page.evaluate(() => harness.createRecords(true))
  const fixture = await seedSourceAccount(page, 'NoteUpdates', { secondProjectNote: true, metadataCount: 2,
    stage: true, stageOrder: true, postInitialUpdates: [0, 0, 1, 2] })
  await login(page, true)
  const developer = await page.evaluate(async () => { const request = indexedDB.open('worta-web'); await new Promise((resolve, reject) => {
    request.onsuccess = resolve; request.onerror = () => reject(request.error)
  }); const version = request.result.version; request.result.close(); return version })
  assert.equal(developer, 12)
  const quota = await page.evaluate(async () => {
    const original = IDBObjectStore.prototype.put
    IDBObjectStore.prototype.put = function (value, ...args) {
      if (this.name === 'localNotes' && value.note_id === 'initial-0' && value.source_revision === 4)
        throw new DOMException('fixture', 'QuotaExceededError')
      return original.call(this, value, ...args)
    }
    try { return await reader.applyCloudInitialNotes(new AbortController().signal) }
    catch (error) { return { error: error.code ?? error.message } }
    finally { IDBObjectStore.prototype.put = original }
  })
  assert.equal(quota.error, 'quota_exceeded')
  const partial = await cache(page)
  assert.equal(partial.rows.replayManifest[0].applied_cursor, fixture.count - fixture.updates.length)
  assert.equal(partial.rows.inbox.filter(row => row.state === 'note_proven').length, 3)
  assert.equal(partial.rows.legacyNoteHistory.length, 6)
  checks.push('update-quota-rolls-back-source-history-proof')
  await context.close(); page = await launch(); await login(page)
  assert.deepEqual(await cache(page), partial); checks.push('restart-before-first-update')
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
  const afterFirst = await cache(page)
  assert.equal(afterFirst.rows.replayManifest[0].applied_cursor, fixture.updates[0].server_sequence)
  assert.equal(afterFirst.rows.inbox.filter(row => row.state === 'note_proven').length, 4)
  checks.push('lost-update-commit-response-retains-genuine-proof')
  await context.close(); page = await launch(); await login(page)
  assert.deepEqual(await cache(page), afterFirst); checks.push('restart-between-updates')
  const result = await apply(page)
  assert.ok(!result.error, JSON.stringify(result))
  assert.equal(result.projectOpened, false)
  const after = await cache(page)
  assert.equal(after.version, 8)
  assert.equal(after.rows.replayManifest[0].applied_cursor, fixture.count)
  assert.equal(after.rows.inbox.filter(row => row.state === 'note_proven').length, 7)
  assert.equal(after.rows.legacyNoteHistory.length, 10)
  assert.equal(after.rows.localNotes.length, 3)
  assert.equal(after.rows.inbox.filter(row => row.state === 'applied').length, 2)
  assert.equal(after.rows.inbox.filter(row => row.state === 'stage_proven').length, 1)
  assert.equal(after.rows.inbox.filter(row => row.state === 'stage_order_proven').length, 1)
  for (const update of fixture.updates) {
    const row = after.rows.inbox.find(value => value.event_id === update.event_id)
    assert.equal(row.state, 'note_proven')
    assert.equal(row.note_proof.header.parent_event_id, update.parent_event_id)
    assert.equal(row.note_proof.header.revision, update.revision)
  }
  const values = await opened(page, fixture.projects)
  assert.deepEqual(values.map(value => value.revision), [6, 3])
  assert.deepEqual(values.flatMap(value => value.notes.map(note => note.content)).sort(), [
    'PRIVATE_SEED_UPDATED_CONTENT_0_3', 'PRIVATE_SEED_UPDATED_CONTENT_1_2', 'PRIVATE_SEED_UPDATED_CONTENT_2_2'])
  assert.ok(!JSON.stringify(after).includes('PRIVATE_SEED_CONTENT_'))
  assert.ok(!JSON.stringify(after).includes('PRIVATE_SEED_UPDATED_CONTENT_'))
  checks.push('linear-update-chain-two-projects-three-notes', 'metadata-stage-order-proofs-retained', 'final-encrypted-note-content')
  for (const kind of ['source', 'history', 'proof']) {
    const store = kind === 'source' ? 'localProjects' : kind === 'history' ? 'legacyNoteHistory' : 'inbox'
    const row = structuredClone(kind === 'source' ? after.rows.localProjects[0]
      : kind === 'history' ? after.rows.legacyNoteHistory.find(value => value.record_kind === 'candidate' && value.header.revision === 2)
        : after.rows.inbox.find(value => value.state === 'note_proven' && value.note_proof.header.revision === 2))
    const damaged = structuredClone(row)
    if (kind === 'source') damaged.envelope.ciphertext = (damaged.envelope.ciphertext[0] === 'A' ? 'B' : 'A') + damaged.envelope.ciphertext.slice(1)
    if (kind === 'history') damaged.record_digest = '0'.repeat(64)
    if (kind === 'proof') damaged.note_proof.outcome_digest = '0'.repeat(64)
    await putRow(page, store, damaged)
    const bad = await cache(page)
    assert.ok((await apply(page)).error)
    assert.deepEqual(await cache(page), bad)
    await putRow(page, store, row)
    checks.push(`damaged-update-${kind}-refused`)
  }
  assert.ok(!(await apply(page)).error)
  assert.deepEqual(await cache(page), after); checks.push('exact-replay-no-duplicate')
  const changedWire = structuredClone(after.rows.inbox.find(row => row.state === 'note_proven' && row.note_proof.header.revision === 2))
  const wire = JSON.parse(changedWire.wire_item)
  wire.object.ciphertext = (wire.object.ciphertext[0] === 'A' ? 'B' : 'A') + wire.object.ciphertext.slice(1)
  changedWire.wire_item = JSON.stringify(wire)
  await putRow(page, 'inbox', changedWire)
  const damagedWire = await cache(page)
  assert.ok((await apply(page)).error)
  assert.deepEqual(await cache(page), damagedWire)
  await putRow(page, 'inbox', after.rows.inbox.find(row => row.event_id === changedWire.event_id))
  checks.push('changed-server-wire-refused')
  await context.close(); page = await launch(); await login(page)
  assert.ok(!(await apply(page)).error)
  assert.deepEqual(await cache(page), after)
  assert.deepEqual(await opened(page, fixture.projects), values)
  checks.push('restart-after-all-updates-reauthenticates')
  await page.evaluate(() => reader.lock())
  assert.ok((await apply(page)).error); assert.deepEqual(await cache(page), after)
  await page.evaluate(() => reader.unlock('encryption-password'))
  checks.push('amk-lock-refuses-without-write')
  await page.evaluate(() => reader.dispose())
  assert.ok((await apply(page)).error); assert.deepEqual(await cache(page), after)
  await login(page)
  assert.ok(!(await apply(page)).error); assert.deepEqual(await cache(page), after)
  checks.push('logout-and-reauthorization')
  fault = 'offline'; assert.ok((await apply(page)).error); assert.deepEqual(await cache(page), after)
  checks.push('offline-refused-no-write')
  fault = null
  for (const kind of ['wrong_parent', 'revision_gap', 'fork']) {
    await page.evaluate(() => reader.dispose())
    phase = 'setup'
    const username = { wrong_parent: 'NoteUpdatesWrongParent', revision_gap: 'NoteUpdatesRevisionGap', fork: 'NoteUpdatesFork' }[kind]
    const badFixture = await seedSourceAccount(page, username, { metadataCount: 2, stage: true, stageOrder: true,
      postInitialUpdates: [0], malformedUpdate: kind })
    await login(page, true, username)
    const identity = await page.evaluate(() => reader.currentIdentity)
    const rejected = await apply(page)
    assert.ok(rejected.error, JSON.stringify(rejected))
    const stopped = await cache(page)
    const manifest = stopped.rows.replayManifest.find(row => row.account_id === identity.local_account_id)
    const inbound = stopped.rows.inbox.filter(row => row.account_id === identity.local_account_id)
    assert.equal(manifest.applied_cursor, badFixture.count - 1)
    assert.equal(inbound.at(-1).event_id, badFixture.malformed.event_id)
    assert.equal(inbound.at(-1).state, 'received')
    assert.equal(inbound.filter(row => row.state === 'note_proven').length, 3)
    const correct = await opened(page, [badFixture.projects[0]])
    assert.equal(correct[0].notes.find(note => note.id === 'initial-0').content, 'PRIVATE_SEED_UPDATED_CONTENT_0_2')
    assert.ok((await apply(page)).error)
    assert.deepEqual(await cache(page), stopped)
    checks.push(`genuine-${kind}-refuses-without-false-proof`)
  }
  assert.ok(requests.every(request => request.method === 'GET'), JSON.stringify(requests.filter(request => request.method !== 'GET')))
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ pass: true, scenarios: checks.length, checks, noteProofs: 7, readerWrites: 0 }))
} finally { await context?.close().catch(() => undefined); await rm(directory, { recursive: true, force: true }) }
