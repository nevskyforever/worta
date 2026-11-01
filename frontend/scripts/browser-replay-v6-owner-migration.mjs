import assert from 'node:assert/strict'
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { chromium } from '@playwright/test'
import { seedSourceAccount } from './replay-source-seed-fixture.mjs'
import { replayV5SessionFixture } from './replay-v1-fixture.mjs'

const origin = new URL(process.argv[2]).origin, backend = new URL(process.argv[3]).origin
assert.equal(new URL(origin).hostname, '127.0.0.1'); assert.equal(new URL(backend).hostname, '127.0.0.1')
const directory = await mkdtemp(join(tmpdir(), 'worta-replay-v6-owner-')), frozen = await replayV5SessionFixture()
let context
async function snapshot(page) {
  return page.evaluate(async () => {
    const req = indexedDB.open('worta-web-replay'); await new Promise((resolve, reject) => { req.onsuccess = resolve; req.onerror = () => reject(req.error) })
    const db = req.result, rows = {}, version = db.version
    await new Promise((resolve, reject) => { const tx = db.transaction([...db.objectStoreNames], 'readonly')
      for (const name of db.objectStoreNames) { const r = tx.objectStore(name).getAll(); r.onsuccess = () => { rows[name] = r.result } }
      tx.oncomplete = resolve; tx.onabort = () => reject(tx.error) }); db.close(); return {version, rows}
  })
}
try {
  context = await chromium.launchPersistentContext(directory, {headless:true})
  await context.route(url => url.pathname.startsWith('/api/'), async route => {
    const url = new URL(route.request().url()); await route.fulfill({response: await route.fetch({url: backend + url.pathname + url.search})})
  })
  let page = await context.newPage(); await page.goto(`${origin}/scripts/browser-cloud-account-harness.html`); await page.waitForFunction(() => !!window.harness)
  await seedSourceAccount(page, 'SourceSeed')
  const identity = await page.evaluate(async url => {
    const {BrowserCloudAccountSession} = await import(/* @vite-ignore */ url)
    window.oldSession = new BrowserCloudAccountSession(); await oldSession.login('SourceSeed', 'correct horse battery staple')
    await oldSession.unlock('encryption-password'); await oldSession.registerCloudReadDevice()
    await oldSession.receiveCloudAccountHistory(new AbortController().signal)
    return oldSession.currentIdentity
  }, frozen.url)
  const outcome = await page.evaluate(async () => oldSession.applyCloudInitialNotes(new AbortController().signal))
  assert.equal(outcome.projectOpened, false)
  const before = await snapshot(page)
  assert.equal(before.version, 5)
  assert.ok(before.rows.replayManifest[0].source_seeds.length > 0)
  assert.ok(before.rows.inbox.some(row => row.state !== 'received'))
  assert.ok(before.rows.ownerHeads.length > 0)
  await page.evaluate(() => oldSession.logout())
  const scope = {accountId: identity.local_account_id, deviceId: identity.device_id, canonicalUserId: before.rows.identities[0].canonical_user_id}
  const opened = await page.evaluate(async scope => {
    const {openBrowserReplayStorage} = await import('/src/platform/web/replayStorage.ts')
    const store = await openBrowserReplayStorage(scope, new AbortController().signal, () => true)
    try { await store.read(); return true } finally { await store.close() }
  }, scope)
  assert.equal(opened, true)
  const after = await snapshot(page), expected = structuredClone(before); expected.version = 6
  expected.rows.replayManifest = expected.rows.replayManifest.map(row => ({...row, record_version:6, contract_version:6}))
  assert.deepEqual(after, expected)
  console.log(JSON.stringify({pass:true, ownerHeads:before.rows.ownerHeads.length, seeds:before.rows.replayManifest[0].source_seeds.length, inboxProofs:before.rows.inbox.filter(row => row.state !== 'received').length}))
} finally { await context?.close(); await frozen.close(); await rm(directory, {recursive:true, force:true}) }
