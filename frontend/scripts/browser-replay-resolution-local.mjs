import assert from 'node:assert/strict'
import { readFile, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { createServer } from 'vite'
import { chromium } from '@playwright/test'

const fixture = JSON.parse(await readFile(process.argv[2], 'utf8'))
const directory = await mkdtemp(join(tmpdir(), 'worta-replay-conflict-'))
const expectedCursor = fixture.successor ? 5 : 4
const expectedKind = fixture.successor ? 'bounded_resolution_continuation_applied' : 'bounded_resolution_owner_applied'
let context, vite, offlineGate = false, lastGateAuthorization
const gateRequests = []
try {
  vite = await createServer({ logLevel: 'error', server: { host: '127.0.0.1', port: 0, strictPort: true } })
  await vite.listen()
  const address = vite.httpServer.address()
  if (!address || typeof address === 'string') throw Error('vite_address')
  const origin = `http://127.0.0.1:${address.port}`
  const launch = async () => { context = await chromium.launchPersistentContext(directory, { headless: true }); if (fixture.backend) await context.route(url => url.pathname.startsWith('/api/'), async route => {
    const url = new URL(route.request().url())
    if (url.pathname.endsWith('/note-reader-capabilities')) {
      gateRequests.push(route.request().method()); lastGateAuthorization = route.request().headers().authorization
      if (offlineGate) return route.abort('failed')
    }
    const response = await route.fetch({ url: fixture.backend + url.pathname + url.search })
    await route.fulfill({ response })
  }); const page = await context.newPage(); await page.goto(`${origin}/scripts/browser-cloud-account-harness.html`); return page }
  let page = await launch()
  if (fixture.stores) await page.evaluate(async f => {
    const { openBrowserReplayStorage } = await import('/src/platform/web/replayStorage.ts')
    const initial = await openBrowserReplayStorage(f.scope, new AbortController().signal, () => true); await initial.close()
    const open = indexedDB.open('worta-web-replay', 6)
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
      const { syncApi } = await import('/src/api/sync.ts')
      if (!f.backend) syncApi.registerDevice = async (_token, device_id) => ({ protocol_version: 1, device_id, last_ack_cursor: 0 })
      const { openBrowserReplayStorage } = await import('/src/platform/web/replayStorage.ts')
      const auth = f.backend ? new NormalUserAuthRuntime() : new NormalUserAuthRuntime({ login: async () => ({ access_token: 'local-fixture', refresh_token: 'local-fixture', access_expires_in: 600 }),
        me: async () => ({ id: f.scope.canonicalUserId, username: 'fixture', email: 'fixture@example.test', email_verified: true, role: 'user', status: 'active', created_at: 'now' }), logout: async () => undefined })
      await auth.login(f.backend ? 'ReaderQualification' : 'fixture', f.backend ? 'correct horse battery staple' : 'fixture'); const context = auth.requireContext()
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
      window.successor = !!f.successor; window.readerAuth = { auth, bindings, keys }; window.adapter = { store, run: () => store.applyOrderedContentUpdate(auth, bindings, keys) }
    }, fixture)
  }
  await attach(page)
  const aborted = await page.evaluate(async () => {
    const put = IDBObjectStore.prototype.put
    IDBObjectStore.prototype.put = function(value, ...args) {
      if (this.name === 'inbox' && (window.successor ? value.state === 'content_note_proven' && value.content_note_proof.header.revision === 4 : value.state === 'note_resolution_proven')) { this.transaction.abort(); throw new DOMException('test abort', 'AbortError') }
      return put.call(this, value, ...args)
    }
    try { await adapter.run(); return { refused: false } } catch (error) { return { refused: true, error: String(error?.stack ?? error) } } finally { IDBObjectStore.prototype.put = put }
  })
  assert.equal(aborted.refused, true)
  const partial = await page.evaluate(() => adapter.store.read())
  if (partial.manifest.applied_cursor !== expectedCursor) throw Error(`unexpected early failure: ${aborted.error}`)
  assert.equal(partial.rows.at(-1).state, 'received')
  assert.equal(partial.manifest.applied_cursor, expectedCursor)
  assert.equal(partial.noteHistory.some(r => ['resolution', 'group_transition'].includes(r.record_kind)), !!fixture.successor)
  assert.equal(partial.sources[0].source_revision, fixture.successor ? 4 : 3)
  await page.evaluate(() => adapter.store.close()); await context.close()
  page = await launch(); await attach(page)
  assert.deepEqual(await page.evaluate(() => adapter.store.read()), partial)
  const result = await page.evaluate(() => adapter.run())
  assert.equal(result.kind, expectedKind)
  assert.equal(result.ready, false)
  assert.equal(result.declarationAllowed, false)
  assert.equal(result.writerAuthorized, false)
  assert.equal(result.projectOpened, false)
  const before = await page.evaluate(() => adapter.store.read())
  assert.equal(before.rows.at(-1).state, fixture.successor ? 'content_note_proven' : 'note_resolution_proven')
  assert.deepEqual(before.rows.map(row => JSON.parse(row.wire_item)), fixture.items ?? fixture.stores.inbox.map(row => JSON.parse(row.wire_item)))
  if (fixture.selectedNote) {
    const actual = await page.evaluate(async f => {
      const { decryptObjectBytes } = await import('/src/crypto/index.ts')
      const { decodeBase64Url } = await import('/src/api/base64url.ts')
      const state = await adapter.store.read(), row = state.notes[0], envelope = row.envelope
      const bytes = await decryptObjectBytes(Uint8Array.from(f.key), { userId: f.scope.canonicalUserId,
        projectId: row.project_id, entityId: row.note_id, entityType: 'note' },
        { ...envelope, nonce: decodeBase64Url(envelope.nonce), ciphertext: decodeBase64Url(envelope.ciphertext) })
      try { return JSON.parse(new TextDecoder().decode(bytes)).source.note.content } finally { bytes.fill(0) }
    }, fixture)
    assert.equal(actual, fixture.selectedNote.content)
  }
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
  assert.equal((await page.evaluate(() => adapter.run())).kind, expectedKind)
  assert.deepEqual(await page.evaluate(() => adapter.store.read()), before)
  for (const kind of ['proof', 'ciphertext']) {
    const rejected = await page.evaluate(async kind => {
      const original = structuredClone((await adapter.store.read()).rows.at(-1)), damaged = structuredClone(original)
      if (kind === 'proof') (damaged.resolution_proof ?? damaged.content_note_proof).outcome_digest = '0'.repeat(64)
      else { const wire = JSON.parse(damaged.wire_item); wire.object.ciphertext = 'AA'; damaged.wire_item = JSON.stringify(wire) }
      const request = indexedDB.open('worta-web-replay', 6)
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
  for (const kind of ['resolution', 'group_transition', 'head']) {
    assert.equal(await page.evaluate(async kind => {
      const state = await adapter.store.read(), original = structuredClone(state.noteHistory.find(r => r.record_kind === kind)), damaged = structuredClone(original)
      if (kind === 'resolution') damaged.evidence_envelope.ciphertext = 'AA'
      if (kind === 'group_transition') damaged.from_digest = '0'.repeat(64)
      if (kind === 'head') damaged.linear_head_event_id = crypto.randomUUID()
      const request = indexedDB.open('worta-web-replay', 6)
      await new Promise((resolve, reject) => { request.onsuccess = resolve; request.onerror = () => reject(request.error) })
      const db = request.result
      const put = value => new Promise((resolve, reject) => { const tx = db.transaction('legacyNoteHistory', 'readwrite'); tx.objectStore('legacyNoteHistory').put(value); tx.oncomplete = resolve; tx.onabort = () => reject(tx.error) })
      await put(damaged)
      let refused = false
      try { await adapter.run() } catch { refused = true }
      await put(original); db.close()
      return refused
    }, kind), true)
  }
  assert.deepEqual(await page.evaluate(() => adapter.store.read()), before)
  assert.equal(await page.evaluate(async f => {
    const { validateBrowserInboxRecord } = await import('/src/platform/web/syncDurability.ts')
    const { validateNoteHistorySet } = await import('/src/platform/web/noteHistoryRecords.ts')
    const state = await adapter.store.read()
    let inbox = false, history = false
    try { validateBrowserInboxRecord(state.rows.find(r => r.state === 'note_resolution_proven'), f.scope) } catch { inbox = true }
    try { validateNoteHistorySet(f.scope, state.noteHistory) } catch { history = true }
    return inbox && history
  }, fixture), true)
  if (fixture.backend) {
    const result = await page.evaluate(async f => {
      const { auth, bindings, keys } = readerAuth
      return await adapter.store.inspectNoteReaderReadiness(auth, bindings, keys, {checkServerGate:true})
    }, fixture)
    assert.equal(result.serverGate, 'checked_blocked')
    assert.equal(result.ready, false); assert.equal(result.declarationAllowed, false); assert.equal(result.writerAuthorized, false)
    assert.equal(result.blockers.length, 10)
    assert.equal(result.blockers.includes('server_note_gate_not_checked'), true)
    offlineGate = true
    assert.equal(await page.evaluate(async () => {
      try { await adapter.store.inspectNoteReaderReadiness(readerAuth.auth, readerAuth.bindings, readerAuth.keys, {checkServerGate:true}); return false } catch { return true }
    }), true)
    offlineGate = false
    assert.deepEqual(await page.evaluate(() => adapter.store.read()), before)
    await page.evaluate(() => readerAuth.auth.logout())
    assert.equal((await fetch(fixture.backend + '/api/v3/sync/encrypted/note-reader-capabilities', {
      headers: { Authorization: lastGateAuthorization }
    })).status, 401)
    assert.equal(await page.evaluate(async () => {
      try { await adapter.store.inspectNoteReaderReadiness(readerAuth.auth, readerAuth.bindings, readerAuth.keys, {checkServerGate:true}); return false } catch { return true }
    }), true)
    await page.evaluate(() => adapter.store.close())
    await attach(page)
    assert.deepEqual(await page.evaluate(() => adapter.store.read()), before)
    assert.equal(gateRequests.includes('PUT'), false)
    assert.ok(gateRequests.filter(method => method === 'GET').length >= 2)

  }
  const oldRefused = await page.evaluate(() => new Promise(resolve => {
    const request = indexedDB.open('worta-web-replay', 5)
    request.onerror = () => resolve(request.error?.name === 'VersionError')
    request.onsuccess = () => { request.result.close(); resolve(false) }
  }))
  assert.equal(oldRefused, true)
  if (fixture.successor) {
    await page.evaluate(() => readerAuth.auth.logout())
    assert.equal(await page.evaluate(async () => { try { await adapter.run(); return false } catch { return true } }), true)
    await page.evaluate(() => adapter.store.close())
    await attach(page)
    assert.equal((await page.evaluate(() => adapter.run())).kind, expectedKind)
    assert.deepEqual(await page.evaluate(() => adapter.store.read()), before)
  }
  await page.evaluate(() => adapter.store.close())
  console.log(JSON.stringify({ pass: true, scenarios: 10 }))
} finally {
  await context?.close().catch(() => undefined); await vite?.close().catch(() => undefined)
  await rm(directory, { recursive: true, force: true })
}
