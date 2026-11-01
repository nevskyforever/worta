import assert from 'node:assert/strict'
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { chromium } from '@playwright/test'
import { replayV1Fixture } from './replay-v1-fixture.mjs'
import { seedMetadataAccount } from './replay-metadata-fixture.mjs'

const origin = new URL(process.argv[2]).origin, backend = new URL(process.argv[3]).origin
assert.equal(new URL(origin).hostname, '127.0.0.1'); assert.equal(new URL(backend).hostname, '127.0.0.1')
const directory = await mkdtemp(join(tmpdir(), 'worta-metadata-owner-')), frozen = await replayV1Fixture()
const checks = [], requests = [], errors = [], identities = []
let context, phase = 'setup', fault = null, registryReads = 0
async function launch() {
  context = await chromium.launchPersistentContext(directory, { headless: true })
  await context.route(url => url.pathname.startsWith('/api/'), async route => {
    const request = route.request(), url = new URL(request.url())
    if (phase === 'owner' && !url.pathname.startsWith('/api/v1/auth/')) requests.push({ path: url.pathname, method: request.method(), body: request.postData() })
    if (phase === 'owner' && fault === 'offline') return route.abort('failed')
    if (phase === 'owner' && fault === 'device_gate' && url.pathname === '/api/v3/sync/encrypted/pull' && Number(url.searchParams.get('since')) === 102) url.searchParams.set('device_id', crypto.randomUUID())
    const response = await route.fetch({ url: backend + url.pathname + url.search })
    // Fault injection affects negative cases only; positive data is real API output.
    if (phase === 'owner' && fault && url.pathname === '/api/v3/sync/encrypted/pull') {
      const value = await response.json()
      if (fault === 'gap') value.items.shift()
      if (fault === 'duplicate') value.items[1] = value.items[0]
      if (fault === 'short') { value.items.pop(); value.next_cursor--; value.has_more = false }
      if (fault === 'wire') value.items[0].object.ciphertext = value.items[0].object.ciphertext.replace(/^./, value.items[0].object.ciphertext[0] === 'A' ? 'B' : 'A')
      if (fault === 'family') value.items[0].event.entity_type = 'future_family'
      if (fault === 'version') value.protocol_version = 4
      if (['gap', 'duplicate', 'short', 'wire', 'family', 'version'].includes(fault)) return route.fulfill({ response, json: value })
    }
    if (phase === 'owner' && url.pathname === '/api/v1/cloud/projects/bootstrap') {
      registryReads++
      if (fault === 'registry' && registryReads >= 3) { const value = await response.json(); value.current_cursor++; return route.fulfill({ response, json: value }) }
    }
    await route.fulfill({ response })
  })
  const page = await context.newPage(); page.on('pageerror', error => errors.push(error.message))
  await page.goto(`${origin}/scripts/browser-cloud-account-harness.html`); await page.waitForFunction(() => !!window.harness)
  return page
}
async function login(page, username, register = false) {
  phase = 'setup'
  const result = await page.evaluate(async ({ username, register }) => {
    const { BrowserCloudAccountSession } = await import('/src/platform/web/cloudAccountSession.ts')
    const session = new BrowserCloudAccountSession(); window.receiver = session
    await session.login(username, 'correct horse battery staple')
    let locked; try { await session.applyCloudAccountMetadata(new AbortController().signal) } catch (e) { locked = e.code }
    await session.unlock('encryption-password'); if (register) await session.registerCloudReadDevice()
    return { identity: session.currentIdentity, locked }
  }, { username, register })
  identities.push(result.identity.device_id); return result
}
const apply = page => page.evaluate(async () => { try { return await receiver.applyCloudAccountMetadata(new AbortController().signal) } catch (e) { return { error: e.code ?? e.message } } })
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
async function openCurrent(page, scope) {
  return page.evaluate(async scope => {
    const { openBrowserReplayStorage } = await import('/src/platform/web/replayStorage.ts')
    try { const store = await openBrowserReplayStorage(scope, new AbortController().signal, () => true); try { await store.read() } finally { await store.close() }; return null }
    catch (e) { return e.code }
  }, scope)
}
try {
  let page = await launch()
  await page.evaluate(() => harness.createRecords(true))
  await seedMetadataAccount(page, 'Metadata')
  const initial = await login(page, 'Metadata', true), identity = initial.identity
  assert.equal(initial.locked, 'key_locked'); checks.push('locked-amk-no-owner-result')
  const beforeLegacy = await cache(page, 'worta-web')
  assert.equal(beforeLegacy.version, 12)
  // Real frozen v1 storage stages all real server wire before the versioned migration.
  const scope = await page.evaluate(async ({ identity, module }) => {
    const login = await fetch('/api/v1/auth/login', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ username: 'Metadata', password: 'correct horse battery staple' }) })
    const token = (await login.json()).access_token, headers = { Authorization: `Bearer ${token}` }
    const get = async path => { const r = await fetch(path, { headers }); if (!r.ok) throw Error(`seed_${r.status}`); return r.json() }
    const me = await get('/api/v1/account/me'), registry = await get('/api/v1/cloud/projects/bootstrap'), transport = await get('/api/v2/sync/encrypted/capabilities')
    const scope = { accountId: identity.local_account_id, deviceId: identity.device_id, canonicalUserId: me.id }
    const { openBrowserReplayStorage } = await import(/* @vite-ignore */ module)
    const store = await openBrowserReplayStorage(scope, new AbortController().signal, () => true)
    try {
      let snapshot = await store.start({ registry, transport }), since = 0, more = true
      while (more) {
        const value = await get(`/api/v3/sync/encrypted/pull?device_id=${identity.device_id}&since=${since}&limit=100&protocol_version=3&encrypted_sync_version=3`)
        snapshot = await store.commitPage(snapshot, since, JSON.stringify(value)); since = value.next_cursor; more = value.has_more
      }
      await store.finish(snapshot)
    } finally { await store.close() }
    return scope
  }, { identity, module: frozen.url })
  const v1 = await cache(page); assert.equal(v1.version, 1); assert.equal(v1.rows.inbox.length, 102)
  // Inject actual native versionchange failures: neither manifests nor db version may leak.
  for (const kind of ['quota', 'abort']) {
    const failure = await page.evaluate(async ({ scope, kind }) => {
      const original = IDBObjectStore.prototype.put
      IDBObjectStore.prototype.put = function (value, ...args) {
        if (this.name === 'replayManifest' && value.record_version === 6) {
          if (kind === 'quota') throw new DOMException('fixture quota', 'QuotaExceededError')
          this.transaction.abort(); throw new DOMException('fixture abort', 'AbortError')
        }
        return original.call(this, value, ...args)
      }
      try { const { openBrowserReplayStorage } = await import('/src/platform/web/replayStorage.ts'); const store = await openBrowserReplayStorage(scope, new AbortController().signal, () => true); await store.close(); return null }
      catch (e) { return e.code } finally { IDBObjectStore.prototype.put = original }
    }, { scope, kind })
    assert.equal(failure, kind === 'quota' ? 'quota_exceeded' : 'storage_failed'); assert.deepEqual(await cache(page), v1); checks.push(`v1-migration-${kind}-atomic-rollback`)
  }
  const badMigration = await page.evaluate(async scope => {
    const open = indexedDB.open('worta-web-replay', 1); await new Promise(resolve => { open.onsuccess = resolve }); const db = open.result
    const tx = db.transaction('inbox', 'readwrite'), get = tx.objectStore('inbox').get([scope.accountId, 1])
    get.onsuccess = () => tx.objectStore('inbox').put({ ...get.result, record_version: 99 })
    await new Promise(resolve => { tx.oncomplete = resolve }); db.close()
  }, scope)
  void badMigration
  const corrupted = await cache(page); assert.equal(await openCurrent(page, scope), 'corrupt_record'); assert.deepEqual(await cache(page), corrupted)
  await page.evaluate(async row => { const open = indexedDB.open('worta-web-replay', 1); await new Promise(resolve => { open.onsuccess = resolve }); const db = open.result, tx = db.transaction('inbox', 'readwrite'); tx.objectStore('inbox').put(row); await new Promise(resolve => { tx.oncomplete = resolve }); db.close() }, v1.rows.inbox[0])
  checks.push('corrupt-v1-migration-refuses-without-data-loss')
  assert.equal(await openCurrent(page, scope), null)
  const migrated = await cache(page); assert.equal(migrated.version, 6)
  const expected = structuredClone(v1); expected.version = 6
  expected.rows.replayManifest = expected.rows.replayManifest.map(value => ({ ...value, record_version: 6, contract_version: 6, metadata_cursor: 0, applied_cursor: 0, source_seeds: [] }))
  assert.deepEqual(migrated, expected); checks.push('v1-v6-only-manifest-change-ciphertext-preserved')
  assert.equal(await page.evaluate(async ({ scope, module }) => { try { const old = await import(/* @vite-ignore */ module); await old.openBrowserReplayStorage(scope, new AbortController().signal, () => true); return null } catch (e) { return e.code } }, { scope, module: frozen.url }), 'unsupported_version')
  assert.deepEqual(await cache(page), migrated); checks.push('genuine-v1-client-refuses-v6-without-writes')
  phase = 'owner'
  for (const kind of ['gap', 'duplicate', 'short', 'wire', 'family', 'version', 'registry', 'device_gate', 'offline']) {
    fault = kind; registryReads = 0
    assert.ok((await apply(page)).error, kind); assert.deepEqual(await cache(page), migrated, kind)
    checks.push(`negative-server-${kind}-no-owner-proof`)
  }
  fault = null
  // Full-inventory CAS and real native quota/abort reject all four related writes together.
  for (const kind of ['quota', 'abort', 'lock']) {
    const outcome = await page.evaluate(async kind => {
      const original = IDBObjectStore.prototype.put
      IDBObjectStore.prototype.put = function (value, ...args) {
        if (this.name === 'ownerHeads') { if (kind === 'lock') { void receiver.lock(); return original.call(this, value, ...args) }; if (kind === 'quota') throw new DOMException('quota', 'QuotaExceededError'); this.transaction.abort(); throw new DOMException('abort', 'AbortError') }
        return original.call(this, value, ...args)
      }
      try { await receiver.applyCloudAccountMetadata(new AbortController().signal); return null } catch (e) { return e.code } finally { IDBObjectStore.prototype.put = original }
    }, kind)
    assert.equal(outcome, kind === 'quota' ? 'quota_exceeded' : kind === 'lock' ? 'session_closed' : 'storage_failed'); assert.deepEqual(await cache(page), migrated); checks.push(`owner-${kind}-atomic-binding-head-proof-journal`)
    if (kind === 'lock') { await page.evaluate(() => receiver.logout()); await login(page, 'Metadata'); phase = 'owner' }
  }
  const stale = await page.evaluate(async () => {
    const original = window.fetch; let modes = 0
    window.fetch = async (...args) => {
      const response = await original(...args), url = new URL(args[0] instanceof Request ? args[0].url : String(args[0]), location.href)
      if (url.pathname === '/api/v2/sync/encrypted/capabilities' && ++modes === 3) {
        const open = indexedDB.open('worta-web-replay', 6); await new Promise(resolve => { open.onsuccess = resolve })
        const db = open.result, tx = db.transaction('replayManifest', 'readwrite'), store = tx.objectStore('replayManifest'), read = store.getAll()
        read.onsuccess = () => store.put({ ...read.result[0], generation: crypto.randomUUID() })
        await new Promise(resolve => { tx.oncomplete = resolve }); db.close()
      }
      return response
    }
    try { await receiver.applyCloudAccountMetadata(new AbortController().signal); return null } catch (e) { return e.code }
    finally { window.fetch = original }
  })
  assert.equal(stale, 'stale_authority')
  const raced = await cache(page); assert.deepEqual(raced.rows.ownerHeads, []); assert.deepEqual(raced.rows.projectBindings, []); assert.deepEqual(raced.rows.inbox, migrated.rows.inbox)
  await page.evaluate(async row => { const open = indexedDB.open('worta-web-replay', 6); await new Promise(resolve => { open.onsuccess = resolve }); const db = open.result, tx = db.transaction('replayManifest', 'readwrite'); tx.objectStore('replayManifest').put(row); await new Promise(resolve => { tx.oncomplete = resolve }); db.close() }, migrated.rows.replayManifest[0])
  assert.deepEqual(await cache(page), migrated); checks.push('complete-inventory-generation-cas-after-fresh-server-check')
  // Abort/key/account context cannot publish a late result, even after a read response.
  for (const kind of ['cancel', 'lock', 'logout', 'account']) {
    const outcome = await page.evaluate(async kind => {
      const original = window.fetch, controller = new AbortController(); let fired = false
      window.fetch = async (...args) => {
        const response = await original(...args), url = new URL(args[0] instanceof Request ? args[0].url : String(args[0]), location.href)
        if (!fired && url.pathname === '/api/v3/sync/encrypted/pull') {
          fired = true
          if (kind === 'cancel') controller.abort()
          else if (kind === 'lock') await receiver.lock()
          else if (kind === 'logout') await receiver.logout()
          else await receiver.login('MetadataOther', 'correct horse battery staple')
        }
        return response
      }
      try { await receiver.applyCloudAccountMetadata(controller.signal); return null } catch (e) { return e.code } finally { window.fetch = original }
    }, kind)
    assert.ok(outcome); assert.deepEqual(await cache(page), migrated); checks.push(`late-${kind}-no-success`)
    await page.evaluate(() => receiver.logout()); await login(page, 'Metadata'); phase = 'owner'
  }
  const competitor = await context.newPage(); await competitor.goto(`${origin}/scripts/browser-cloud-account-harness.html`); await competitor.waitForFunction(() => !!window.harness)
  const held = await page.evaluate(async scope => { const { openBrowserReplayStorage } = await import('/src/platform/web/replayStorage.ts'); window.held = await openBrowserReplayStorage(scope, new AbortController().signal, () => true) }, scope)
  void held
  assert.equal(await openCurrent(competitor, scope), 'writer_busy'); await page.evaluate(() => held.close()); await competitor.close(); checks.push('real-competing-tab-writer-lock')
  const lost = await page.evaluate(async () => {
    const original = IDBDatabase.prototype.transaction; let armed = true
    IDBDatabase.prototype.transaction = function (...args) {
      const tx = original.apply(this, args)
      if (this.name === 'worta-web-replay' && args[1] === 'readwrite') tx.addEventListener('complete', () => {
        // No put monkeypatch: lock only if the actual owner head/proof writes were scheduled.
        if (armed && window.ownerWritten) { armed = false; void receiver.lock() }
      })
      return tx
    }
    const put = IDBObjectStore.prototype.put; window.ownerWritten = false
    IDBObjectStore.prototype.put = function (...args) { if (this.name === 'ownerHeads') window.ownerWritten = true; return put.apply(this, args) }
    try { await receiver.applyCloudAccountMetadata(new AbortController().signal); return null } catch (e) { return e.code }
    finally { IDBDatabase.prototype.transaction = original; IDBObjectStore.prototype.put = put }
  })
  assert.ok(lost)
  const committed = await cache(page); assert.equal(committed.rows.ownerHeads.length, 1, `lost outcome: ${lost}`); assert.equal(committed.rows.inbox.filter(row => row.state === 'applied').length, 1)
  assert.equal(committed.rows.projectBindings.length, 1); assert.equal(committed.rows.replayManifest[0].metadata_cursor, 1)
  checks.push('lost-commit-response-real-atomic-owner-outcome')
  await context.close(); page = await launch(); assert.deepEqual((await login(page, 'Metadata')).identity, identity); phase = 'owner'
  const result = await apply(page); assert.equal(result.kind, 'metadata_owner_applied'); assert.equal(result.ownerProofs, 102); assert.equal(result.projects, 2); assert.equal(result.projectOpened, false)
  const complete = await cache(page)
  assert.equal(complete.rows.ownerHeads.length, 2); assert.equal(complete.rows.projectBindings.length, 2)
  assert.ok(complete.rows.ownerHeads.every(head => head.history.length === 51 && head.tip_event_ids.length === 1))
  for (const row of complete.rows.inbox) {
    assert.equal(row.state, 'applied'); const wire = JSON.parse(row.wire_item)
    const original = v1.rows.inbox.find(value => value.event_id === row.event_id)
    assert.equal(row.wire_item, original.wire_item)
    assert.deepEqual(row.applied_proof.reference, wire.event)
    assert.ok(complete.rows.ownerHeads.some(head => head.history.some(proof => JSON.stringify(proof) === JSON.stringify(row.applied_proof))))
  }
  assert.ok(complete.rows.cursors.every(row => row.ack_cursor === 0 && row.proven_prefix === 0))
  for (const name of ['outbox', 'receipts', 'localProjects', 'localNotes', 'legacyNoteHistory', 'legacyNoteIntents']) assert.deepEqual(complete.rows[name], [])
  assert.equal(JSON.stringify(complete).includes('PRIVATE_METADATA_'), false)
  assert.deepEqual(await cache(page, 'worta-web'), beforeLegacy)
  checks.push('genuine-create-linear-update-two-projects-102-events-two-pages', 'exact-owner-heads-inbox-proofs-original-wire', 'all-v12-stores-unchanged')
  await context.close(); page = await launch(); await login(page, 'Metadata'); phase = 'owner'
  const warm = await page.evaluate(async () => {
    const originals = {}
    for (const name of ['put', 'add', 'delete', 'clear']) {
      originals[name] = IDBObjectStore.prototype[name]
      IDBObjectStore.prototype[name] = function (...args) {
        if (this.transaction.db.name === 'worta-web-replay') throw Error('unexpected_replay_write_after_restart')
        return originals[name].apply(this, args)
      }
    }
    try { return await receiver.applyCloudAccountMetadata(new AbortController().signal) }
    finally { for (const [name, original] of Object.entries(originals)) IDBObjectStore.prototype[name] = original }
  })
  assert.deepEqual(warm, result); assert.deepEqual(await cache(page), complete)
  checks.push('full-chromium-restart-server-and-aead-proof-reauthentication-no-rewrite')
  // Matching forged head/inbox proof DTOs are still not authentication.
  await page.evaluate(async () => {
    const open = indexedDB.open('worta-web-replay', 6); await new Promise(resolve => { open.onsuccess = resolve }); const db = open.result
    const tx = db.transaction(['ownerHeads', 'inbox'], 'readwrite'), heads = tx.objectStore('ownerHeads').getAll(), rows = tx.objectStore('inbox').getAll()
    heads.onsuccess = () => { const head = heads.result[0]; head.history[0].wire_digest = '0'.repeat(64); tx.objectStore('ownerHeads').put(head) }
    rows.onsuccess = () => { for (const row of rows.result) if (row.applied_proof.event_id === heads.result[0].history[0].event_id) { row.applied_proof.wire_digest = '0'.repeat(64); tx.objectStore('inbox').put(row) } }
    await new Promise(resolve => { tx.oncomplete = resolve }); db.close()
  })
  const forged = await cache(page); assert.equal((await apply(page)).error, 'metadata_proof_reauthentication_failed'); assert.deepEqual(await cache(page), forged)
  await page.evaluate(async rows => { const open = indexedDB.open('worta-web-replay', 6); await new Promise(resolve => { open.onsuccess = resolve }); const db = open.result, tx = db.transaction(['ownerHeads', 'inbox'], 'readwrite'); for (const name of ['ownerHeads', 'inbox']) for (const row of rows[name]) tx.objectStore(name).put(row); await new Promise(resolve => { tx.oncomplete = resolve }); db.close() }, complete.rows)
  checks.push('matching-forged-proofs-fail-real-aead-reauthentication')
  const receipt = await page.evaluate(async () => { try { return await receiver.receiveCloudAccountHistory(new AbortController().signal) } catch (e) { return e.code } })
  assert.equal(receipt, 'metadata_owner_state_present'); assert.deepEqual(await cache(page), complete); checks.push('receipt-only-never-lies-about-owner-proofs')
  fault = 'offline'; assert.ok((await apply(page)).error); fault = null; assert.deepEqual(await cache(page), complete); checks.push('no-offline-owner-opening')
  fault = 'registry'; registryReads = 3; assert.equal((await apply(page)).error, 'stale_authority'); fault = null
  assert.deepEqual(await cache(page), complete); checks.push('owned-generation-never-cleared-on-new-server-cursor')
  await page.evaluate(() => receiver.logout())
  for (const variant of ['corrupt', 'descriptor', 'bootstrap', 'account', 'device', 'missing', 'conflict', 'delete', 'cover', 'generation', 'mixed']) {
    phase = 'setup'; const fixture = await seedMetadataAccount(page, `Meta${variant}`, variant); const account = await login(page, `Meta${variant}`, true)
    phase = 'owner'; const outcome = await apply(page)
    const expectedError = { corrupt: 'metadata_rejected', descriptor: 'conflicting_record', bootstrap: 'conflicting_record', account: 'metadata_rejected', device: 'conflicting_record', missing: 'missing_history', conflict: 'metadata_conflict', delete: 'unsupported_metadata_operation', cover: 'cover_material_unavailable', generation: 'metadata_rejected', mixed: 'unsupported_history_family' }
    assert.equal(outcome.error, expectedError[variant], variant)
    const state = (await cache(page)).rows, rows = state.inbox.filter(row => row.account_id === account.identity.local_account_id)
    const applied = rows.filter(row => row.state === 'applied')
    assert.equal(applied.length, variant === 'conflict' ? 2 : ['missing', 'generation'].includes(variant) ? 1 : 0, variant)
    assert.equal(state.ownerHeads.filter(head => head.account_id === account.identity.local_account_id).reduce((n, head) => n + head.history.length, 0), applied.length)
    assert.deepEqual(rows.map(row => JSON.parse(row.wire_item)), fixture.expectedWire, variant)
    const stable = await cache(page); assert.equal((await apply(page)).error, expectedError[variant]); assert.deepEqual(await cache(page), stable)
    assert.ok(rows.every(row => row.state === 'applied' || !row.applied_proof)); checks.push(`genuine-producer-negative-${variant}-only-proven-prefix`)
    await page.evaluate(() => receiver.logout())
  }
  checks.push('negative-wire-preservation-and-retry-idempotence')
  phase = 'setup'
  const selfSource = crypto.randomUUID(), self = await seedMetadataAccount(page, 'MetaSelf', 'self', selfSource)
  await page.evaluate(async ({ self, selfSource }) => {
    const open = indexedDB.open('worta-web', 12); await new Promise(resolve => { open.onsuccess = resolve }); const db = open.result
    const tx = db.transaction(['identities', 'accountBindings'], 'readwrite'), local = crypto.randomUUID()
    tx.objectStore('identities').add({ record_version: 1, canonical_user_id: self.canonicalUserId, local_account_id: local, device_id: selfSource })
    tx.objectStore('accountBindings').add({ record_version: 1, canonical_user_id: self.canonicalUserId, local_account_id: local })
    await new Promise(resolve => { tx.oncomplete = resolve }); db.close()
  }, { self, selfSource })
  const own = await login(page, 'MetaSelf'); phase = 'owner'; assert.equal(own.identity.device_id, selfSource)
  assert.equal((await apply(page)).error, 'metadata_self_echo_unsupported')
  const ownRows = (await cache(page)).rows
  assert.ok(ownRows.inbox.filter(row => row.account_id === own.identity.local_account_id).every(row => row.state === 'received' && !row.applied_proof))
  assert.deepEqual(ownRows.inbox.filter(row => row.account_id === own.identity.local_account_id).map(row => JSON.parse(row.wire_item)), self.expectedWire)
  assert.ok(ownRows.ownerHeads.every(head => head.account_id !== own.identity.local_account_id)); checks.push('real-self-echo-without-receipts-refused')
  assert.equal(requests.some(row => row.method !== 'GET' || row.body !== null || !['/api/v1/cloud/projects/bootstrap', '/api/v2/sync/encrypted/capabilities', '/api/v3/sync/encrypted/pull', '/api/v1/account/me', '/api/v1/legal/documents/status', '/api/v1/account/crypto'].includes(row.path)), false)
  assert.deepEqual(errors, []); checks.push('reader-no-plaintext-ack-push-or-capability-declaration')
  console.log(JSON.stringify({ pass: true, scenarios: checks.length, checks, identities: [...new Set(identities)], ownerProofs: 102, readerWrites: 0, selfEchoDevice: selfSource }))
} finally { await context?.close(); await frozen.close(); await rm(directory, { recursive: true, force: true }) }
