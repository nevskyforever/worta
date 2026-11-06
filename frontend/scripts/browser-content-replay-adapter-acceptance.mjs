import assert from 'node:assert/strict'
import { readFile, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { createServer } from 'vite'
import { chromium } from '@playwright/test'
const fixture = JSON.parse(await readFile(process.argv[2], 'utf8'))
const directory = await mkdtemp(join(tmpdir(), 'worta-content-replay-'))
let context, vite, startupTimer
try {
  const startup = (async () => {
    vite = await createServer({ logLevel: 'error', server: { host: '127.0.0.1', port: 0, strictPort: true } })
    await vite.listen()
    const address = vite.httpServer?.address()
    if (!address || typeof address === 'string') throw Error('vite_missing_listening_address')
    return `http://127.0.0.1:${address.port}`
  })()
  let origin
  try {
    origin = await Promise.race([startup, new Promise((_, reject) => {
      startupTimer = setTimeout(() => reject(Error('vite_startup_timeout: server did not listen within 30000ms')), 30000)
    })])
  } catch (error) {
    // A late listen after timeout must not leave a server behind.
    void startup.then(() => vite?.close()).catch(() => vite?.close())
    throw error
  } finally { clearTimeout(startupTimer) }
  async function launch() {
    context = await chromium.launchPersistentContext(directory, { headless: true })
    const page = await context.newPage(); await page.goto(`${origin}/scripts/browser-cloud-account-harness.html`)
    return page
  }
  let page = await launch()
  await page.evaluate(async f => {
    const { openBrowserReplayStorage } = await import('/src/platform/web/replayStorage.ts')
    const store = await openBrowserReplayStorage(f.scope, new AbortController().signal, () => true); await store.close()
    const open = indexedDB.open('worta-web-replay')
    await new Promise((resolve, reject) => { open.onsuccess = resolve; open.onerror = () => reject(open.error) })
    const db = open.result, tx = db.transaction([...db.objectStoreNames], 'readonly')
    const schema = [...db.objectStoreNames].map(name => { const s = tx.objectStore(name); return { name, keyPath: s.keyPath,
      indexes: [...s.indexNames].map(name => { const i = s.index(name); return { name, keyPath: i.keyPath, unique: i.unique } }) } })
    await new Promise(resolve => { tx.oncomplete = resolve }); db.close()
    const remove = indexedDB.deleteDatabase('worta-web-replay'); await new Promise(resolve => { remove.onsuccess = resolve })
    const old = indexedDB.open('worta-web-replay', 4)
    old.onupgradeneeded = () => { for (const s of schema) { const store = old.result.createObjectStore(s.name, { keyPath: s.keyPath }); for (const i of s.indexes) store.createIndex(i.name, i.keyPath, { unique: i.unique }) } }
    await new Promise((resolve, reject) => { old.onsuccess = resolve; old.onerror = () => reject(old.error) })
    const write = old.result.transaction(schema.map(s => s.name), 'readwrite')
    for (const [name, rows] of Object.entries(f.stores)) for (const row of rows) write.objectStore(name).put(row)
    await new Promise((resolve, reject) => { write.oncomplete = resolve; write.onabort = () => reject(write.error) }); old.result.close()
  }, fixture)
  async function attach(page) {
    return page.evaluate(async f => {
      const { NormalUserAuthRuntime } = await import('/src/auth/userAuth.ts')
      const { cloudProjectsApi } = await import('/src/api/cloudProjects.ts')
      const { encryptedSyncV2Api } = await import('/src/api/encryptedSyncV2.ts')
      const { encryptedSyncV3Api } = await import('/src/api/encryptedSyncV3.ts')
      const { openBrowserReplayStorage } = await import('/src/platform/web/replayStorage.ts')
      const auth = new NormalUserAuthRuntime({ login: async () => ({ access_token: 'isolated-test', refresh_token: 'isolated-test', access_expires_in: 600 }),
        me: async () => ({ id: f.scope.canonicalUserId, username: 'fixture', email: 'fixture@example.test', email_verified: true, role: 'user', status: 'active', created_at: 'now' }), logout: async () => undefined })
      await auth.login('fixture', 'fixture'); const context = auth.requireContext()
      const lease = { localAccountId: f.scope.accountId, canonicalUserId: f.scope.canonicalUserId, authEpoch: context.authEpoch,
        isCurrent: () => auth.isCurrent(context), use: async fn => { const key = Uint8Array.from(f.key); try { return await fn(key) } finally { key.fill(0) } } }
      const bindings = { ensureForCurrentUser: async () => ({ context }) }, keys = { leaseForAccount: () => lease, onInvalidated: () => () => undefined }
      cloudProjectsApi.listBootstraps = async () => f.anchor.registry
      encryptedSyncV2Api.capabilities = async () => f.anchor.transport
      encryptedSyncV3Api.pullRaw = async (_token, _device, since) => JSON.stringify({ protocol_version: 3, encrypted_sync_version: 3,
        items: f.stores.inbox.slice(since).map(row => JSON.parse(row.wire_item)), next_cursor: 3, has_more: false })
      const store = await openBrowserReplayStorage(f.scope, new AbortController().signal, () => true)
      window.adapter = { store, run: () => store.applyContentUpdate(3, auth, bindings, keys),
        orderedRun: () => store.applyOrderedContentUpdate(auth, bindings, keys) }
      return store.read()
    }, fixture)
  }
  // Migration abort must leave the physical v4 and every original record intact.
  const failed = await page.evaluate(async f => {
    const { openBrowserReplayStorage } = await import('/src/platform/web/replayStorage.ts')
    const put = IDBObjectStore.prototype.put
    IDBObjectStore.prototype.put = function(value, ...args) { if (this.name === 'replayManifest' && value.record_version === 6) throw new DOMException('test quota', 'QuotaExceededError'); return put.call(this, value, ...args) }
    try { await openBrowserReplayStorage(f.scope, new AbortController().signal, () => true); return false } catch { return true }
    finally { IDBObjectStore.prototype.put = put }
  }, fixture)
  assert.equal(failed, true)
  const before = await attach(page)
  assert.equal(before.manifest.record_version, 6)
  assert.deepEqual(before.rows, fixture.stores.inbox)
  assert.deepEqual(before.noteHistory, fixture.stores.legacyNoteHistory)
  assert.deepEqual(before.sources, fixture.stores.localProjects)
  assert.deepEqual(before.notes, fixture.stores.localNotes)
  assert.deepEqual(before.owners, fixture.stores.ownerHeads)
  for (const failure of ['quota', 'abort']) {
    const result = await page.evaluate(async failure => {
      const before = await adapter.store.read(), put = IDBObjectStore.prototype.put
      IDBObjectStore.prototype.put = function(value, ...args) {
        if (this.name === 'inbox' && value.state === 'content_note_proven') {
          if (failure === 'abort') this.transaction.abort()
          throw new DOMException('isolated failure', failure === 'quota' ? 'QuotaExceededError' : 'AbortError')
        }
        return put.call(this, value, ...args)
      }
      let refused = false
      try { await adapter.run() } catch { refused = true } finally { IDBObjectStore.prototype.put = put }
      return { refused, same: JSON.stringify(before) === JSON.stringify(await adapter.store.read()) }
    }, failure)
    assert.deepEqual(result, { refused: true, same: true })
  }
  const lost = await page.evaluate(async () => {
    const { BrowserContentNoteApplyRuntime } = await import('/src/platform/web/contentNoteApply.ts')
    const original = BrowserContentNoteApplyRuntime.prototype.commit
    BrowserContentNoteApplyRuntime.prototype.commit = async function(token) { await original.call(this, token); throw Error('isolated lost commit response') }
    try { await adapter.run(); return false } catch { return true }
    finally { BrowserContentNoteApplyRuntime.prototype.commit = original }
  })
  assert.equal(lost, true)
  const applied = await page.evaluate(() => adapter.store.read())
  assert.equal(applied.rows[2].state, 'content_note_proven'); assert.equal(applied.manifest.applied_cursor, 3)
  assert.ok(!JSON.stringify(applied).includes('PRIVATE_'))
  for (const kind of ['ciphertext', 'transport', 'source', 'projection', 'parent']) {
    const refused = await page.evaluate(async kind => {
      const snapshot = await adapter.store.read()
      let name, row
      if (kind === 'source') { name = 'localProjects'; row = structuredClone(snapshot.sources[0]); row.envelope.ciphertext = 'AA' }
      else if (kind === 'projection') { name = 'localNotes'; row = structuredClone(snapshot.notes[0]); row.envelope.ciphertext = 'AA' }
      else if (kind === 'transport') { name = 'legacyNoteHistory'; row = structuredClone(snapshot.noteHistory.find(r => r.record_kind === 'candidate' && r.record_version === 2)); row.transport_proof.proof_digest = '0'.repeat(64) }
      else { name = 'inbox'; row = structuredClone(snapshot.rows[2]); if (kind === 'ciphertext') { const wire = JSON.parse(row.wire_item); wire.object.ciphertext = 'AA'; row.wire_item = JSON.stringify(wire) } else row.content_note_proof.header.parent_event_id = crypto.randomUUID() }
      const original = name === 'localProjects' ? snapshot.sources[0] : name === 'localNotes' ? snapshot.notes[0] : name === 'inbox' ? snapshot.rows[2] : snapshot.noteHistory.find(r => r.record_kind === 'candidate' && r.record_version === 2)
      const open = indexedDB.open('worta-web-replay')
      await new Promise(resolve => { open.onsuccess = resolve }); const db = open.result
      const put = async value => { const tx = db.transaction(name, 'readwrite'); tx.objectStore(name).put(value); await new Promise((resolve, reject) => { tx.oncomplete = resolve; tx.onabort = () => reject(tx.error) }) }
      let rejected = false
      try { await put(row); await adapter.run() } catch { rejected = true }
      finally { await put(original); db.close() }
      return rejected
    }, kind)
    assert.equal(refused, true, kind)
  }
  await context.close()
  page = await launch(); assert.deepEqual(await attach(page), applied)
  assert.equal((await page.evaluate(() => adapter.run())).status, 'already_applied')
  assert.equal((await page.evaluate(() => adapter.orderedRun())).contentProofs, 1)
  assert.deepEqual(await page.evaluate(() => adapter.store.read()), applied)
  const refusal = await page.evaluate(async () => { const old = indexedDB.open('worta-web-replay', 4); return new Promise(resolve => { old.onerror = () => resolve(old.error.name); old.onsuccess = () => { old.result.close(); resolve('unsafe') } }) })
  assert.equal(refusal, 'VersionError')
  console.log(JSON.stringify({ pass: true, scenarios: 15, server: 'mocked ordinary v3 responses; no capability qualification' }))
} finally { try { await context?.close() } finally { try { await vite?.close() } finally { await rm(directory, { recursive: true, force: true }) } } }
