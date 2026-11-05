import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { createServer } from 'vite'
import { chromium } from '@playwright/test'

const fixture = JSON.parse(await readFile(process.argv[2], 'utf8'))
const backend = process.argv[3], profile = process.argv[4]
let vite, context
const forbidden = [], recovery = []
try {
  vite = await createServer({ logLevel: 'error', server: { host: '127.0.0.1', port: 0, strictPort: true } }); await vite.listen()
  const origin = `http://127.0.0.1:${vite.httpServer.address().port}`
  const launch = async () => {
    context = await chromium.launchPersistentContext(profile, { headless: true })
    await context.route(url => url.pathname.startsWith('/api/'), async route => {
      const request = route.request(), url = new URL(request.url())
      if (request.method() === 'PUT' && url.pathname.includes('capabilities') || url.pathname.endsWith('/ack') || url.pathname.startsWith('/api/v1/sync/devices/')) {
        forbidden.push(`${request.method()} ${url.pathname}`)
        return route.abort()
      }
      if (url.pathname === '/api/v3/recovery/encrypted/history') recovery.push(url.search)
      // Forward unchanged requests/responses to a real loopback FastAPI server.
      return route.continue({ url: backend + url.pathname + url.search })
    })
    const page = await context.newPage(); await page.goto(origin + '/scripts/browser-cloud-account-harness.html')
    return page
  }
  let page = await launch()
  // Isolated Desktop account key provisioning via its real wrapped-key API.
  // The subsequent user flow receives only a passphrase, never this fixture key.
  await page.evaluate(async raw => {
    const crypto = await import('/src/crypto/index.ts'), { encodeBase64Url } = await import('/src/api/base64url.ts'), { accountCryptoApi } = await import('/src/api/accountCrypto.ts')
    const auth = await (await fetch('/api/v1/auth/login', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ username: 'Arthur', password: 'correct horse battery staple' }) })).json()
    const amk = Uint8Array.from(raw), recovery = await crypto.generateRecoveryKey()
    try {
      const password = await crypto.wrapAmkWithPassphrase(amk, 'document-test-passphrase'), wrapped = await crypto.wrapAmkWithRecoveryKey(amk, recovery)
      await accountCryptoApi.provision(auth.access_token, { password: { ...password, kdf: { ...password.kdf, salt: encodeBase64Url(password.kdf.salt) }, nonce: encodeBase64Url(password.nonce), ciphertext: encodeBase64Url(password.ciphertext) }, recovery: { ...wrapped, nonce: encodeBase64Url(wrapped.nonce), ciphertext: encodeBase64Url(wrapped.ciphertext) } })
    } finally { amk.fill(0); recovery.fill(0) }
  }, fixture.key)
  await context.close(); page = await launch()
  const loginAndOpen = async page => {
    await page.evaluate(() => harness.mountAccountCard())
    const card = page.locator('[data-testid="browser-cloud-account"]')
    await card.locator('input[autocomplete="username"]').fill('Arthur')
    await card.locator('input[autocomplete="current-password"]').fill('correct horse battery staple')
    await card.locator('form button[type="submit"]').click()
    await card.locator('input[autocomplete="off"]').fill('document-test-passphrase')
    await card.locator('form button[type="submit"]').click()
    await card.locator('[data-status="unlocked"]').waitFor()
    await card.locator('[data-testid="cloud-catalog-toggle"]').click()
    await card.locator(`[data-cloud-project-id="${fixture.project}"]`).click()
    const panel = card.locator('[data-testid="cloud-project-read"]')
    await panel.locator('[data-testid="cloud-notes-open"]').waitFor()
    await page.waitForFunction(() => !document.querySelector('[data-testid="cloud-notes-open"]').disabled)
    return panel
  }
  const read = async panel => {
    await panel.locator('[data-testid="cloud-notes-open"]').click()
    await panel.locator(`[data-document-id="${fixture.expected.id}"]`).waitFor()
    assert.equal(await panel.locator('[data-testid="cloud-document-text"]').count(), 0)
    await panel.locator(`[data-document-id="${fixture.expected.id}"]`).click()
    const text = panel.locator('[data-testid="cloud-document-text"]')
    assert.equal((await text.textContent()).trimEnd(), fixture.text)
    assert.equal(await text.locator('script,iframe,img,a').count(), 0)
  }
  const snapshot = page => page.evaluate(async () => {
    const q = indexedDB.open('worta-web-replay', 7)
    const db = await new Promise((ok, no) => { q.onsuccess = () => ok(q.result); q.onerror = () => no(q.error) })
    try { return await new Promise((ok, no) => {
      const tx = db.transaction([...db.objectStoreNames]), rows = {}
      for (const name of db.objectStoreNames) { const r = tx.objectStore(name).getAll(); r.onsuccess = () => { rows[name] = r.result } }
      tx.oncomplete = () => ok(rows); tx.onabort = () => no(tx.error)
    }) } finally { db.close() }
  })
  let panel = await loginAndOpen(page); await read(panel)
  const first = await snapshot(page)
  assert.equal(first.inbox[1].state, 'document_proven'); assert.equal(first.documentLedger.length, 1); assert.equal(first.documentProjection.length, 1)
  assert.deepEqual(first.inbox.map(row => JSON.parse(row.wire_item)), fixture.items)
  assert.equal(first.replayManifest[0].applied_cursor, 2); assert.equal(first.cursors[0].ack_cursor, 0)
  assert.ok(!JSON.stringify(first).includes(fixture.text))
  assert.equal(await page.evaluate(() => window.documentInjected), undefined)
  // Same unlocked session reimports already proven Document source without a duplicate.
  await read(panel); assert.deepEqual(await snapshot(page), first)
  await context.close(); page = await launch(); panel = await loginAndOpen(page); await read(panel)
  assert.deepEqual(await snapshot(page), first)
  // A damaged durable owner proof must clear the UI instead of showing cached text.
  const original = first.inbox[1]
  const replaceProof = async row => page.evaluate(async row => {
    const q = indexedDB.open('worta-web-replay', 7), db = await new Promise((ok, no) => { q.onsuccess = () => ok(q.result); q.onerror = () => no(q.error) })
    try { await new Promise((ok, no) => { const tx = db.transaction('inbox', 'readwrite'); tx.objectStore('inbox').put(row); tx.oncomplete = ok; tx.onabort = no }) } finally { db.close() }
  }, row)
  const damaged = structuredClone(original); damaged.document_proof.ledger_digest = '0'.repeat(64)
  await replaceProof(damaged)
  await panel.locator('[data-testid="cloud-notes-open"]').click(); await panel.locator('[role="alert"]').waitFor()
  assert.equal(await panel.locator('[data-testid="cloud-document-list"]').count(), 0)
  assert.equal(await panel.locator('[data-testid="cloud-document-text"]').count(), 0)
  const bad = await snapshot(page); assert.deepEqual({ ...bad, inbox: first.inbox }, first)
  await replaceProof(original); await read(panel); assert.deepEqual(await snapshot(page), first)
  // Actual lock UI revokes all displayed plaintext.
  await page.locator('[data-testid="cloud-account-lock"]').click()
  await page.locator('[data-testid="browser-cloud-account"] [data-status="locked"]').waitFor()
  assert.equal(await page.locator('[data-testid="cloud-document-text"]').count(), 0)
  assert.deepEqual(forbidden, []); assert.ok(recovery.length > 0)
  console.log(JSON.stringify({ pass: true, nativeDocumentHttp: true, reopen: true, reimport: true, corruptProofRefused: true, recoveryRequests: recovery.length }))
} finally { await context?.close(); await vite?.close() }
