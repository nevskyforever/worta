import assert from 'node:assert/strict'
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { chromium } from '@playwright/test'
import { replayV4Fixture, replayV5Fixture } from './replay-v1-fixture.mjs'

const origin = new URL(process.argv[2] || 'http://127.0.0.1:5178').origin
assert.equal(new URL(origin).hostname, '127.0.0.1')
const directory = await mkdtemp(join(tmpdir(), 'worta-replay-v6-')), old = await replayV5Fixture(), v4 = await replayV4Fixture()
let context
const checks = []
async function tab() {
  const page = await context.newPage()
  await page.goto(`${origin}/scripts/browser-cloud-account-harness.html`)
  await page.waitForFunction(() => !!window.harness)
  return page
}
async function snapshot(page) {
  return page.evaluate(async () => {
    const req = indexedDB.open('worta-web-replay')
    await new Promise((resolve, reject) => { req.onsuccess = resolve; req.onerror = () => reject(req.error) })
    const db = req.result, rows = {}, version = db.version
    await new Promise((resolve, reject) => {
      const tx = db.transaction([...db.objectStoreNames], 'readonly')
      for (const name of db.objectStoreNames) { const read = tx.objectStore(name).getAll(); read.onsuccess = () => { rows[name] = read.result } }
      tx.oncomplete = resolve; tx.onabort = () => reject(tx.error)
    }); db.close(); return { version, rows }
  })
}
async function open(page, scope, url, fault = '') {
  return page.evaluate(async ({scope, url, fault}) => {
    const { openBrowserReplayStorage } = await import(/* @vite-ignore */ url)
    const put = IDBObjectStore.prototype.put; let writes = 0, mode = '', firstWriteSucceeded = false
    IDBObjectStore.prototype.put = function (value, ...args) {
      if (fault && this.name === 'replayManifest' && value.record_version === 6) {
        mode = this.transaction.mode
        if (++writes === 1 && fault === 'abort') {
          const request = put.call(this, value, ...args), tx = this.transaction
          request.addEventListener('success', () => { firstWriteSucceeded = true; tx.abort() }, { once: true })
          return request
        }
        if (writes === 2 && fault === 'quota') throw new DOMException('test quota', 'QuotaExceededError')
      }
      return put.call(this, value, ...args)
    }
    try { const store = await openBrowserReplayStorage(scope, new AbortController().signal, () => true); await store.read(); await store.close(); return { code: null, writes, mode, firstWriteSucceeded } }
    catch (error) { return { code: error.code ?? error.name, writes, mode, firstWriteSucceeded } }
    finally { IDBObjectStore.prototype.put = put }
  }, {scope, url, fault})
}
try {
  context = await chromium.launchPersistentContext(directory, {headless:true})
  let page = await tab()
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
  const other = await page.evaluate(() => ({canonicalUserId: crypto.randomUUID(), accountId: crypto.randomUUID(), deviceId: crypto.randomUUID()}))
  await page.evaluate(async ({fixture, other, url}) => {
    const {openBrowserReplayStorage} = await import(/* @vite-ignore */ url)
    for (const scope of [fixture.scope, other]) {
      const store = await openBrowserReplayStorage(scope, new AbortController().signal, () => true)
      try {
        let state = await store.start(fixture.anchor)
        if (scope.accountId === fixture.scope.accountId) {
          state = await store.commitPage(state, 0, fixture.first)
          state = await store.commitPage(state, 2, fixture.second)
          await store.finish(state)
        }
      } finally { await store.close() }
    }
  }, {fixture, other, url: old.storage})
  const before = await snapshot(page)
  assert.equal(before.version, 5); assert.equal(before.rows.replayManifest.length, 2); assert.equal(before.rows.inbox.length, 4)
  for (const kind of ['manifest', 'inventory']) {
    await page.evaluate(async ({ kind, other, rows }) => {
      const open = indexedDB.open('worta-web-replay', 5)
      await new Promise((resolve, reject) => { open.onsuccess = resolve; open.onerror = () => reject(open.error) })
      const db = open.result, name = kind === 'manifest' ? 'replayManifest' : 'cursors'
      const row = rows[name].find(value => value.account_id === other.accountId)
      await new Promise((resolve, reject) => {
        const tx = db.transaction(name, 'readwrite')
        tx.objectStore(name).put(kind === 'manifest' ? {...row, revision: -1} : {...row, ack_cursor: 999})
        tx.oncomplete = resolve; tx.onabort = () => reject(tx.error)
      }); db.close()
    }, { kind, other, rows: before.rows })
    const corrupt = await snapshot(page)
    assert.ok((await open(page, fixture.scope, '/src/platform/web/replayStorage.ts')).code)
    assert.deepEqual(await snapshot(page), corrupt)
    await page.evaluate(async ({ kind, other, rows }) => {
      const req = indexedDB.open('worta-web-replay', 5)
      await new Promise(resolve => { req.onsuccess = resolve })
      const db = req.result, name = kind === 'manifest' ? 'replayManifest' : 'cursors'
      await new Promise((resolve, reject) => { const tx = db.transaction(name, 'readwrite')
        tx.objectStore(name).put(rows[name].find(value => value.account_id === other.accountId))
        tx.oncomplete = resolve; tx.onabort = () => reject(tx.error) }); db.close()
    }, { kind, other, rows: before.rows })
    assert.deepEqual(await snapshot(page), before)
    checks.push(`corrupt-second-account-${kind}-refused-atomically`)
  }
  for (const fault of ['abort', 'quota']) {
    const failed = await open(page, fixture.scope, '/src/platform/web/replayStorage.ts', fault)
    assert.ok(failed.code); assert.equal(failed.mode, 'versionchange'); assert.equal(failed.writes, 2)
    if (fault === 'abort') assert.equal(failed.firstWriteSucceeded, true)
    assert.deepEqual(await snapshot(page), before); checks.push(`native-versionchange-${fault}-after-first-write-atomic`)
  }
  await context.close(); context = await chromium.launchPersistentContext(directory, {headless:true}); page = await tab()
  assert.deepEqual(await snapshot(page), before); checks.push('restart-after-aborted-upgrade')
  assert.equal((await open(page, fixture.scope, '/src/platform/web/replayStorage.ts')).code, null)
  const after = await snapshot(page), expected = structuredClone(before)
  expected.version = 6
  expected.rows.replayManifest = expected.rows.replayManifest.map(row => ({...row, record_version: 6, contract_version: 6}))
  assert.deepEqual(after, expected); checks.push('two-accounts-exact-ciphertext-and-record-preservation')
  assert.equal((await open(page, other, '/src/platform/web/replayStorage.ts')).code, null)
  assert.equal((await open(page, fixture.scope, old.storage)).code, 'unsupported_version')
  assert.deepEqual(await snapshot(page), after); checks.push('pinned-v5-client-refuses-without-write')
  await context.close(); context = await chromium.launchPersistentContext(directory, {headless:true}); page = await tab()
  assert.equal((await open(page, fixture.scope, '/src/platform/web/replayStorage.ts')).code, null)
  assert.deepEqual(await snapshot(page), after); checks.push('restart-after-success')
  await page.evaluate(async () => { const request = indexedDB.deleteDatabase('worta-web-replay')
    await new Promise((resolve, reject) => { request.onsuccess = resolve; request.onerror = () => reject(request.error) }) })
  await page.evaluate(async ({fixture, url}) => {
    const {openBrowserReplayStorage} = await import(/* @vite-ignore */ url)
    const store = await openBrowserReplayStorage(fixture.scope, new AbortController().signal, () => true)
    try { let state = await store.start(fixture.anchor); state = await store.commitPage(state, 0, fixture.first)
      state = await store.commitPage(state, 2, fixture.second); await store.finish(state) } finally { await store.close() }
  }, {fixture, url:v4.storage})
  const before4 = await snapshot(page); assert.equal(before4.version, 4)
  assert.equal((await open(page, fixture.scope, '/src/platform/web/replayStorage.ts')).code, null)
  const expected4 = structuredClone(before4); expected4.version = 6
  expected4.rows.replayManifest = expected4.rows.replayManifest.map(row => ({...row, record_version:6, contract_version:6}))
  assert.deepEqual(await snapshot(page), expected4); checks.push('pinned-v4-upgrade-preserves-history')
  console.log(JSON.stringify({pass:true, checks}))
} finally { await context?.close(); await old.close(); await v4.close(); await rm(directory, {recursive:true, force:true}) }
