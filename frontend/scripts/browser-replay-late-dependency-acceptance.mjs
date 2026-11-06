import assert from 'node:assert/strict'
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { chromium } from '@playwright/test'
import { seedStructuralAccount } from './replay-structural-fixture.mjs'

const origin = new URL(process.argv[2]).origin, backend = new URL(process.argv[3]).origin
assert.equal(new URL(origin).hostname, '127.0.0.1')
assert.equal(new URL(backend).hostname, '127.0.0.1')
const directory = await mkdtemp(join(tmpdir(), 'worta-late-dependency-'))
let context, reading = false, interrupt = false
const requests = []
async function launch() {
  context = await chromium.launchPersistentContext(directory, { headless: true })
  await context.route(url => url.pathname.startsWith('/api/'), async route => {
    const request = route.request(), url = new URL(request.url())
    if (reading && !url.pathname.startsWith('/api/v1/auth/')) requests.push([request.method(), url.pathname])
    if (interrupt && url.pathname === '/api/v3/sync/encrypted/pull') return route.abort('failed')
    const response = await route.fetch({ url: backend + url.pathname + url.search })
    await route.fulfill({ response })
  })
  const page = await context.newPage()
  await page.goto(`${origin}/scripts/browser-cloud-account-harness.html`)
  await page.waitForFunction(() => !!window.harness)
  return page
}
async function login(page, register = false) {
  reading = false
  await page.evaluate(async register => {
    const { BrowserCloudAccountSession } = await import('/src/platform/web/cloudAccountSession.ts')
    window.reader = new BrowserCloudAccountSession()
    await reader.login('LateDependency', 'correct horse battery staple')
    await reader.unlock('encryption-password')
    if (register) await reader.registerCloudReadDevice()
  }, register)
  reading = true
}
const apply = page => page.evaluate(async () => {
  try { return await reader.applyCloudAccountStructure(new AbortController().signal) }
  catch (error) { return { error: error.code ?? error.message } }
})
const cache = page => page.evaluate(async () => {
  const open = indexedDB.open('worta-web-replay')
  await new Promise((resolve, reject) => { open.onsuccess = resolve; open.onerror = () => reject(open.error) })
  const db = open.result, rows = {}
  try {
    await new Promise((resolve, reject) => {
      const tx = db.transaction([...db.objectStoreNames], 'readonly')
      for (const name of db.objectStoreNames) tx.objectStore(name).getAll().onsuccess = event => { rows[name] = event.target.result }
      tx.oncomplete = resolve; tx.onabort = () => reject(tx.error)
    })
    return rows
  } finally { db.close() }
})
try {
  let page = await launch()
  await page.evaluate(() => harness.createRecords(true))
  const fixture = await seedStructuralAccount(page, 'LateDependency', 'late_metadata_dependency')
  assert.equal(fixture.total, 3)
  await login(page, true)
  const failed = await apply(page)
  assert.deepEqual(failed, { error: 'missing_history' })
  const saved = await cache(page)
  assert.equal(saved.inbox.length, 3)
  assert.deepEqual(saved.inbox.map(row => JSON.parse(row.wire_item)), fixture.wire)
  assert.deepEqual(saved.inbox.map(row => row.state), ['applied', 'received', 'received'])
  assert.equal(saved.replayManifest[0].receive_cursor, 3)
  assert.equal(saved.replayManifest[0].applied_cursor, 1)
  assert.equal(saved.ownerHeads.length, 1)
  await context.close()
  page = await launch(); await login(page)
  assert.deepEqual(await cache(page), saved)
  interrupt = true
  assert.ok((await apply(page)).error)
  assert.deepEqual(await cache(page), saved)
  interrupt = false
  assert.deepEqual(await apply(page), failed)
  assert.deepEqual(await cache(page), saved)
  assert.ok(requests.some(([method, path]) => method === 'GET' && path === '/api/v3/sync/encrypted/pull'))
  assert.ok(requests.every(([method]) => method === 'GET'))
  console.log(JSON.stringify({ pass: true, blocker: failed.error, events: 3, ownerProofs: 1,
    restart: true, interruptedReconciliation: true, readerWrites: 0,
    projectResultReturned: Object.hasOwn(failed, 'projectOpened') }))
} finally { await context?.close(); await rm(directory, { recursive: true, force: true }) }
