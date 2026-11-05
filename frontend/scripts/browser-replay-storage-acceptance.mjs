import assert from 'node:assert/strict'
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { chromium } from '@playwright/test'

const origin = new URL(process.argv[2] || 'http://127.0.0.1:5173').origin
assert.equal(new URL(origin).hostname, '127.0.0.1')
const directory = await mkdtemp(join(tmpdir(), 'worta-replay-storage-')), malformed = await mkdtemp(join(tmpdir(), 'worta-replay-schema-'))
const checks = [], errors = []
let context
async function tab() {
  const page = await context.newPage()
  page.on('pageerror', error => errors.push(error.message))
  await page.goto(`${origin}/scripts/browser-cloud-account-harness.html`)
  await page.waitForFunction(() => !!window.harness)
  return page
}
async function attach(page, fixture) {
  return page.evaluate(async data => {
    const module = await import('/src/platform/web/replayStorage.ts')
    const controller = new AbortController(), current = () => !controller.signal.aborted
    const store = await module.openBrowserReplayStorage(data.scope, controller.signal, current)
    window.rt = { ...data, module, controller, current, store }
    return store.start(data.anchor)
  }, fixture)
}
try {
  context = await chromium.launchPersistentContext(directory, { headless: true })
  let page = await tab()
  await page.evaluate(() => harness.createRecords(true))
  const legacy = await page.evaluate(async () => ({ projects: await harness.rows('localProjects'), notes: await harness.rows('localNotes') }))
  const fixture = await page.evaluate(async () => {
    const { generateAccountMasterKey } = await import('/src/crypto/index.ts')
    const { sealProjectMetadataEvent } = await import('/src/cloud/projectMetadataCodec.ts')
    const { sealCatalogEvent } = await import('/src/cloud/accountCatalogCodec.ts')
    const { encodeBase64Url } = await import('/src/api/base64url.ts')
    const base = (await (await fetch('/src/cloud/__fixtures__/projectMetadataV2.json')).json())[0].event
    const folder = await (await fetch('/src/cloud/__fixtures__/accountCatalogV1.json')).json()
    const scope = { canonicalUserId: base.header.account_id, accountId: crypto.randomUUID(), deviceId: crypto.randomUUID() }
    const source = crypto.randomUUID(), projects = ['project', 'second-project'].map(project_id => ({ project_id, bootstrap_id: crypto.randomUUID(), origin_device_id: source,
      state: 'active', initial_event_count: 0, initial_max_server_sequence: 0 }))
    const anchor = { registry: { current_cursor: 4, projects }, transport: { supported_transport_version: 2, writer_transport_version: 3, cutover_epoch: 2 } }
    const key = await generateAccountMasterKey(), items = []
    try {
      let first
      for (let index = 0; index < 4; index++) {
        const event = index === 1 ? structuredClone(folder) : structuredClone(base), account = index === 1
        const project = projects[index === 2 ? 1 : 0]
        event.header = { ...event.header, account_id: scope.canonicalUserId, device_id: source, event_id: crypto.randomUUID() }
        if (!account) {
          Object.assign(event.header, { project_id: project.project_id, entity_id: project.project_id, bootstrap_id: project.bootstrap_id })
          event.metadata.name = `PRIVATE_REPLAY_${index}`
          if (index === 3) Object.assign(event.header, { operation: 'update', parent_event_ids: [first], generation: 2, revision: 2 })
          else if (index === 0) first = event.header.event_id
        } else event.payload.name = 'PRIVATE_REPLAY_FOLDER'
        const sealed = await (account ? sealCatalogEvent(key, event) : sealProjectMetadataEvent(key, event)), h = event.header
        const descriptor = { event_id: h.event_id, device_id: source, ...(account ? { canonical_user_id: scope.canonicalUserId, scope: 'account' } : { project_id: h.project_id }),
          entity_id: h.entity_id, entity_type: account ? h.entity_type : 'project_metadata', operation: 'upsert', revision: h.revision,
          updated_at: h.updated_at, deleted_at: null, server_sequence: index + 1 }
        items.push({ event: descriptor, object: { ...sealed, nonce: encodeBase64Url(sealed.nonce), ciphertext: encodeBase64Url(sealed.ciphertext) } })
      }
    } finally { key.fill(0) }
    const raw = (start, end) => JSON.stringify({ protocol_version: 3, encrypted_sync_version: 3, items: items.slice(start, end), next_cursor: end, has_more: end < 4 })
    return { scope, anchor, first: raw(0, 2), second: raw(2, 4) }
  })
  const initialization = await page.evaluate(async scope => {
    const { openBrowserReplayStorage } = await import('/src/platform/web/replayStorage.ts')
    const original = IDBDatabase.prototype.createObjectStore
    IDBDatabase.prototype.createObjectStore = function (...args) { if (args[0] === 'inbox') throw new DOMException('test upgrade interruption', 'AbortError'); return original.apply(this, args) }
    let failed = false
    try { await openBrowserReplayStorage(scope, new AbortController().signal, () => true) } catch { failed = true }
    finally { IDBDatabase.prototype.createObjectStore = original }
    return { failed, database: (await indexedDB.databases()).find(row => row.name === 'worta-web-replay') ?? null }
  }, fixture.scope)
  assert.equal(initialization.failed, true); assert.ok(!initialization.database || initialization.database.version === 0)
  checks.push('atomic-initialization')
  const fresh = await attach(page, fixture)
  assert.equal(fresh.manifest.phase, 'receiving'); assert.equal(fresh.rows.length, 0)
  const first = await page.evaluate(async () => rt.store.commitPage(await rt.store.read(), 0, rt.first))
  const duplicate = await page.evaluate(async () => rt.store.commitPage(await rt.store.read(), 0, rt.first))
  assert.deepEqual(duplicate, first); checks.push('fresh-schema-original-wire-exact-page-replay')
  const competitor = await tab()
  const contention = await competitor.evaluate(async scope => {
    const { openBrowserReplayStorage } = await import('/src/platform/web/replayStorage.ts')
    try { const store = await openBrowserReplayStorage(scope, new AbortController().signal, () => true); await store.close(); return null } catch (e) { return e.code }
  }, fixture.scope)
  assert.equal(contention, 'writer_busy'); await competitor.close(); checks.push('real-tab-writer-contention')
  await context.close()
  context = await chromium.launchPersistentContext(directory, { headless: true })
  page = await tab()
  assert.deepEqual(await attach(page, fixture), first); checks.push('chromium-profile-restart')
  const failures = await page.evaluate(async () => {
    const before = await rt.store.read(), codes = {}
    const attempt = async (name, action) => { try { await action(); codes[name] = null } catch (e) { codes[name] = e.code } }
    const bad = JSON.parse(rt.second); bad.items[1].event.event_id = before.rows[0].event_id
    await attempt('duplicateEvent', () => rt.store.commitPage(before, 2, JSON.stringify(bad)))
    const changed = structuredClone(before); changed.manifest.revision++
    await attempt('staleCas', () => rt.store.commitPage(changed, 2, rt.second))
    // Fault-inject an over-budget persisted row representation. Admission must
    // reject BEFORE native writes, not discover this only when reopening rows.
    const encode = TextEncoder.prototype.encode, addedEvent = JSON.parse(rt.second).items[0].event.event_id
    TextEncoder.prototype.encode = function (value) {
      if (value.includes('"wire_item":') && value.includes(addedEvent)) return new Uint8Array(32 * 1024 * 1024)
      return encode.call(this, value)
    }
    try { await attempt('inventoryBudget', () => rt.store.commitPage(before, 2, rt.second)) } finally { TextEncoder.prototype.encode = encode }
    const afterBudget = await rt.store.read()
    const put = IDBObjectStore.prototype.put
    IDBObjectStore.prototype.put = function (...args) { if (this.name === 'replayManifest') throw new DOMException('test quota', 'QuotaExceededError'); return put.apply(this, args) }
    try { await attempt('quota', () => rt.store.commitPage(before, 2, rt.second)) } finally { IDBObjectStore.prototype.put = put }
    const afterQuota = await rt.store.read()
    IDBObjectStore.prototype.put = function (...args) {
      const request = put.apply(this, args)
      if (this.name === 'cursors') request.addEventListener('success', () => rt.controller.abort(), { once: true })
      return request
    }
    try { await attempt('abort', () => rt.store.commitPage(before, 2, rt.second)) } finally { IDBObjectStore.prototype.put = put }
    return { before, afterBudget, afterQuota, codes }
  })
  assert.ok(failures.codes.duplicateEvent); assert.equal(failures.codes.staleCas, 'stale_cursor')
  assert.equal(failures.codes.inventoryBudget, 'source_limit'); assert.deepEqual(failures.afterBudget, first)
  assert.equal(failures.codes.quota, 'quota_exceeded'); assert.equal(failures.codes.abort, 'closed')
  assert.deepEqual(failures.afterQuota, first); assert.deepEqual(await attach(page, fixture), first)
  checks.push('duplicate-event-atomic-refusal', 'full-cas', 'inventory-budget-before-native-write', 'quota-entire-page-rollback', 'revocation-entire-page-rollback')
  const complete = await page.evaluate(async () => rt.store.finish(await rt.store.commitPage(await rt.store.read(), 2, rt.second)))
  assert.equal(complete.rows.length, 4); assert.equal(complete.manifest.phase, 'received')
  assert.equal(new Set(complete.rows.map(row => row.event_id)).size, 4)
  assert.ok(complete.rows.every(row => row.state === 'received' && row.record_version === 1))
  assert.equal(JSON.stringify(complete).includes('PRIVATE_REPLAY_'), false)
  assert.deepEqual(await page.evaluate(async () => ({ projects: await harness.rows('localProjects'), notes: await harness.rows('localNotes') })), legacy)
  checks.push('mixed-account-project-ciphertext-only-zero-proofs', 'v12-byte-preservation')
  const barriers = await page.evaluate(async () => {
    const snapshot = await rt.store.read(), rollback = structuredClone(rt.anchor), drift = structuredClone(rt.anchor), codes = {}
    rollback.registry.current_cursor = 3; drift.registry.projects[0].bootstrap_id = crypto.randomUUID()
    for (const [name, anchor] of [['rollback', rollback], ['lineage', drift]]) {
      try { await rt.store.start(anchor); codes[name] = null } catch (e) { codes[name] = e.code }
    }
    const open = indexedDB.open('worta-web-replay', 7)
    await new Promise(resolve => { open.onsuccess = resolve })
    const db = open.result
    const replace = row => new Promise((resolve, reject) => {
      const tx = db.transaction('inbox', 'readwrite'); tx.objectStore('inbox').put(row); tx.oncomplete = resolve; tx.onabort = () => reject(tx.error)
    })
    try {
      await replace({ ...snapshot.rows[0], state: 'applied' })
      try { await rt.store.read(); codes.proof = null } catch (e) { codes.proof = e.code }
      await replace(snapshot.rows[0])
    } finally { db.close() }
    return { codes, after: await rt.store.read() }
  })
  assert.deepEqual(barriers.codes, { rollback: 'stale_cursor', lineage: 'stale_lineage', proof: 'corrupt_record' })
  assert.deepEqual(barriers.after, complete); checks.push('cursor-lineage-rollback-refusal', 'no-forged-proof-admission')
  await page.evaluate(() => rt.store.close())
  const other = { canonicalUserId: crypto.randomUUID(), accountId: crypto.randomUUID(), deviceId: crypto.randomUUID() }
  const isolation = await page.evaluate(async ({ scope, anchor }) => {
    const store = await rt.module.openBrowserReplayStorage(scope, new AbortController().signal, () => true)
    try { return await store.start(anchor) } finally { await store.close() }
  }, { scope: other, anchor: fixture.anchor })
  assert.equal(isolation.rows.length, 0)
  assert.deepEqual(await attach(page, fixture), complete)
  const rebuilt = await page.evaluate(async () => { const next = structuredClone(rt.anchor); next.registry.current_cursor = 5; return rt.store.start(next) })
  assert.notEqual(rebuilt.manifest.generation, complete.manifest.generation); assert.equal(rebuilt.rows.length, 0)
  await page.evaluate(() => rt.store.close())
  const otherAfter = await page.evaluate(async scope => {
    const store = await rt.module.openBrowserReplayStorage(scope, new AbortController().signal, () => true)
    try { return await store.read() } finally { await store.close() }
  }, other)
  assert.deepEqual(otherAfter, isolation); checks.push('account-scoped-generation-rebuild')
  const identityMismatch = await page.evaluate(async scope => {
    const { openBrowserReplayStorage } = await import('/src/platform/web/replayStorage.ts')
    const store = await openBrowserReplayStorage({ ...scope, accountId: crypto.randomUUID() }, new AbortController().signal, () => true)
    try { await store.read(); return null } catch (e) { return e.code } finally { await store.close() }
  }, fixture.scope)
  assert.equal(identityMismatch, 'corrupt_record'); checks.push('account-identity-binding')
  const version = await page.evaluate(async () => {
    const open = indexedDB.open('worta-web-replay', 7)
    await new Promise((resolve, reject) => { open.onsuccess = resolve; open.onerror = () => reject(open.error) })
    const db = open.result; db.close()
    try { await rt.module.openBrowserReplayStorage(rt.scope, new AbortController().signal, () => true); return null } catch (e) { return e.code }
  })
  assert.equal(version, 'unsupported_version'); checks.push('older-client-refuses-newer-schema')
  await context.close()
  context = await chromium.launchPersistentContext(malformed, { headless: true })
  page = await tab()
  const schema = await page.evaluate(async scope => {
    const open = indexedDB.open('worta-web-replay', 5)
    open.onupgradeneeded = () => open.result.createObjectStore('unknown').put('preserve', 'sentinel')
    await new Promise((resolve, reject) => { open.onsuccess = resolve; open.onerror = () => reject(open.error) }); open.result.close()
    const { openBrowserReplayStorage } = await import('/src/platform/web/replayStorage.ts')
    let code
    try { await openBrowserReplayStorage(scope, new AbortController().signal, () => true) } catch (e) { code = e.code }
    const read = indexedDB.open('worta-web-replay', 5)
    await new Promise(resolve => { read.onsuccess = resolve })
    const db = read.result, request = db.transaction('unknown').objectStore('unknown').get('sentinel')
    const sentinel = await new Promise(resolve => { request.onsuccess = () => resolve(request.result) }); db.close()
    return { code, sentinel }
  }, fixture.scope)
  assert.deepEqual(schema, { code: 'unsupported_version', sentinel: 'preserve' }); checks.push('malformed-schema-refusal-no-deletion')
  assert.deepEqual(errors, [])
  console.log(JSON.stringify({ pass: true, scenarios: checks.length, checks }))
} finally { await context?.close(); await rm(directory, { recursive: true, force: true }); await rm(malformed, { recursive: true, force: true }) }
