import assert from 'node:assert/strict'
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { chromium } from '@playwright/test'
import { replayV2Fixture } from './replay-v1-fixture.mjs'
import { seedMetadataAccount } from './replay-metadata-fixture.mjs'
import { seedStructuralAccount } from './replay-structural-fixture.mjs'
const origin = new URL(process.argv[2]).origin, backend = new URL(process.argv[3]).origin
assert.equal(new URL(origin).hostname, '127.0.0.1'); assert.equal(new URL(backend).hostname, '127.0.0.1')
const directory = await mkdtemp(join(tmpdir(), 'worta-structural-')), frozen = await replayV2Fixture()
const checks = [], requests = [], errors = [], identities = []
let context, phase = 'setup', fault = null
async function launch() {
  context = await chromium.launchPersistentContext(directory, { headless: true })
  await context.route(url => url.pathname.startsWith('/api/'), async route => {
    const request = route.request(), url = new URL(request.url())
    if (phase === 'owner' && !url.pathname.startsWith('/api/v1/auth/')) requests.push({ path: url.pathname, method: request.method(), body: request.postData(), since: url.searchParams.get('since') })
    if (phase === 'owner' && fault === 'offline') return route.abort('failed')
    const response = await route.fetch({ url: backend + url.pathname + url.search })
    if (phase === 'owner' && fault === 'wire' && url.pathname === '/api/v3/sync/encrypted/pull') {
      const value = await response.json(); if (value.items.length) value.items[0].object.ciphertext = (value.items[0].object.ciphertext[0] === 'A' ? 'B' : 'A') + value.items[0].object.ciphertext.slice(1)
      return route.fulfill({ response, json: value })
    }
    if (phase === 'owner' && fault === 'registry' && url.pathname === '/api/v1/cloud/projects/bootstrap') {
      const value = await response.json(); value.current_cursor++; return route.fulfill({ response, json: value })
    }
    await route.fulfill({ response })
  })
  const page = await context.newPage(); page.on('pageerror', error => errors.push(error.message))
  await page.goto(`${origin}/scripts/browser-cloud-account-harness.html`); await page.waitForFunction(() => !!window.harness)
  return page
}
async function login(page, username, register = false, module = '/src/platform/web/cloudAccountSession.ts') {
  phase = 'setup'
  const identity = await page.evaluate(async ({ username, register, module }) => {
    const { BrowserCloudAccountSession } = await import(/* @vite-ignore */ module)
    window.reader = new BrowserCloudAccountSession(); await reader.login(username, 'correct horse battery staple')
    await reader.unlock('encryption-password'); if (register) await reader.registerCloudReadDevice(); return reader.currentIdentity
  }, { username, register, module }); identities.push(identity.device_id); return identity
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
const apply = page => page.evaluate(async () => { try { return await reader.applyCloudAccountStructure(new AbortController().signal) } catch (e) { return { error: e.code ?? e.message } } })
async function openCurrent(page, scope) {
  return page.evaluate(async scope => {
    const { openBrowserReplayStorage } = await import('/src/platform/web/replayStorage.ts')
    try { const store = await openBrowserReplayStorage(scope, new AbortController().signal, () => true); try { await store.read() } finally { await store.close() }; return null } catch (e) { return e.code }
  }, scope)
}
async function replace(page, database, records) {
  await page.evaluate(async ({ database, records }) => {
    const open = indexedDB.open(database); await new Promise(resolve => { open.onsuccess = resolve }); const db = open.result
    const tx = db.transaction(Object.keys(records), 'readwrite'); for (const [name, rows] of Object.entries(records)) for (const row of rows) tx.objectStore(name).put(row)
    await new Promise((resolve, reject) => { tx.oncomplete = resolve; tx.onabort = () => reject(tx.error) }); db.close()
  }, { database, records })
}
try {
  let page = await launch(); await page.evaluate(() => harness.createRecords(true))
  await seedMetadataAccount(page, 'StructuralMigration', 'one')
  const migrationIdentity = await login(page, 'StructuralMigration', true, frozen.url)
  const oldResult = await page.evaluate(() => reader.applyCloudAccountMetadata(new AbortController().signal))
  assert.equal(oldResult.ownerProofs, 1); await page.evaluate(() => reader.logout())
  const v2 = await cache(page); assert.equal(v2.version, 2); assert.equal(v2.rows.ownerHeads.length, 1)
  const scope = { accountId: migrationIdentity.local_account_id, deviceId: migrationIdentity.device_id, canonicalUserId: v2.rows.identities[0].canonical_user_id }
  for (const kind of ['quota', 'abort']) {
    const error = await page.evaluate(async ({ scope, kind }) => {
      const original = IDBObjectStore.prototype.put
      IDBObjectStore.prototype.put = function (value, ...args) {
        if (this.name === 'replayManifest' && value.record_version === 6) { if (kind === 'quota') throw new DOMException('fixture', 'QuotaExceededError'); this.transaction.abort(); throw new DOMException('fixture', 'AbortError') }
        return original.call(this, value, ...args)
      }
      try { const { openBrowserReplayStorage } = await import('/src/platform/web/replayStorage.ts'); const store = await openBrowserReplayStorage(scope, new AbortController().signal, () => true); await store.close(); return null }
      catch (e) { return e.code } finally { IDBObjectStore.prototype.put = original }
    }, { scope, kind })
    assert.equal(error, kind === 'quota' ? 'quota_exceeded' : 'storage_failed'); assert.deepEqual(await cache(page), v2); checks.push(`v2-v3-${kind}-rollback-preserves-genuine-metadata-proofs`)
  }
  const corrupt = structuredClone(v2.rows.ownerHeads[0]); corrupt.record_version = 99
  await replace(page, 'worta-web-replay', { ownerHeads: [corrupt] }); const bad = await cache(page)
  assert.equal(await openCurrent(page, scope), 'corrupt_record'); assert.deepEqual(await cache(page), bad)
  await replace(page, 'worta-web-replay', { ownerHeads: v2.rows.ownerHeads }); checks.push('corrupt-v2-migration-no-cleanup')
  assert.equal(await openCurrent(page, scope), null)
  const v3 = structuredClone(v2); v3.version = 6; v3.rows.replayManifest = v3.rows.replayManifest.map(row => ({ ...row, record_version: 6, contract_version: 6, applied_cursor: row.metadata_cursor, source_seeds: [] }))
  assert.deepEqual(await cache(page), v3); checks.push('v2-v6-exact-wire-and-real-owner-proof-preservation')
  const oldFailure = await page.evaluate(async ({ scope, url }) => { try { const { openBrowserReplayStorage } = await import(/* @vite-ignore */ url); const store = await openBrowserReplayStorage(scope, new AbortController().signal, () => true); await store.close(); return null } catch (e) { return e.code } }, { scope, url: frozen.storage })
  assert.equal(oldFailure, 'unsupported_version'); assert.deepEqual(await cache(page), v3); checks.push('genuine-v2-client-refuses-v6')
  await login(page, 'StructuralMigration'); phase = 'owner'
  assert.equal((await page.evaluate(() => reader.applyCloudAccountMetadata(new AbortController().signal))).ownerProofs, 1)
  assert.deepEqual(await cache(page), v3); checks.push('metadata-only-restart-reauthentication-after-migration')
  phase = 'setup'; await page.evaluate(() => reader.logout())
  const fixture = await seedStructuralAccount(page, 'Structure'), identity = await login(page, 'Structure', true), account = identity.local_account_id
  const legacy = await cache(page, 'worta-web'); assert.equal(legacy.version, 12)
  phase = 'owner'
  const scoped = state => ({ rows: state.rows.inbox.filter(row => row.account_id === account), owners: state.rows.ownerHeads.filter(row => row.account_id === account), manifest: state.rows.replayManifest.find(row => row.account_id === account) })
  const coverage = async expected => { const state = scoped(await cache(page)); assert.equal(state.manifest.applied_cursor, expected); assert.equal(state.rows.filter(row => row.state !== 'received').length, expected); assert.deepEqual(state.rows.map(row => JSON.parse(row.wire_item)), fixture.wire); return state }
  // The receiver stages once; later family dispatches never pull a separate full feed.
  const receipt = await page.evaluate(() => reader.receiveCloudAccountHistory(new AbortController().signal)); assert.equal(receipt.ownerProofs, 0)
  for (const family of ['stage', 'stage_order']) {
    let stable
    for (const kind of ['quota', 'abort', 'lock']) {
      const outcome = await page.evaluate(async ({ family, kind }) => {
        const original = IDBObjectStore.prototype.put
        IDBObjectStore.prototype.put = function (value, ...args) {
          if (this.name === 'ownerHeads' && value.owner_kind === family) {
            if (kind === 'quota') throw new DOMException('fixture', 'QuotaExceededError')
            if (kind === 'lock') { void reader.lock(); return original.call(this, value, ...args) }
            this.transaction.abort(); throw new DOMException('fixture', 'AbortError')
          }
          return original.call(this, value, ...args)
        }
        try { await reader.applyCloudAccountStructure(new AbortController().signal); return null } catch (e) { return e.code } finally { IDBObjectStore.prototype.put = original }
      }, { family, kind })
      assert.equal(outcome, kind === 'quota' ? 'quota_exceeded' : kind === 'lock' ? 'session_closed' : 'storage_failed')
      await coverage(family === 'stage' ? 1 : 4)
      const after = await cache(page); if (stable) assert.deepEqual(after, stable); stable = after
      if (kind === 'lock') { await page.evaluate(() => reader.logout()); await login(page, 'Structure'); phase = 'owner' }
      checks.push(`${family}-${kind}-atomic-head-inbox-dependency-journal-rollback`)
    }
  }
  // Mutate an already-applied Stage head AND matching inbox proof after Order preparation.
  const race = await page.evaluate(async () => {
    const original = window.fetch; let modes = 0
    window.fetch = async (...args) => {
      const response = await original(...args), url = new URL(args[0] instanceof Request ? args[0].url : String(args[0]), location.href)
      if (url.pathname === '/api/v2/sync/encrypted/capabilities' && ++modes === 4) {
        const open = indexedDB.open('worta-web-replay', 7); await new Promise(resolve => { open.onsuccess = resolve }); const db = open.result
        const tx = db.transaction(['ownerHeads', 'inbox'], 'readwrite'), heads = tx.objectStore('ownerHeads').getAll(), rows = tx.objectStore('inbox').getAll()
        heads.onsuccess = () => { const h = heads.result.find(head => head.owner_kind === 'stage'); window.repairHead = structuredClone(h); h.history[0].wire_digest = '0'.repeat(64); tx.objectStore('ownerHeads').put(h) }
        rows.onsuccess = () => { const row = rows.result.find(row => row.event_id === repairHead.history[0].event_id); window.repairRow = structuredClone(row); row.stage_proof.wire_digest = '0'.repeat(64); tx.objectStore('inbox').put(row) }
        await new Promise(resolve => { tx.oncomplete = resolve }); db.close()
      }
      return response
    }
    try { await reader.applyCloudAccountStructure(new AbortController().signal); return null } catch (e) { return e.code } finally { window.fetch = original }
  })
  assert.equal(race, 'stale_authority')
  const repair = await page.evaluate(() => ({ ownerHeads: [repairHead], inbox: [repairRow] })); await replace(page, 'worta-web-replay', repair)
  await coverage(5); checks.push('order-full-inventory-cas-rejects-concurrent-stage-head-change')
  const lost = await page.evaluate(async () => {
    const original = IDBDatabase.prototype.transaction, put = IDBObjectStore.prototype.put; let written = false
    IDBObjectStore.prototype.put = function (value, ...args) { if (this.name === 'ownerHeads' && value.owner_kind === 'stage_order') written = true; return put.call(this, value, ...args) }
    IDBDatabase.prototype.transaction = function (...args) { const tx = original.apply(this, args); if (this.name === 'worta-web-replay' && args[1] === 'readwrite') tx.addEventListener('complete', () => { if (written) { written = false; void reader.lock() } }); return tx }
    try { await reader.applyCloudAccountStructure(new AbortController().signal); return null } catch (e) { return e.code }
    finally { IDBDatabase.prototype.transaction = original; IDBObjectStore.prototype.put = put }
  })
  assert.equal(lost, 'session_closed'); await coverage(6); checks.push('lost-order-commit-response-real-durable-proof')
  await context.close(); page = await launch(); assert.deepEqual(await login(page, 'Structure'), identity); phase = 'owner'
  const beforeResume = requests.length, result = await apply(page)
  assert.deepEqual(requests.slice(beforeResume).filter(row => row.path === '/api/v3/sync/encrypted/pull' && Number(row.since) < 102).map(row => Number(row.since)), [0, 100])
  checks.push('one-account-feed-dispatched-to-all-three-owners')
  assert.deepEqual(result, { kind: 'structural_owners_applied', cursor: 102, generation: result.generation, ownerProofs: 102, projects: 2,
    metadataProofs: 92, stageProofs: 6, stageOrderProofs: 4, projectOpened: false, ready: false, declarationAllowed: false, writerAuthorized: false })
  const complete = await cache(page), state = await coverage(102)
  assert.equal(state.manifest.metadata_cursor, 1)
  assert.equal(state.owners.filter(head => head.owner_kind === 'stage').length, 4); assert.equal(state.owners.filter(head => head.owner_kind === 'stage_order').length, 2)
  for (const row of state.rows) {
    const proof = row.applied_proof ?? row.stage_proof ?? row.stage_order_proof
    assert.deepEqual(proof.reference, JSON.parse(row.wire_item).event)
    assert.ok(state.owners.some(head => head.history.some(value => JSON.stringify(value) === JSON.stringify(proof))))
  }
  assert.equal(JSON.stringify(complete).includes('PRIVATE_STRUCTURAL_'), false); assert.deepEqual(await cache(page, 'worta-web'), legacy)
  checks.push('genuine-metadata-stage-create-update-and-order-reorder-two-projects-two-pages', 'exact-real-owner-proofs-and-separate-derived-journal', 'v12-all-stores-unchanged')
  // Read exact committed winners through the real owners with fresh public auth/key
  // composition. This is readonly acceptance plumbing, never a production UI route.
  phase = 'setup'; await page.evaluate(() => reader.logout())
  const winners = await page.evaluate(async () => {
    const { NormalUserAuthRuntime } = await import('/src/auth/userAuth.ts')
    const { AuthoritativeAccountBinding } = await import('/src/auth/accountBinding.ts')
    const { RuntimeKeyContext } = await import('/src/auth/keyContext.ts')
    const { openBrowserPersistence, runBrowserTransaction } = await import('/src/platform/web/persistence.ts')
    const { BrowserStageApply } = await import('/src/platform/web/stageApply.ts')
    const { BrowserStageOrderApply } = await import('/src/platform/web/stageOrderApply.ts')
    const auth = new NormalUserAuthRuntime(); const context = await auth.login('Structure', 'correct horse battery staple')
    const storage = await openBrowserPersistence(context.userId, new AbortController().signal)
    const identity = await storage.read(context.userId), binding = new AuthoritativeAccountBinding(auth, storage), keys = new RuntimeKeyContext(auth, binding)
    let db
    try {
      await keys.unlockWithPassphrase(identity.local_account_id, 'encryption-password')
      const open = indexedDB.open('worta-web-replay', 7); await new Promise(resolve => { open.onsuccess = resolve }); db = open.result
      const scope = { canonicalUserId: context.userId, accountId: identity.local_account_id, deviceId: identity.device_id }
      const current = () => auth.isCurrent(context) && !!keys.leaseForAccount(scope.accountId)?.isCurrent()
      const transaction = (input, _stores, mode, schedule) => {
        if (Object.entries(scope).some(([key, value]) => input[key] !== value) || mode !== 'readonly' || !current()) throw Error('readonly_owner_acceptance_scope')
        return runBrowserTransaction(db, [...db.objectStoreNames], 'readonly', schedule)
      }
      const stages = new BrowserStageApply(transaction, current).bind(auth, binding, keys), orders = new BrowserStageOrderApply(transaction, current).bind(auth, binding, keys), result = []
      for (const project of ['project', 'second-project']) result.push({ a: (await stages.read(scope, project, 'a')).name, b: (await stages.read(scope, project, 'b')).name, order: await orders.read(scope, project) })
      return result
    } finally { db?.close(); await keys.dispose(); await storage.close(); await auth.logout() }
  })
  assert.deepEqual(winners, [0, 1].map(() => ({ a: 'PRIVATE_STRUCTURAL_STAGE_a_EDITED', b: 'PRIVATE_STRUCTURAL_STAGE_b', order: ['b', 'a'] })))
  assert.deepEqual(await cache(page), complete); assert.deepEqual(await cache(page, 'worta-web'), legacy)
  await context.close(); page = await launch(); await login(page, 'Structure'); phase = 'owner'
  const warm = await page.evaluate(async () => {
    const originals = {}
    for (const name of ['put', 'add', 'delete', 'clear']) { originals[name] = IDBObjectStore.prototype[name]; IDBObjectStore.prototype[name] = function (...args) { if (this.transaction.db.name === 'worta-web-replay') throw Error('unexpected_replay_write'); return originals[name].apply(this, args) } }
    try { return await reader.applyCloudAccountStructure(new AbortController().signal) } finally { for (const [name, original] of Object.entries(originals)) IDBObjectStore.prototype[name] = original }
  })
  assert.deepEqual(warm, result); assert.deepEqual(await cache(page), complete); checks.push('full-chromium-restart-all-owner-aead-reauthentication-with-zero-writes')
  for (const family of ['stage', 'stage_order']) {
    const fake = structuredClone(complete.rows), head = fake.ownerHeads.find(head => head.account_id === account && head.owner_kind === family), proof = head.history[0]
    proof.wire_digest = '0'.repeat(64)
    const row = fake.inbox.find(row => row.account_id === account && row.event_id === proof.event_id)
    row[family === 'stage' ? 'stage_proof' : 'stage_order_proof'].wire_digest = proof.wire_digest
    await replace(page, 'worta-web-replay', { ownerHeads: [head], inbox: [row] })
    const forged = await cache(page)
    assert.equal((await apply(page)).error, 'structural_proof_reauthentication_failed'); assert.deepEqual(await cache(page), forged)
    await replace(page, 'worta-web-replay', { ownerHeads: complete.rows.ownerHeads, inbox: complete.rows.inbox })
    checks.push(`${family}-matching-fake-proofs-fail-real-reauthentication`)
  }
  for (const kind of ['wire', 'registry', 'offline']) { fault = kind; assert.ok((await apply(page)).error); fault = null; assert.deepEqual(await cache(page), complete); checks.push(`server-${kind}-no-cached-success-or-proof-mutation`) }
  for (const kind of ['cancel', 'lock', 'logout', 'account']) {
    const error = await page.evaluate(async kind => {
      const original = window.fetch, controller = new AbortController(); let fired = false
      window.fetch = async (...args) => { const response = await original(...args), url = new URL(args[0] instanceof Request ? args[0].url : String(args[0]), location.href)
        if (!fired && url.pathname === '/api/v3/sync/encrypted/pull') { fired = true; if (kind === 'cancel') controller.abort(); else if (kind === 'lock') await reader.lock(); else if (kind === 'logout') await reader.logout(); else await reader.login('StructuralOther', 'correct horse battery staple') }; return response }
      try { await reader.applyCloudAccountStructure(controller.signal); return null } catch (e) { return e.code } finally { window.fetch = original }
    }, kind)
    assert.equal(error, 'session_closed'); assert.deepEqual(await cache(page), complete); checks.push(`late-${kind}-no-success`)
    await page.evaluate(() => reader.logout()); await login(page, 'Structure'); phase = 'owner'
  }
  phase = 'setup'; await page.evaluate(() => reader.logout())
  const rejectedPrefix = { version: 1, late_stage: 0, corrupt: 1, bootstrap: 1, account: 1, device: 1, metadata: 1, parent: 3, fork: 3, delete: 0, unknown: 4, omit: 4, head: 4, order_parent: 5 }
  for (const [variant, prefix] of Object.entries(rejectedPrefix)) {
    phase = 'setup'; const source = await seedStructuralAccount(page, `Stage${variant}`, variant), id = await login(page, `Stage${variant}`, true)
    phase = 'owner'; const failed = await apply(page); assert.ok(failed.error, variant)
    const saved = await cache(page), rows = saved.rows.inbox.filter(row => row.account_id === id.local_account_id), manifest = saved.rows.replayManifest.find(row => row.account_id === id.local_account_id)
    assert.equal(manifest.applied_cursor, prefix, variant); assert.equal(rows.filter(row => row.state !== 'received').length, prefix, variant)
    assert.deepEqual(rows.map(row => JSON.parse(row.wire_item)), source.wire)
    assert.equal((await apply(page)).error, failed.error); assert.deepEqual(await cache(page), saved)
    checks.push(`genuine-negative-${variant}-no-false-proof-or-coverage-advance`); await page.evaluate(() => reader.logout())
  }
  assert.equal(requests.some(row => row.method !== 'GET' || row.body !== null || !['/api/v1/cloud/projects/bootstrap', '/api/v2/sync/encrypted/capabilities', '/api/v3/sync/encrypted/pull', '/api/v1/account/me', '/api/v1/legal/documents/status', '/api/v1/account/crypto'].includes(row.path)), false)
  assert.deepEqual(errors, []); checks.push('no-plaintext-ack-push-registration-or-capability-requests')
  console.log(JSON.stringify({ pass: true, scenarios: checks.length, checks, identities: [...new Set(identities)], ownerProofs: 102, readerWrites: 0 }))
} finally { await context?.close(); await frozen.close(); await rm(directory, { recursive: true, force: true }) }
