import assert from 'node:assert/strict'
import { readFile, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { createServer } from 'vite'
import { chromium } from '@playwright/test'

const fixture = JSON.parse(await readFile(process.argv[2], 'utf8'))
const directory = await mkdtemp(join(tmpdir(), 'worta-replay-conflict-'))
const expectedCursor = fixture.interruptSequence ? fixture.interruptSequence - 1 : fixture.repeated ? 7 : fixture.successor ? 5 + (fixture.interruptRevision ?? 4) - 4 : 4
const expectedKind = fixture.repeated ? 'bounded_resolution_chain_applied' : fixture.successor ? 'bounded_resolution_continuation_applied' : 'bounded_resolution_owner_applied'
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
      const { BrowserV10TransportRuntime } = await import('/src/platform/web/v10Transport.ts')
      if (!window.pageReplayInstalled) {
        window.pageReplayInstalled = true; window.pageReplayCalls = 0
        const replay = BrowserV10TransportRuntime.prototype.replayInboundHistory
        BrowserV10TransportRuntime.prototype.replayInboundHistory = async function(scope, pages) {
          window.pageReplayCalls++
          if (!window.badPage && pages.length === 1) {
            const whole = JSON.parse(pages[0])
            pages = whole.items.map((item, i) => JSON.stringify({ ...whole, items: [item], next_cursor: i + 1, has_more: i < whole.items.length - 1 }))
          }
          const raw = pages[0]
          if (window.badPage) {
            const page = JSON.parse(raw)
            if (window.badPage === 'wire') page.items[0].object.ciphertext = (page.items[0].object.ciphertext[0] === 'A' ? 'B' : 'A') + page.items[0].object.ciphertext.slice(1)
            if (window.badPage === 'missing') page.items.splice(1, 1)
            if (window.badPage === 'reorder') [page.items[0], page.items[1]] = [page.items[1], page.items[0]]
            if (window.badPage === 'future') page.next_cursor++
            if (window.badPage === 'protocol') { page.protocol_version = 2; page.encrypted_sync_version = 2 }
            pages = [JSON.stringify(page), ...pages.slice(1)]
          }
          const get = IDBObjectStore.prototype.get
          if (window.badPage === 'abort') IDBObjectStore.prototype.get = function(...args) {
            if (this.name === 'inbox') { this.transaction.abort(); throw new DOMException('read abort', 'AbortError') }
            return get.apply(this, args)
          }
          try { return await replay.call(this, scope, pages) } finally { IDBObjectStore.prototype.get = get }
        }
      }
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
      cloudProjectsApi.listBootstraps = async () => {
        if (window.logoutBeforeCommit) await window.readerAuth.auth.logout()
        if (window.recoverySnapshotRace) {
          window.recoverySnapshotRace = false
          const request = indexedDB.open('worta-web-replay', 6)
          await new Promise((resolve, reject) => { request.onsuccess = resolve; request.onerror = () => reject(request.error) })
          const db = request.result
          await new Promise((resolve, reject) => {
            const tx = db.transaction('replayManifest', 'readwrite'), store = tx.objectStore('replayManifest')
            const get = store.get([f.scope.accountId, f.scope.deviceId])
            get.onsuccess = () => store.put({ ...get.result, revision: get.result.revision + 1 })
            tx.oncomplete = resolve; tx.onabort = () => reject(tx.error)
          }); db.close()
        }
        return f.anchor.registry
      }
      encryptedSyncV2Api.capabilities = async () => f.anchor.transport
      const items = f.items ?? f.stores.inbox.map(row => JSON.parse(row.wire_item))
      encryptedSyncV3Api.pullRaw = async (_token, _device, since, limit = 100) => {
        if (window.networkLost) throw new TypeError('connection lost')
        const page = items.slice(since, since + Math.min(limit, 2)), next = since + page.length
        return JSON.stringify({ protocol_version: 3, encrypted_sync_version: 3, items: page, next_cursor: next, has_more: next < items.length })
      }
      let store = await openBrowserReplayStorage(f.scope, new AbortController().signal, () => true)
      if (f.items && !(await store.read()).manifest) {
        let state = await store.start(f.anchor)
        const swapped = [...items]; [swapped[2], swapped[3]] = [swapped[3], swapped[2]]
        let rejected = false
        try { await store.commitPage(state, 0, JSON.stringify({ protocol_version: 3, encrypted_sync_version: 3,
          items: swapped, next_cursor: items.length, has_more: false })) } catch { rejected = true }
        if (!rejected || JSON.stringify(await store.read()) !== JSON.stringify(state)) throw Error('out_of_order_not_atomic')
        if (f.stageRoute) {
          const page = (items, next, more) => JSON.stringify({ protocol_version: 3, encrypted_sync_version: 3, items, next_cursor: next, has_more: more })
          state = await store.commitPage(state, 0, page(items.slice(0, 1), 1, true))
          const note = items.find(item => item.event.entity_type === 'note')
          for (const next of [1, 2]) {
            const before = await store.read()
            let refused = false
            try { await store.commitPage(before, next, page([note], note.event.server_sequence, true)) } catch { refused = true }
            if (!refused || JSON.stringify(await store.read()) !== JSON.stringify(before)) throw Error('missing_stage_not_atomic')
            await store.close()
            store = await openBrowserReplayStorage(f.scope, new AbortController().signal, () => true)
            if (JSON.stringify(await store.read()) !== JSON.stringify(before)) throw Error('missing_stage_reopen_changed')
            state = await store.commitPage(await store.read(), next, page(items.slice(next, next + 1), next + 1, true))
          }
          await store.close()
          store = await openBrowserReplayStorage(f.scope, new AbortController().signal, () => true)
          if (JSON.stringify(await store.read()) !== JSON.stringify(state)) throw Error('stage_prefix_reopen_changed')
          state = await store.commitPage(state, 3, page(items.slice(3), items.length, false))
        } else state = await store.commitPage(state, 0, JSON.stringify({ protocol_version: 3, encrypted_sync_version: 3,
          items, next_cursor: items.length, has_more: false }))
        await store.finish(state)
      }
      window.interruptEventId = f.interruptEventId; window.repeated = !!f.repeated; window.interruptRevision = f.interruptRevision ?? 4; window.successor = !!f.successor; window.readerAuth = { auth, bindings, keys }; window.adapter = { store, run: () => store.applyOrderedContentUpdate(auth, bindings, keys) }
    }, fixture)
  }
  await attach(page)
  if (fixture.stageRoute) {
    const beforeBinding = await page.evaluate(() => adapter.store.read())
    const recovery = await page.evaluate(() => adapter.store.recoverV10(readerAuth.auth, readerAuth.bindings, readerAuth.keys))
    assert.equal(recovery.blocker?.kind, 'dependency_unavailable')
    assert.deepEqual(await page.evaluate(() => adapter.store.read()), beforeBinding)
    assert.equal(await page.evaluate(async f => {
      const { prepareBrowserRemoteProjectImport } = await import('/src/platform/web/projectBindings.ts')
      const foreign = prepareBrowserRemoteProjectImport(f.scope, { project_id: 'foreign-independent-project', bootstrap_id: crypto.randomUUID(), remote_state: 'active', remote_high_water: f.items.length })
      const req = indexedDB.open('worta-web-replay', 6)
      await new Promise((resolve, reject) => { req.onsuccess = resolve; req.onerror = () => reject(req.error) })
      const db = req.result
      const change = (op) => new Promise((resolve, reject) => { const tx = db.transaction('projectBindings', 'readwrite'); op(tx.objectStore('projectBindings')); tx.oncomplete = resolve; tx.onabort = () => reject(tx.error) })
      await change(store => store.add(foreign))
      let refused = false
      try { const result = await adapter.store.recoverV10(readerAuth.auth, readerAuth.bindings, readerAuth.keys); refused = result.applied.length === 0 && result.blocker?.kind === 'dependency_unavailable' } catch { refused = true }
      await change(store => store.delete([f.scope.accountId, foreign.project_id])); db.close()
      return refused
    }, fixture), true)
    assert.deepEqual(await page.evaluate(() => adapter.store.read()), beforeBinding)
    assert.equal(await page.evaluate(async () => {
      try { await adapter.store.importRecoveryProject('foreign-project', readerAuth.auth, readerAuth.bindings, readerAuth.keys); return false }
      catch { return true }
    }), true)
    assert.deepEqual(await page.evaluate(() => adapter.store.read()), beforeBinding)
    await page.evaluate(() => adapter.store.close()); await context.close()
    page = await launch(); await attach(page)
    assert.deepEqual(await page.evaluate(() => adapter.store.read()), beforeBinding)
    await page.evaluate(f => adapter.store.importRecoveryProject(f.items[0].event.project_id, readerAuth.auth, readerAuth.bindings, readerAuth.keys), fixture)
    for (let sequence = 1; sequence <= 3; sequence++) {
      const before = await page.evaluate(() => adapter.store.read())
      if (sequence === 2) {
        assert.equal(await page.evaluate(async () => {
          const row = structuredClone((await adapter.store.read()).rows.find(value => value.server_sequence === 2))
          const damaged = structuredClone(row), wire = JSON.parse(damaged.wire_item)
          wire.event.server_sequence++; damaged.wire_item = JSON.stringify(wire)
          const req = indexedDB.open('worta-web-replay', 6)
          await new Promise((resolve, reject) => { req.onsuccess = resolve; req.onerror = () => reject(req.error) })
          const db = req.result
          const put = value => new Promise((resolve, reject) => { const tx = db.transaction('inbox', 'readwrite'); tx.objectStore('inbox').put(value); tx.oncomplete = resolve; tx.onabort = () => reject(tx.error) })
          await put(damaged)
          let refused = false
          try { const result = await adapter.store.recoverV10(readerAuth.auth, readerAuth.bindings, readerAuth.keys); refused = result.applied.length === 0 } catch { refused = true }
          await put(row); db.close(); return refused
        }), true)
        assert.deepEqual(await page.evaluate(() => adapter.store.read()), before)
        await page.evaluate(() => adapter.store.close()); await context.close()
        page = await launch(); await attach(page)
        assert.deepEqual(await page.evaluate(() => adapter.store.read()), before)
      }
      if (sequence === 2) {
        const refused = await page.evaluate(async () => {
          window.recoverySnapshotRace = true
          try { const value = await adapter.store.recoverV10(readerAuth.auth, readerAuth.bindings, readerAuth.keys); return value.blocker?.kind === 'dependency_unavailable' && value.applied.length === 0 }
          catch { return true } finally { window.recoverySnapshotRace = false }
        })
        assert.equal(refused, true)
        const raced = await page.evaluate(() => adapter.store.read())
        assert.deepEqual(raced.rows, before.rows); assert.deepEqual(raced.owners, before.owners)
        assert.equal(raced.manifest.applied_cursor, before.manifest.applied_cursor)
        await page.evaluate(async manifest => {
          const request = indexedDB.open('worta-web-replay', 6)
          await new Promise((resolve, reject) => { request.onsuccess = resolve; request.onerror = () => reject(request.error) })
          const db = request.result
          await new Promise((resolve, reject) => { const tx = db.transaction('replayManifest', 'readwrite'); tx.objectStore('replayManifest').put(manifest); tx.oncomplete = resolve; tx.onabort = () => reject(tx.error) }); db.close()
        }, before.manifest)
        await page.evaluate(() => adapter.store.close()); await context.close()
        page = await launch(); await attach(page)
        assert.deepEqual(await page.evaluate(() => adapter.store.read()), before)
      }
      if (sequence === 3) {
        assert.equal(await page.evaluate(async () => {
          const row = structuredClone((await adapter.store.read()).rows.find(value => value.state === 'stage_proven'))
          const damaged = structuredClone(row); damaged.stage_proof.outcome_digest = '0'.repeat(64)
          const request = indexedDB.open('worta-web-replay', 6)
          await new Promise((resolve, reject) => { request.onsuccess = resolve; request.onerror = () => reject(request.error) })
          const db = request.result
          const put = value => new Promise((resolve, reject) => { const tx = db.transaction('inbox', 'readwrite'); tx.objectStore('inbox').put(value); tx.oncomplete = resolve; tx.onabort = () => reject(tx.error) })
          await put(damaged)
          let rejected = false
          try { await adapter.store.recoverV10(readerAuth.auth, readerAuth.bindings, readerAuth.keys) } catch { rejected = true }
          await put(row); db.close(); return rejected
        }), true)
        assert.deepEqual(await page.evaluate(() => adapter.store.read()), before)
      }
      for (const fault of ['network', 'abort']) {
        const refused = await page.evaluate(async fault => {
          const put = IDBObjectStore.prototype.put
          if (fault === 'network') window.networkLost = true
          else IDBObjectStore.prototype.put = function(value, ...args) {
            const request = put.call(this, value, ...args)
            if (this.name === 'inbox' && value.state !== 'received') request.addEventListener('success', () => this.transaction.abort(), { once: true })
            return request
          }
          try { await adapter.store.recoverV10(readerAuth.auth, readerAuth.bindings, readerAuth.keys); return false }
          catch { return true }
          finally { window.networkLost = false; IDBObjectStore.prototype.put = put }
        }, fault)
        assert.equal(refused, true)
        assert.deepEqual(await page.evaluate(() => adapter.store.read()), before)
        await page.evaluate(() => adapter.store.close()); await context.close()
        page = await launch(); await attach(page)
        assert.deepEqual(await page.evaluate(() => adapter.store.read()), before)
      }
      if (sequence === 2) {
        assert.equal(await page.evaluate(async () => {
          window.logoutBeforeCommit = true
          try { await adapter.store.recoverV10(readerAuth.auth, readerAuth.bindings, readerAuth.keys); return false }
          catch { return true } finally { window.logoutBeforeCommit = false }
        }), true)
        await context.close(); page = await launch(); await attach(page)
        assert.deepEqual(await page.evaluate(() => adapter.store.read()), before)
      }
      await page.evaluate(async sequence => {
        const result = await adapter.store.recoverV10(readerAuth.auth, readerAuth.bindings, readerAuth.keys)
        if (result.applied.length !== 1 || result.applied[0].sequence !== sequence) throw Error('recovery_sequence')
      }, sequence)
    }
    const imported = await page.evaluate(() => adapter.store.read())
    await page.evaluate(() => adapter.store.close()); await context.close()
    page = await launch(); await attach(page)
    assert.deepEqual(await page.evaluate(() => adapter.store.read()), imported)
    for (let sequence = 4; sequence <= 7; sequence++) {
      if (sequence === 7) {
        const pending = await page.evaluate(() => adapter.store.read())
        assert.equal(pending.rows.find(row => row.server_sequence === 7).state, 'received')
        assert.equal(await page.evaluate(async () => {
          const put = IDBObjectStore.prototype.put
          IDBObjectStore.prototype.put = function(value, ...args) {
            const request = put.call(this, value, ...args)
            if (this.name === 'inbox' && value.state === 'note_resolution_proven') request.addEventListener('success', () => this.transaction.abort(), { once: true })
            return request
          }
          try { await adapter.store.recoverV10(readerAuth.auth, readerAuth.bindings, readerAuth.keys); return false }
          catch { return true } finally { IDBObjectStore.prototype.put = put }
        }), true)
        assert.deepEqual(await page.evaluate(() => adapter.store.read()), pending)
        await page.evaluate(() => adapter.store.close()); await context.close()
        page = await launch(); await attach(page)
        assert.deepEqual(await page.evaluate(() => adapter.store.read()), pending)
      }
      const result = await page.evaluate(() => adapter.store.recoverV10(readerAuth.auth, readerAuth.bindings, readerAuth.keys))
      assert.equal(result.applied[0]?.sequence, sequence, JSON.stringify(result))
      if (sequence === 7) {
        assert.equal(result.applied[0].route, 'resolution')
        assert.ok(result.prefix)
        assert.equal(result.ready, false)
      }
    }
    const resolved = await page.evaluate(() => adapter.store.read())
    await page.evaluate(() => adapter.store.close()); await context.close()
    page = await launch(); await attach(page)
    assert.deepEqual(await page.evaluate(() => adapter.store.read()), resolved)
    if (fixture.repeated && fixture.interruptSequence > 7) {
      const secondResolution = fixture.items.find(item => item.event.entity_type === 'note' && item.event.revision === 6 && item.event.server_sequence > 7)
      if (!secondResolution) throw Error('missing_successor_resolution')
      for (let sequence = 8; sequence <= secondResolution.event.server_sequence; sequence++) {
        if (sequence === secondResolution.event.server_sequence) {
          const before = await page.evaluate(() => adapter.store.read())
          const refused = await page.evaluate(async () => {
            const put = IDBObjectStore.prototype.put
            IDBObjectStore.prototype.put = function(value, ...args) {
              const request = put.call(this, value, ...args)
              if (this.name === 'inbox' && value.state === 'note_resolution_proven') request.addEventListener('success', () => this.transaction.abort(), { once: true })
              return request
            }
            try { await adapter.store.recoverV10(readerAuth.auth, readerAuth.bindings, readerAuth.keys); return false }
            catch { return true } finally { IDBObjectStore.prototype.put = put }
          })
          assert.equal(refused, true); assert.deepEqual(await page.evaluate(() => adapter.store.read()), before)
          await page.evaluate(() => adapter.store.close()); await context.close(); page = await launch(); await attach(page)
          assert.deepEqual(await page.evaluate(() => adapter.store.read()), before)
        }
        const result = await page.evaluate(() => adapter.store.recoverV10(readerAuth.auth, readerAuth.bindings, readerAuth.keys))
        assert.equal(result.applied[0]?.sequence, sequence, JSON.stringify(result))
      }
      const successor = await page.evaluate(() => adapter.store.read())
      assert.equal(successor.rows.find(row => row.server_sequence === secondResolution.event.server_sequence)?.state, 'note_resolution_proven')
      await page.evaluate(() => adapter.store.close()); await context.close(); page = await launch(); await attach(page)
      assert.deepEqual(await page.evaluate(() => adapter.store.read()), successor)
    }
  }
  const aborted = await page.evaluate(async () => {
    const put = IDBObjectStore.prototype.put
    IDBObjectStore.prototype.put = function(value, ...args) {
      if (this.name === 'inbox' && (window.interruptEventId ? ['note_resolution_proven', 'content_note_proven'].includes(value.state) && value.event_id === window.interruptEventId : window.repeated ? value.state === 'note_resolution_proven' && value.resolution_proof.header.revision === 5 : window.successor ? value.state === 'content_note_proven' && value.content_note_proof.header.revision === window.interruptRevision : value.state === 'note_resolution_proven')) { this.transaction.abort(); throw new DOMException('test abort', 'AbortError') }
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
  assert.equal(partial.sources[0].source_revision, fixture.sourceBeforeAbort ?? (fixture.repeated ? 5 : expectedCursor - 1))
  await page.evaluate(() => adapter.store.close()); await context.close()
  page = await launch(); await attach(page)
  assert.deepEqual(await page.evaluate(() => adapter.store.read()), partial)
  if (fixture.finalContent) {
    assert.equal(await page.evaluate(async () => {
      window.networkLost = true
      try { await adapter.run(); return false } catch { return true } finally { window.networkLost = false }
    }), true)
    assert.deepEqual(await page.evaluate(() => adapter.store.read()), partial)
  }
  const result = await page.evaluate(() => adapter.run())
  assert.equal(result.kind, expectedKind)
  assert.equal(result.ready, false)
  assert.equal(result.declarationAllowed, false)
  assert.equal(result.writerAuthorized, false)
  assert.equal(result.projectOpened, false)
  const before = await page.evaluate(() => adapter.store.read())
  if (fixture.stageRoute) {
    for (const [kind, state, proof] of [['stage', 'stage_proven', 'stage_proof'], ['stage_order', 'stage_order_proven', 'stage_order_proof']]) {
      const row = before.rows.find(value => JSON.parse(value.wire_item).event.entity_type === kind)
      assert.equal(row?.state, state)
      assert.ok(row?.[proof])
    }
  }
  assert.ok(await page.evaluate(() => window.pageReplayCalls > 0))
  for (const kind of ['wire', 'missing', 'reorder', 'future', 'protocol', 'abort']) {
    assert.equal(await page.evaluate(async kind => {
      window.badPage = kind
      try { await adapter.run(); return false } catch { return true } finally { window.badPage = undefined }
    }, kind), true)
    assert.deepEqual(await page.evaluate(() => adapter.store.read()), before)
  }

  assert.equal(before.rows.at(-1).state, fixture.finalDeleted || fixture.finalContent || fixture.successor && !fixture.repeated ? 'content_note_proven' : 'note_resolution_proven')
  assert.deepEqual(before.rows.map(row => JSON.parse(row.wire_item)), fixture.items ?? fixture.stores.inbox.map(row => JSON.parse(row.wire_item)))
  if (fixture.selectedNote) {
    const actual = await page.evaluate(async f => {
      const { decryptObjectBytes } = await import('/src/crypto/index.ts')
      const { decodeBase64Url } = await import('/src/api/base64url.ts')
      const state = await adapter.store.read(), row = state.notes[0], envelope = row.envelope
      const bytes = await decryptObjectBytes(Uint8Array.from(f.key), { userId: f.scope.canonicalUserId,
        projectId: row.project_id, entityId: row.note_id, entityType: 'note' },
        { ...envelope, nonce: decodeBase64Url(envelope.nonce), ciphertext: decodeBase64Url(envelope.ciphertext) })
      try { return JSON.parse(new TextDecoder().decode(bytes)).source } finally { bytes.fill(0) }
    }, fixture)
    assert.equal(actual.state, fixture.selectedNote.deleted ? 'tombstone' : 'live')
    if (!fixture.selectedNote.deleted) assert.equal(actual.note.content, fixture.selectedNote.content)
  }
  if (fixture.expectedContents) {
    const bothPreserved = await page.evaluate(async f => {
      const { openHistoryCandidate } = await import('/src/platform/web/noteHistory.ts')
      const genesis = f.items.find(item => item.event.entity_type === 'note' && item.event.revision === 1)
      const state = await adapter.store.read(), group = state.noteHistory.find(row => row.record_kind === 'group' && row.common_parent_event_id === genesis.event.event_id)
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
  if (fixture.stageRoute) {
    for (const kind of ['stage', 'stage_order']) {
      assert.equal(await page.evaluate(async kind => {
        const original = structuredClone((await adapter.store.read()).rows.find(row => JSON.parse(row.wire_item).event.entity_type === kind))
        const damaged = structuredClone(original)
        const proof = kind === 'stage' ? damaged.stage_proof : damaged.stage_order_proof
        proof.outcome_digest = '0'.repeat(64)
        const request = indexedDB.open('worta-web-replay', 6)
        await new Promise((resolve, reject) => { request.onsuccess = resolve; request.onerror = () => reject(request.error) })
        const db = request.result
        const put = row => new Promise((resolve, reject) => { const tx = db.transaction('inbox', 'readwrite'); tx.objectStore('inbox').put(row); tx.oncomplete = resolve; tx.onabort = () => reject(tx.error) })
        await put(damaged)
        let refused = false
        try { await adapter.run() } catch { refused = true }
        await put(original); db.close()
        return refused
      }, kind), true)
      assert.deepEqual(await page.evaluate(() => adapter.store.read()), before)
    }
  }
  for (const kind of ['proof', 'ciphertext']) {
    const rejected = await page.evaluate(async ({ kind, eventId }) => {
      const rows = (await adapter.store.read()).rows
      const original = structuredClone(eventId ? rows.find(row => row.event_id === eventId) : rows.at(-1)), damaged = structuredClone(original)
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
    }, { kind, eventId: fixture.finalContent ? fixture.interruptEventId : undefined })
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
    assert.equal(result.blockers.length, 8)
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
  if (fixture.repeated) {
    const sequences = before.rows.filter(r => r.state === 'note_resolution_proven').map(r => r.server_sequence)
    assert.equal(sequences.length, fixture.expectedResolutionCount ?? 2)
    for (const sequence of sequences) {
      assert.equal(await page.evaluate(async sequence => {
        const state = await adapter.store.read(), original = state.rows.find(r => r.server_sequence === sequence), damaged = structuredClone(original)
        damaged.resolution_proof.outcome_digest = '0'.repeat(64)
        const request = indexedDB.open('worta-web-replay', 6)
        await new Promise((resolve, reject) => { request.onsuccess = resolve; request.onerror = () => reject(request.error) })
        const db = request.result
        const put = value => new Promise((resolve, reject) => { const tx = db.transaction('inbox', 'readwrite'); tx.objectStore('inbox').put(value); tx.oncomplete = resolve; tx.onabort = () => reject(tx.error) })
        await put(damaged)
        let refused = false
        try { await adapter.run() } catch { refused = true }
        await put(original); db.close(); return refused
      }, sequence), true)
      assert.deepEqual(await page.evaluate(() => adapter.store.read()), before)
    }
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
  if(fixture.authorSelfEcho){
    const {acceptReplayAuthor}=await import('./browser-replay-author-local.mjs')
    page=await acceptReplayAuthor({page,fixture,reopen:async(p)=>{await p.evaluate(()=>adapter.store.close());await context.close();const next=await launch();await attach(next);return next}})
  }
  await page.evaluate(() => adapter.store.close())
  console.log(JSON.stringify({ pass: true, scenarios: 10 }))
} finally {
  await context?.close().catch(() => undefined); await vite?.close().catch(() => undefined)
  await rm(directory, { recursive: true, force: true })
}
