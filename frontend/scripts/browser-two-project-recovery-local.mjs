import assert from 'node:assert/strict'
import { readFile, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { createServer } from 'vite'
import { chromium } from '@playwright/test'

const fixture = JSON.parse(await readFile(process.argv[2], 'utf8'))
const directory = await mkdtemp(join(tmpdir(), 'worta-two-project-'))
let context, vite
try {
  vite = await createServer({ logLevel: 'error', server: { host: '127.0.0.1', port: 0, strictPort: true } }); await vite.listen()
  const origin = `http://127.0.0.1:${vite.httpServer.address().port}`
  const launch = async () => {
    context = await chromium.launchPersistentContext(directory, { headless: true })
    const page = await context.newPage(); await page.goto(`${origin}/scripts/browser-cloud-account-harness.html`)
    await page.evaluate(async f => {
      const { NormalUserAuthRuntime } = await import('/src/auth/userAuth.ts')
      const { cloudProjectsApi } = await import('/src/api/cloudProjects.ts')
      const { encryptedSyncV2Api } = await import('/src/api/encryptedSyncV2.ts')
      const { encryptedSyncV3Api } = await import('/src/api/encryptedSyncV3.ts')
      const { syncApi } = await import('/src/api/sync.ts')
      const { openBrowserReplayStorage } = await import('/src/platform/web/replayStorage.ts')
      const auth = new NormalUserAuthRuntime({ login: async () => ({ access_token: 'local-fixture', refresh_token: 'local-fixture', access_expires_in: 600 }),
        me: async () => ({ id: f.scope.canonicalUserId, username: 'fixture', email: 'fixture@example.test', email_verified: true, role: 'user', status: 'active', created_at: 'now' }), logout: async () => undefined })
      await auth.login('fixture', 'fixture'); const session = auth.requireContext()
      const lease = { localAccountId: f.scope.accountId, canonicalUserId: f.scope.canonicalUserId, authEpoch: session.authEpoch,
        isCurrent: () => auth.isCurrent(session), use: async fn => { const key = Uint8Array.from(f.key); try { return await fn(key) } finally { key.fill(0) } } }
      const bindings = { ensureForCurrentUser: async () => ({ context: session }) }, keys = { leaseForAccount: () => lease, onInvalidated: () => () => undefined }
      syncApi.registerDevice = async (_token, device_id) => ({ protocol_version: 1, device_id, last_ack_cursor: 0 })
      cloudProjectsApi.listBootstraps = async () => f.anchor.registry
      encryptedSyncV2Api.capabilities = async () => f.anchor.transport
      encryptedSyncV3Api.pullRaw = async (_token, _device, since) => JSON.stringify({ protocol_version: 3, encrypted_sync_version: 3,
        items: f.items.slice(since), next_cursor: f.items.length, has_more: false })
      window.store = await openBrowserReplayStorage(f.scope, new AbortController().signal, () => true)
      window.reader = { auth, bindings, keys }
    }, fixture)
    return page
  }
  let page = await launch()
  await page.evaluate(async f => {
    let state = await store.start(f.anchor)
    state = await store.commitPage(state, 0, JSON.stringify({ protocol_version: 3, encrypted_sync_version: 3,
      items: f.items, next_cursor: f.items.length, has_more: false }))
    await store.finish(state)
    for (const project of f.anchor.registry.projects) await store.importRecoveryProject(project.project_id, reader.auth, reader.bindings, reader.keys)
  }, fixture)
  const initial = await page.evaluate(() => store.read())
  assert.equal(initial.bindings.length, 2)
  await page.evaluate(() => store.close()); await context.close(); page = await launch()
  assert.deepEqual(await page.evaluate(() => store.read()), initial)
  const isolated = await page.evaluate(async f => {
    const a = (await store.read()).bindings.find(binding => binding.project_id === 'isolation-A')
    const b = (await store.read()).bindings.find(binding => binding.project_id === 'isolation-B')
    if (!a || !b || a.bootstrap_id === b.bootstrap_id) throw Error('missing_independent_bindings')
    const request = indexedDB.open('worta-web-replay')
    await new Promise((resolve, reject) => { request.onsuccess = resolve; request.onerror = () => reject(request.error) })
    const db = request.result
    const change = action => new Promise((resolve, reject) => { const tx = db.transaction('projectBindings', 'readwrite'); action(tx.objectStore('projectBindings')); tx.oncomplete = resolve; tx.onabort = () => reject(tx.error) })
    await change(s => s.delete([f.scope.accountId, a.project_id]))
    let refused = false
    try { const result = await store.recoverV10(reader.auth, reader.bindings, reader.keys); refused = result.applied.length === 0 && result.blocker?.kind === 'dependency_unavailable' }
    catch { refused = true }
    db.close()
    await store.importRecoveryProject(a.project_id, reader.auth, reader.bindings, reader.keys)
    return refused
  }, fixture)
  assert.equal(isolated, true)
  assert.deepEqual(await page.evaluate(() => store.read()), initial)
  await page.evaluate(() => store.close()); await context.close(); page = await launch()
  assert.deepEqual(await page.evaluate(() => store.read()), initial)
  for (let sequence = 1; sequence <= fixture.items.length; sequence++) {
    const result = await page.evaluate(() => store.recoverV10(reader.auth, reader.bindings, reader.keys))
    assert.equal(result.applied[0]?.sequence, sequence, JSON.stringify(result))
  }
  const complete = await page.evaluate(() => store.read())
  assert.equal(complete.manifest.applied_cursor, fixture.items.length)
  assert.deepEqual(complete.rows.map(row => JSON.parse(row.wire_item)), fixture.items)
  await page.evaluate(() => store.close()); await context.close(); page = await launch()
  assert.deepEqual(await page.evaluate(() => store.read()), complete)
  await page.evaluate(() => store.applyStructure(reader.auth, reader.bindings, reader.keys))
  assert.deepEqual(await page.evaluate(() => store.read()), complete)
  await page.evaluate(() => store.close())
  console.log(JSON.stringify({ pass: true }))
} finally { await context?.close().catch(() => undefined); await vite?.close().catch(() => undefined); await rm(directory, { recursive: true, force: true }) }
