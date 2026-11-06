import assert from 'node:assert/strict'
import { readFile, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { createServer } from 'vite'
import { chromium } from '@playwright/test'

const fixture = JSON.parse(await readFile(process.argv[2], 'utf8'))
const directory = await mkdtemp(join(tmpdir(), 'worta-replay-conflict-'))
let context, vite
try {
  vite = await createServer({ logLevel: 'error', server: { host: '127.0.0.1', port: 0, strictPort: true } })
  await vite.listen()
  const address = vite.httpServer.address()
  if (!address || typeof address === 'string') throw Error('vite_address')
  const origin = `http://127.0.0.1:${address.port}`
  const launch = async () => { context = await chromium.launchPersistentContext(directory, { headless: true }); const page = await context.newPage(); await page.goto(`${origin}/scripts/browser-cloud-account-harness.html`); return page }
  let page = await launch()
  if (fixture.stores) await page.evaluate(async f => {
    const { openBrowserReplayStorage } = await import('/src/platform/web/replayStorage.ts')
    const initial = await openBrowserReplayStorage(f.scope, new AbortController().signal, () => true); await initial.close()
    const open = indexedDB.open('worta-web-replay', 8)
    await new Promise((resolve, reject) => { open.onsuccess = resolve; open.onerror = () => reject(open.error) })
    const db = open.result, tx = db.transaction([...db.objectStoreNames], 'readwrite')
    for (const [name, rows] of Object.entries(f.stores)) for (const row of rows) tx.objectStore(name).put(row)
    await new Promise((resolve, reject) => { tx.oncomplete = resolve; tx.onabort = () => reject(tx.error) }); db.close()
  }, fixture)
  async function attach(page) {
    await page.evaluate(async f => {
      const { NormalUserAuthRuntime } = await import('/src/auth/userAuth.ts')
      const { cloudProjectsApi } = await import('/src/api/cloudProjects.ts')
      const { encryptedSyncV2Api } = await import('/src/api/encryptedSyncV2.ts')
      const { encryptedSyncV3Api } = await import('/src/api/encryptedSyncV3.ts')
      const { openBrowserReplayStorage } = await import('/src/platform/web/replayStorage.ts')
      const auth = new NormalUserAuthRuntime({ login: async () => ({ access_token: 'local-fixture', refresh_token: 'local-fixture', access_expires_in: 600 }),
        me: async () => ({ id: f.scope.canonicalUserId, username: 'fixture', email: 'fixture@example.test', email_verified: true, role: 'user', status: 'active', created_at: 'now' }), logout: async () => undefined })
      await auth.login('fixture', 'fixture'); const context = auth.requireContext()
      const lease = { localAccountId: f.scope.accountId, canonicalUserId: f.scope.canonicalUserId, authEpoch: context.authEpoch,
        isCurrent: () => auth.isCurrent(context), use: async fn => { const key = Uint8Array.from(f.key); try { return await fn(key) } finally { key.fill(0) } } }
      const bindings = { ensureForCurrentUser: async () => ({ context }) }, keys = { leaseForAccount: () => lease, onInvalidated: () => () => undefined }
      cloudProjectsApi.listBootstraps = async () => f.anchor.registry
      encryptedSyncV2Api.capabilities = async () => f.anchor.transport
      const items = f.items ?? f.stores.inbox.map(row => JSON.parse(row.wire_item))
      encryptedSyncV3Api.pullRaw = async (_token, _device, since) => JSON.stringify({ protocol_version: 3, encrypted_sync_version: 3,
        items: items.slice(since), next_cursor: f.anchor.registry.current_cursor, has_more: false })
      const store = await openBrowserReplayStorage(f.scope, new AbortController().signal, () => true)
      if (f.items && !(await store.read()).manifest) {
        let state = await store.start(f.anchor)
        const swapped = [...items]; [swapped[2], swapped[3]] = [swapped[3], swapped[2]]
        let rejected = false
        try { await store.commitPage(state, 0, JSON.stringify({ protocol_version: 3, encrypted_sync_version: 3,
          items: swapped, next_cursor: items.length, has_more: false })) } catch { rejected = true }
        if (!rejected || JSON.stringify(await store.read()) !== JSON.stringify(state)) throw Error('out_of_order_not_atomic')
        state = await store.commitPage(state, 0, JSON.stringify({ protocol_version: 3, encrypted_sync_version: 3,
          items, next_cursor: items.length, has_more: false }))
        await store.finish(state)
      }
      window.adapter = { store, run: () => store.applyOrderedContentUpdate(auth, bindings, keys) }
    }, fixture)
  }
  await attach(page)
  const aborted = await page.evaluate(async () => {
    const put = IDBObjectStore.prototype.put
    IDBObjectStore.prototype.put = function(value, ...args) {
      if (this.name === 'inbox' && value.state === 'content_note_conflict_preserved') { this.transaction.abort(); throw new DOMException('test abort', 'AbortError') }
      return put.call(this, value, ...args)
    }
    try { await adapter.run(); return { refused: false } } catch (error) { return { refused: true, error: String(error?.stack ?? error) } } finally { IDBObjectStore.prototype.put = put }
  })
  assert.equal(aborted.refused, true)
  const partial = await page.evaluate(() => adapter.store.read())
  if (partial.manifest.applied_cursor !== 3) throw Error(`unexpected early failure: ${aborted.error}`)
  assert.equal(partial.rows.at(-1).state, 'received')
  assert.equal(partial.manifest.applied_cursor, 3)
  await page.evaluate(() => adapter.store.close()); await context.close()
  page = await launch(); await attach(page)
  assert.deepEqual(await page.evaluate(() => adapter.store.read()), partial)
  const result = await page.evaluate(() => adapter.run())
  assert.equal(result.kind, 'bounded_peer_content_conflict_preserved')
  assert.equal(result.ready, false)
  assert.equal(result.declarationAllowed, false)
  assert.equal(result.writerAuthorized, false)
  assert.equal(result.projectOpened, false)
  const before = await page.evaluate(() => adapter.store.read())
  assert.equal(before.rows.at(-1).state, 'content_note_conflict_preserved')
  assert.deepEqual(before.rows.map(row => JSON.parse(row.wire_item)), fixture.items ?? fixture.stores.inbox.map(row => JSON.parse(row.wire_item)))
  if (fixture.expectedContents) {
    const bothPreserved = await page.evaluate(async f => {
      const { openHistoryCandidate } = await import('/src/platform/web/noteHistory.ts')
      const state = await adapter.store.read(), group = state.noteHistory.find(row => row.record_kind === 'group')
      if (!group || group.members.length !== 2) return false
      const values = []
      for (const member of group.members) {
        const candidate = state.noteHistory.find(row => row.record_kind === 'candidate' && row.event_id === member.event_id)
        if (!candidate) return false
        const opened = await openHistoryCandidate(f.scope, candidate, Uint8Array.from(f.key))
        values.push(opened.note.content)
      }
      return JSON.stringify(values.sort()) === JSON.stringify([...f.expectedContents].sort())
    }, fixture)
    assert.equal(bothPreserved, true)
  }
  await page.evaluate(() => adapter.store.close()); await context.close()
  page = await launch(); await attach(page)
  assert.equal((await page.evaluate(() => adapter.run())).kind, 'bounded_peer_content_conflict_preserved')
  assert.deepEqual(await page.evaluate(() => adapter.store.read()), before)
  for (const kind of ['proof', 'ciphertext']) {
    const rejected = await page.evaluate(async kind => {
      const original = structuredClone((await adapter.store.read()).rows.at(-1)), damaged = structuredClone(original)
      if (kind === 'proof') damaged.content_note_conflict_proof.outcome_digest = '0'.repeat(64)
      else { const wire = JSON.parse(damaged.wire_item); wire.object.ciphertext = 'AA'; damaged.wire_item = JSON.stringify(wire) }
      const request = indexedDB.open('worta-web-replay', 8)
      await new Promise((resolve, reject) => { request.onsuccess = resolve; request.onerror = () => reject(request.error) })
      const db = request.result
      const put = row => new Promise((resolve, reject) => { const tx = db.transaction('inbox', 'readwrite'); tx.objectStore('inbox').put(row); tx.oncomplete = resolve; tx.onabort = () => reject(tx.error) })
      await put(damaged)
      let refused = false
      try { await adapter.run() } catch { refused = true }
      await put(original); db.close()
      return refused
    }, kind)
    assert.equal(rejected, true)
  }
  assert.deepEqual(await page.evaluate(() => adapter.store.read()), before)
  const oldRefused = await page.evaluate(() => new Promise(resolve => {
    const request = indexedDB.open('worta-web-replay', 5)
    request.onerror = () => resolve(request.error?.name === 'VersionError')
    request.onsuccess = () => { request.result.close(); resolve(false) }
  }))
  assert.equal(oldRefused, true)
  await page.evaluate(() => adapter.store.close())
  console.log(JSON.stringify({ pass: true, scenarios: 4 }))
} finally {
  await context?.close().catch(() => undefined); await vite?.close().catch(() => undefined)
  await rm(directory, { recursive: true, force: true })
}
