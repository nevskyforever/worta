import assert from 'node:assert/strict'
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { chromium } from '@playwright/test'

const origin = new URL(process.argv[2]).origin, backend = new URL(process.argv[3]).origin
assert.equal(new URL(origin).hostname, '127.0.0.1'); assert.equal(new URL(backend).hostname, '127.0.0.1')
const directory = await mkdtemp(join(tmpdir(), 'worta-replay-server-')), checks = [], requests = [], errors = []
let context, phase = 'setup', offline = false, interruptSuffix = false
async function launch() {
  context = await chromium.launchPersistentContext(directory, { headless: true })
  await context.route(url => url.pathname.startsWith('/api/'), async route => {
    const request = route.request(), url = new URL(request.url())
    if (phase === 'receive') requests.push({ path: url.pathname, method: request.method(), body: request.postData() })
    if (url.pathname === '/api/v3/sync/encrypted/pull' && (offline || interruptSuffix && Number(url.searchParams.get('since')) >= 100)) return route.abort('failed')
    // Real HTTP proxy to the real API; no fabricated data responses.
    const response = await route.fetch({ url: backend + url.pathname + url.search })
    await route.fulfill({ response })
  })
  const page = await context.newPage()
  page.on('pageerror', error => errors.push(error.message))
  await page.goto(`${origin}/scripts/browser-cloud-account-harness.html`)
  await page.waitForFunction(() => !!window.harness)
  return page
}
async function login(page, username, register = false) {
  phase = 'setup'
  return page.evaluate(async ({ username, register }) => {
    const { BrowserCloudAccountSession } = await import('/src/platform/web/cloudAccountSession.ts')
    const session = new BrowserCloudAccountSession(); window.receiver = session
    await session.login(username, 'correct horse battery staple')
    let locked
    try { await session.receiveCloudAccountHistory(new AbortController().signal) } catch (e) { locked = e.code }
    await session.unlock('encryption-password')
    if (register) await session.registerCloudReadDevice()
    return { identity: session.currentIdentity, locked }
  }, { username, register })
}
async function cache(page) {
  return page.evaluate(async () => {
    const open = indexedDB.open('worta-web-replay', 7)
    await new Promise((resolve, reject) => { open.onsuccess = resolve; open.onerror = () => reject(open.error) })
    const db = open.result, rows = {}
    try {
      await new Promise((resolve, reject) => {
        const tx = db.transaction([...db.objectStoreNames], 'readonly')
        for (const name of db.objectStoreNames) { const request = tx.objectStore(name).getAll(); request.onsuccess = () => { rows[name] = request.result } }
        tx.oncomplete = resolve; tx.onabort = () => reject(tx.error)
      })
      return rows
    } finally { db.close() }
  })
}
async function legacy(page) {
  return page.evaluate(async () => {
    const out = {}
    for (const name of ['identities', 'accountBindings', 'localProjects', 'localNotes', 'localStages', 'localDocuments', 'inbox', 'outbox', 'ownerHeads', 'cursors', 'projectBindings', 'receipts', 'legacyNoteHistory', 'legacyNoteIntents']) out[name] = await harness.rows(name)
    return out
  })
}
const receive = page => page.evaluate(async () => {
  try { return await receiver.receiveCloudAccountHistory(new AbortController().signal) } catch (e) { return { error: e.code } }
})
try {
  let page = await launch()
  await page.evaluate(() => harness.createRecords(true))
  await page.evaluate(async () => {
    const cryptoApi = await import('/src/crypto/index.ts'), { encodeBase64Url } = await import('/src/api/base64url.ts')
    const { accountCryptoApi } = await import('/src/api/accountCrypto.ts'), { sealProjectMetadataEvent } = await import('/src/cloud/projectMetadataCodec.ts')
    const { sealCatalogEvent } = await import('/src/cloud/accountCatalogCodec.ts')
    const metadata = (await (await fetch('/src/cloud/__fixtures__/projectMetadataV2.json')).json())[0].event
    const folder = await (await fetch('/src/cloud/__fixtures__/accountCatalogV1.json')).json()
    for (const [username, total] of [['Receiver', 102], ['ReplayGate', 1]]) {
      const response = await fetch('/api/v1/auth/login', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ username, password: 'correct horse battery staple' }) })
      if (!response.ok) throw Error('fixture_login')
      const token = (await response.json()).access_token, headers = { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' }
      const api = async (path, method = 'POST', body) => {
        const r = await fetch(path, { method, headers, body: body === undefined ? undefined : JSON.stringify(body) })
        if (!r.ok) throw Error(`fixture_${path}_${r.status}`)
        return r.status === 204 ? undefined : r.json()
      }
      const me = await api('/api/v1/account/me', 'GET'), amk = await cryptoApi.generateAccountMasterKey(), recoveryKey = await cryptoApi.generateRecoveryKey()
      try {
        const password = await cryptoApi.wrapAmkWithPassphrase(amk, 'encryption-password'), recovery = await cryptoApi.wrapAmkWithRecoveryKey(amk, recoveryKey)
        await accountCryptoApi.provision(token, { password: { ...password, kdf: { ...password.kdf, salt: encodeBase64Url(password.kdf.salt) }, nonce: encodeBase64Url(password.nonce), ciphertext: encodeBase64Url(password.ciphertext) },
          recovery: { ...recovery, nonce: encodeBase64Url(recovery.nonce), ciphertext: encodeBase64Url(recovery.ciphertext) } })
        const source = crypto.randomUUID(), projects = ['project', 'second-project'].map(project_id => ({ project_id, bootstrap_id: crypto.randomUUID() }))
        await api(`/api/v1/sync/devices/${source}`, 'PUT')
        await api('/api/v2/sync/encrypted/cutover', 'POST', { expected_cutover_epoch: 0 })
        await api('/api/v3/sync/encrypted/reader-ready', 'POST', { device_id: source, reader_transport_version: 3 })
        await api('/api/v3/sync/encrypted/cutover', 'POST', { expected_cutover_epoch: 1 })
        for (const project of projects) {
          await api(`/api/v1/cloud/projects/${project.project_id}/bootstrap`, 'POST', { bootstrap_id: project.bootstrap_id, device_id: source })
          await api(`/api/v1/cloud/projects/${project.project_id}/bootstrap/complete`, 'POST', { bootstrap_id: project.bootstrap_id, device_id: source, initial_event_count: 0, initial_max_server_sequence: 0 })
        }
        let batch = []
        const flush = async () => { if (batch.length) { await api('/api/v3/sync/encrypted/push', 'POST', { protocol_version: 3, encrypted_sync_version: 3, device_id: source, items: batch }); batch = [] } }
        const heads = new Map()
        for (let index = 0; index < total; index++) {
          const account = index === 100, event = structuredClone(account ? folder : metadata), project = projects[index % 2]
          const parent = heads.get(project.project_id)
          event.header = { ...event.header, account_id: me.id, device_id: source, event_id: crypto.randomUUID() }
          if (account) event.payload.name = 'PRIVATE_REPLAY_ACCOUNT_FOLDER'
          else {
            Object.assign(event.header, { project_id: project.project_id, entity_id: project.project_id, bootstrap_id: project.bootstrap_id,
              operation: parent ? 'update' : 'create', revision: parent ? parent.revision + 1 : 1, generation: parent ? parent.revision + 1 : 1,
              parent_event_ids: parent ? [parent.event_id] : [] })
            event.metadata.name = `PRIVATE_REPLAY_SERVER_${username}_${index}`
            heads.set(project.project_id, event.header)
          }
          const sealed = await (account ? sealCatalogEvent(amk, event) : sealProjectMetadataEvent(amk, event)), h = event.header
          const wire = { event_id: h.event_id, ...(account ? { canonical_user_id: me.id, scope: 'account' } : { project_id: h.project_id }), entity_id: h.entity_id,
            entity_type: account ? 'folder' : 'project_metadata', operation: 'upsert', revision: h.revision, updated_at: h.updated_at, deleted_at: null }
          const item = { event: wire, object: { ...sealed, nonce: encodeBase64Url(sealed.nonce), ciphertext: encodeBase64Url(sealed.ciphertext) } }
          if (account) { await flush(); await api('/api/v3/sync/encrypted/account/push', 'POST', { protocol_version: 3, encrypted_sync_version: 3, device_id: source, items: [item] }) }
          else { batch.push(item); if (batch.length === 100) await flush() }
        }
        await flush()
        if (username === 'ReplayGate') {
          await api('/api/v3/sync/encrypted/compression-reader-capabilities', 'PUT', { device_id: source, compression_id1: true })
          const gate = await api('/api/v3/sync/encrypted/compression-writer', 'POST', { device_id: source }); if (!gate.ready) throw Error('fixture_gate')
        }
      } finally { amk.fill(0); recoveryKey.fill(0) }
    }
  })
  const gateIdentity = (await login(page, 'ReplayGate', true)).identity
  await page.evaluate(() => receiver.logout())
  const firstLogin = await login(page, 'Receiver', true), identity = firstLogin.identity
  assert.equal(firstLogin.locked, 'key_locked'); checks.push('key-required')
  const beforeLegacy = await legacy(page)
  phase = 'receive'; interruptSuffix = true
  assert.deepEqual(await receive(page), { error: 'connection_failed' })
  const partial = await cache(page)
  assert.equal(partial.inbox.length, 100); assert.equal(partial.replayManifest[0].receive_cursor, 100); assert.equal(partial.replayManifest[0].phase, 'receiving')
  checks.push('real-api-multipage-interruption')
  interruptSuffix = false
  const lock = await page.evaluate(async () => {
    const original = window.fetch
    window.fetch = async (...args) => {
      const response = await original(...args), url = new URL(args[0] instanceof Request ? args[0].url : String(args[0]), location.href)
      if (url.pathname === '/api/v3/sync/encrypted/pull' && url.searchParams.get('since') === '100') await receiver.lock()
      return response
    }
    try { await receiver.receiveCloudAccountHistory(new AbortController().signal); return null } catch (e) { return e.code } finally { window.fetch = original }
  })
  assert.equal(lock, 'session_closed'); assert.deepEqual(await cache(page), partial); checks.push('lock-during-real-receive')
  await context.close()
  page = await launch()
  const reopened = await login(page, 'Receiver')
  assert.deepEqual(reopened.identity, identity)
  phase = 'receive'
  const result = await receive(page)
  assert.deepEqual(result, { kind: 'received_ciphertext', cursor: 102, events: 102, generation: partial.replayManifest[0].generation,
    ready: false, declarationAllowed: false, writerAuthorized: false, projectOpened: false, ownerProofs: 0 })
  const complete = await cache(page)
  assert.equal(complete.inbox.length, 102); assert.ok(complete.inbox.every(row => row.state === 'received' && row.record_version === 1))
  assert.equal(new Set(complete.inbox.map(row => row.event_id)).size, 102)
  assert.equal(complete.inbox.filter(row => JSON.parse(row.wire_item).event.scope === 'account').length, 1)
  for (const name of ['outbox', 'receipts', 'ownerHeads', 'legacyNoteHistory', 'legacyNoteIntents', 'localProjects', 'localNotes', 'projectBindings']) assert.deepEqual(complete[name], [])
  assert.ok(complete.cursors.every(row => row.ack_cursor === 0 && row.proven_prefix === 0))
  assert.equal(JSON.stringify(complete).includes('PRIVATE_REPLAY_'), false)
  checks.push('real-chromium-restart-resume', 'mixed-account-project-history-zero-proofs')
  assert.deepEqual(await receive(page), result); assert.deepEqual(await cache(page), complete); checks.push('online-prefix-revalidation-no-duplicates')
  offline = true
  assert.deepEqual(await receive(page), { error: 'connection_failed' }); offline = false; checks.push('no-cached-success-offline')
  phase = 'setup'; await page.evaluate(() => receiver.logout())
  await login(page, 'ReplayGate')
  phase = 'receive'
  assert.deepEqual(await receive(page), { error: 'compression_reader_required' })
  const both = await cache(page)
  assert.equal(both.inbox.filter(row => row.account_id === identity.local_account_id).length, 102)
  assert.equal(both.inbox.filter(row => row.account_id === gateIdentity.local_account_id).length, 0)
  checks.push('real-server-gate-and-account-isolation')
  assert.deepEqual(await legacy(page), beforeLegacy); checks.push('all-v12-stores-unchanged')
  assert.equal(requests.some(r => r.method !== 'GET' || r.body !== null || !['/api/v1/cloud/projects/bootstrap', '/api/v2/sync/encrypted/capabilities', '/api/v3/sync/encrypted/pull'].includes(r.path)), false)
  assert.deepEqual(errors, []); checks.push('no-plaintext-or-ack-push-capability-requests')
  console.log(JSON.stringify({ pass: true, scenarios: checks.length, checks, identities: [identity.device_id, gateIdentity.device_id], readerWrites: 0 }))
} finally { await context?.close(); await rm(directory, { recursive: true, force: true }) }
