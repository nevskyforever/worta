import assert from 'node:assert/strict'
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { chromium } from '@playwright/test'
import { seedSourceAccount } from './replay-source-seed-fixture.mjs'
import { replayV3Fixture } from './replay-v1-fixture.mjs'

const origin = new URL(process.argv[2]).origin, backend = new URL(process.argv[3]).origin
assert.equal(new URL(origin).hostname, '127.0.0.1'); assert.equal(new URL(backend).hostname, '127.0.0.1')
const directory = await mkdtemp(join(tmpdir(), 'worta-source-seed-')), frozen = await replayV3Fixture()
const checks = [], requests = [], errors = []
let context, phase = 'setup', fault = null
async function launch() {
  context = await chromium.launchPersistentContext(directory, { headless: true })
  await context.route(url => url.pathname.startsWith('/api/'), async route => {
    const request = route.request(), url = new URL(request.url())
    if (phase === 'seed' && !url.pathname.startsWith('/api/v1/auth/')) requests.push({ path: url.pathname, method: request.method(), body: request.postData() })
    if (phase === 'seed' && fault === 'offline') return route.abort('failed')
    const response = await route.fetch({ url: backend + url.pathname + url.search })
    if (phase === 'seed' && fault === 'registry_count' && url.pathname === '/api/v1/cloud/projects/bootstrap') {
      const value = await response.json(); value.projects[0].initial_event_count++; return route.fulfill({ response, json: value })
    }
    if (phase === 'seed' && fault === 'registry_origin' && url.pathname === '/api/v1/cloud/projects/bootstrap') {
      const value = await response.json(); value.projects[0].origin_device_id = crypto.randomUUID(); return route.fulfill({ response, json: value })
    }
    if (phase === 'seed' && fault === 'wire' && url.pathname === '/api/v3/sync/encrypted/pull') {
      const value = await response.json(); if (value.items.length) value.items[0].object.ciphertext = (value.items[0].object.ciphertext[0] === 'A' ? 'B' : 'A') + value.items[0].object.ciphertext.slice(1)
      return route.fulfill({ response, json: value })
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
    window.reader = new BrowserCloudAccountSession(); await reader.login('SourceSeed', 'correct horse battery staple')
    await reader.unlock('encryption-password'); if (register) await reader.registerCloudReadDevice()
    return reader.currentIdentity
  }, register)
}
async function cache(page, database = 'worta-web-replay') {
  return page.evaluate(async database => {
    const open = indexedDB.open(database); await new Promise((resolve, reject) => { open.onsuccess = resolve; open.onerror = () => reject(open.error) })
    const db = open.result, rows = {}, version = db.version
    try { await new Promise((resolve, reject) => {
      const tx = db.transaction([...db.objectStoreNames], 'readonly')
      for (const name of db.objectStoreNames) { const read = tx.objectStore(name).getAll(); read.onsuccess = () => { rows[name] = read.result } }
      tx.oncomplete = resolve; tx.onabort = () => reject(tx.error)
    }); return { version, rows } } finally { db.close() }
  }, database)
}
const seed = (page, project) => page.evaluate(async project => {
  try { return await reader.seedCloudNoteSource(project, new AbortController().signal) }
  catch (error) { return { error: error.code ?? error.message } }
}, project)
try {
  let page = await launch(); await page.evaluate(() => harness.createRecords(true))
  const fixture = await seedSourceAccount(page, 'SourceSeed'), identity = await login(page, true)
  const legacy = await cache(page, 'worta-web'); assert.equal(legacy.version, 12)
  await page.evaluate(async ({ identity, url }) => {
    const login = await fetch('/api/v1/auth/login', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ username: 'SourceSeed', password: 'correct horse battery staple' }) })
    const token = (await login.json()).access_token, headers = { Authorization: `Bearer ${token}` }
    const get = async path => { const response = await fetch(path, { headers }); if (!response.ok) throw Error(`fixture_${response.status}`); return response.json() }
    const me = await get('/api/v1/account/me'), registry = await get('/api/v1/cloud/projects/bootstrap'), transport = await get('/api/v2/sync/encrypted/capabilities')
    const { openBrowserReplayStorage } = await import(/* @vite-ignore */ url)
    const scope = { accountId: identity.local_account_id, deviceId: identity.device_id, canonicalUserId: me.id }
    const store = await openBrowserReplayStorage(scope, new AbortController().signal, () => true)
    try {
      let snapshot = await store.start({ registry, transport }), since = 0, more = true
      while (more) {
        const response = await get(`/api/v3/sync/encrypted/pull?device_id=${scope.deviceId}&since=${since}&limit=100&protocol_version=3&encrypted_sync_version=3`)
        snapshot = await store.commitPage(snapshot, since, JSON.stringify(response)); since = response.next_cursor; more = response.has_more
      }
      await store.finish(snapshot)
    } finally { await store.close() }
  }, { identity, url: frozen.storage })
  const v3 = await cache(page); assert.equal(v3.version, 3); assert.equal(v3.rows.inbox.length, fixture.count)
  for (const kind of ['quota', 'abort']) {
    const failure = await page.evaluate(async ({ identity, kind, user }) => {
      const original = IDBObjectStore.prototype.put
      IDBObjectStore.prototype.put = function (value, ...args) {
        if (this.name === 'replayManifest' && value.record_version === 6) {
          if (kind === 'quota') throw new DOMException('fixture', 'QuotaExceededError')
          this.transaction.abort(); throw new DOMException('fixture', 'AbortError')
        }
        return original.call(this, value, ...args)
      }
      try { const { openBrowserReplayStorage } = await import('/src/platform/web/replayStorage.ts')
        const store = await openBrowserReplayStorage({ accountId: identity.local_account_id, deviceId: identity.device_id,
          canonicalUserId: user }, new AbortController().signal, () => true); await store.close(); return null }
      catch (error) { return error.code }
      finally { IDBObjectStore.prototype.put = original }
    }, { identity, kind, user: v3.rows.identities[0].canonical_user_id })
    assert.equal(failure, kind === 'quota' ? 'quota_exceeded' : 'storage_failed')
    assert.deepEqual(await cache(page), v3); checks.push(`v3-v6-${kind}-migration-rollback`)
  }
  assert.equal((await page.evaluate(() => reader.receiveCloudAccountHistory(new AbortController().signal))).ownerProofs, 0)
  const before = await cache(page); assert.equal(before.version, 10)
  const migrated = structuredClone(before)
  migrated.rows.replayManifest = migrated.rows.replayManifest.map(value => ({ ...value, record_version: 6, contract_version: 6, source_seeds: [] }))
  assert.deepEqual(before, migrated); checks.push('genuine-v3-to-v6-migration-preserves-all-ciphertext')
  phase = 'seed'
  for (const kind of ['quota', 'abort']) {
    const failure = await page.evaluate(async ({ project, kind }) => {
      const original = IDBObjectStore.prototype.add
      IDBObjectStore.prototype.add = function (value, ...args) {
        if (this.name === 'localProjects') { if (kind === 'quota') throw new DOMException('fixture', 'QuotaExceededError'); this.transaction.abort(); throw new DOMException('fixture', 'AbortError') }
        return original.call(this, value, ...args)
      }
      try { await reader.seedCloudNoteSource(project, new AbortController().signal); return null }
      catch (error) { return error.code }
      finally { IDBObjectStore.prototype.add = original }
    }, { project: fixture.projects[0].project_id, kind })
    assert.equal(failure, kind === 'quota' ? 'quota_exceeded' : 'storage_failed')
    assert.deepEqual(await cache(page), before); checks.push(`${kind}-atomic-no-source-or-reference`)
  }
  const first = await seed(page, fixture.projects[0].project_id)
  assert.deepEqual(first, { kind: 'encrypted_note_source_seed', status: 'seeded', projectOpened: false,
    ready: false, declarationAllowed: false, writerAuthorized: false })
  const seeded = await cache(page), manifest = seeded.rows.replayManifest[0]
  assert.equal(seeded.version, 10); assert.equal(seeded.rows.localProjects.length, 1)
  assert.equal(seeded.rows.localNotes.length, 0); assert.equal(seeded.rows.legacyNoteHistory.length, 0)
  assert.equal(manifest.source_seeds.length, 1); assert.equal(manifest.source_seeds[0].initial_event_count, 2)
  assert.equal(manifest.source_seeds[0].initial_max_server_sequence, 2)
  assert.equal(manifest.applied_cursor, 0); assert.equal(manifest.metadata_cursor, 0)
  assert.ok(seeded.rows.inbox.every(row => row.state === 'received'))
  assert.ok(!JSON.stringify(seeded).includes('PRIVATE_SEED_CONTENT_'))
  assert.ok(!JSON.stringify(seeded).includes('PRIVATE_SEED_PROJECT_'))
  assert.deepEqual(await cache(page, 'worta-web'), legacy)
  checks.push('genuine-legacy-notes-before-metadata-genesis', 'encrypted-empty-source-and-cohort-reference', 'no-note-or-metadata-proof', 'developer-v12-unchanged')
  assert.equal((await seed(page, fixture.projects[0].project_id)).status, 'already_seeded')
  assert.deepEqual(await cache(page), seeded); checks.push('idempotent-reauthentication-no-write')
  for (const kind of ['registry_count', 'registry_origin', 'wire', 'offline']) {
    fault = kind; assert.ok((await seed(page, fixture.projects[1].project_id)).error, kind)
    assert.deepEqual(await cache(page), seeded); checks.push(`${kind}-no-false-seed`)
  }
  fault = null
  assert.equal((await seed(page, fixture.projects[1].project_id)).status, 'seeded')
  const both = await cache(page); assert.equal(both.rows.localProjects.length, 2); assert.equal(both.rows.replayManifest[0].source_seeds.length, 2)
  assert.equal(both.rows.replayManifest[0].applied_cursor, 0); checks.push('second-clean-project-same-account')
  const old = await page.evaluate(async ({ scope, url }) => {
    try { const { openBrowserReplayStorage } = await import(/* @vite-ignore */ url)
      const store = await openBrowserReplayStorage(scope, new AbortController().signal, () => true); await store.close(); return null }
    catch (error) { return error.code }
  }, { scope: { accountId: identity.local_account_id, deviceId: identity.device_id, canonicalUserId: both.rows.identities[0].canonical_user_id }, url: frozen.storage })
  assert.equal(old, 'unsupported_version'); assert.deepEqual(await cache(page), both); checks.push('genuine-v3-client-refuses-v6')
  assert.ok(requests.every(request => request.method === 'GET'), JSON.stringify(requests.filter(request => request.method !== 'GET')))
  checks.push('reader-only-http-no-ack-push-capability')
  await context.close(); page = await launch(); await login(page)
  phase = 'seed'
  assert.equal((await seed(page, fixture.projects[0].project_id)).status, 'already_seeded')
  assert.equal((await seed(page, fixture.projects[1].project_id)).status, 'already_seeded')
  assert.deepEqual(await cache(page), both); checks.push('persistent-chromium-restart-server-and-amk-reauthentication')
  const corrupt = structuredClone(both.rows.localProjects[0]); corrupt.envelope.ciphertext = (corrupt.envelope.ciphertext[0] === 'A' ? 'B' : 'A') + corrupt.envelope.ciphertext.slice(1)
  await page.evaluate(async corrupt => { const open = indexedDB.open('worta-web-replay'); await new Promise(resolve => { open.onsuccess = resolve })
    const db = open.result, tx = db.transaction('localProjects', 'readwrite'); tx.objectStore('localProjects').put(corrupt)
    await new Promise(resolve => { tx.oncomplete = resolve }); db.close() }, corrupt)
  const damaged = await cache(page)
  assert.ok((await seed(page, fixture.projects[0].project_id)).error)
  assert.deepEqual(await cache(page), damaged); checks.push('corrupt-source-ciphertext-refuses-without-repair')
  assert.deepEqual(await cache(page, 'worta-web'), legacy)
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ pass: true, scenarios: checks.length, checks, events: fixture.count,
    sourceRows: both.rows.localProjects.length, readerWrites: requests.filter(request => request.method !== 'GET').length }))
} finally {
  await context?.close().catch(() => undefined); await frozen.close(); await rm(directory, { recursive: true, force: true })
}
