import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { createServer } from 'vite'
import { createInterface } from 'node:readline'
import { chromium } from '@playwright/test'

const fixture = JSON.parse(await readFile(process.argv[2], 'utf8'))
const backend = process.argv[3], profile = process.argv[4]
let vite, context
const forbidden = [], recovery = []
let fault = false
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
      if (fault && url.pathname === '/api/v3/recovery/encrypted/history') {
        const response = await route.fetch({url:backend+url.pathname+url.search}), body = await response.json()
        const item = body.items.find(item=>item.event.revision===2 && item.event.entity_type==='document')
        if(item){const raw=item.object.ciphertext;item.object.ciphertext=(raw[0]==='A'?'B':'A')+raw.slice(1)}
        return route.fulfill({response,json:body})
      }
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
  const read = async (panel, expectedText = fixture.text) => {
    await panel.locator('[data-testid="cloud-notes-open"]').click()
    await panel.locator(`[data-document-id="${fixture.expected.id}"]`).waitFor()
    if(await panel.locator('[data-testid="cloud-document-text"]').count() === 0) await panel.locator(`[data-document-id="${fixture.expected.id}"]`).click()
    assert.equal(await panel.locator('[data-document-id]').count(), 1)
    const text = panel.locator('[data-testid="cloud-document-text"]')
    assert.equal((await text.textContent()).trimEnd(), expectedText)
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
  let first = await snapshot(page)
  assert.equal(first.inbox[1].state, 'document_proven'); assert.equal(first.documentLedger.length, 1); assert.equal(first.documentProjection.length, 1)
  assert.deepEqual(first.inbox.map(row => JSON.parse(row.wire_item)), fixture.items)
  assert.equal(first.replayManifest[0].applied_cursor, 2); assert.equal(first.cursors[0].ack_cursor, 0)
  assert.ok(!JSON.stringify(first).includes(fixture.text))
  assert.equal(await page.evaluate(() => window.documentInjected), undefined)
  // Same unlocked session reimports already proven Document source without a duplicate.
  await read(panel); assert.deepEqual(await snapshot(page), first)
  if(fixture.updateRequest){
    const input=createInterface({input:process.stdin}), updateLine=new Promise(ok=>input.once('line',ok))
    console.log(JSON.stringify({phase:'ready_for_update'}))
    const update=JSON.parse(await updateLine); input.close()
    // A corrupted new wire must not even stage the continuation or expose text.
    fault=true;await panel.locator('[data-testid="cloud-notes-open"]').click();await panel.locator('[role="alert"]').waitFor();fault=false
    assert.deepEqual(await snapshot(page),first)
    assert.equal(await panel.locator('[data-testid="cloud-document-text"]').count(),0)
    // Capture the real transaction state before its first write, then abort at
    // encrypted projection replacement. No cursor/proof is assigned by the test.
    await page.evaluate(eventId=>{
      window.originalDocumentAdd=IDBObjectStore.prototype.add;window.originalDocumentPut=IDBObjectStore.prototype.put;window.documentUpdateAborted=false
      IDBObjectStore.prototype.add=function(value,...args){
        if(this.name==='documentLedger'&&value.event_id===eventId){
          const before={};window.documentBeforeCommit=before
          for(const name of this.transaction.objectStoreNames){const r=this.transaction.objectStore(name).getAll();r.onsuccess=()=>{before[name]=r.result}}
        }
        return window.originalDocumentAdd.call(this,value,...args)
      }
      IDBObjectStore.prototype.put=function(value,...args){const r=window.originalDocumentPut.call(this,value,...args)
        if(this.name==='documentProjection'&&value.head_event_id===eventId){window.documentUpdateAborted=true;r.addEventListener('success',()=>this.transaction.abort(),{once:true})}return r}
    },update.items[2].event.event_id)
    await panel.locator('[data-testid="cloud-notes-open"]').click();await panel.locator('[role="alert"]').waitFor()
    const aborted=await page.evaluate(()=>{IDBObjectStore.prototype.add=window.originalDocumentAdd;IDBObjectStore.prototype.put=window.originalDocumentPut;return {fired:window.documentUpdateAborted,before:window.documentBeforeCommit}})
    assert.equal(aborted.fired,true);const pending=await snapshot(page);assert.deepEqual(pending,aborted.before)
    assert.deepEqual(pending.documentProjection,first.documentProjection);assert.deepEqual(pending.documentLedger,first.documentLedger)
    assert.equal(pending.inbox[2].state,'received');assert.equal(pending.replayManifest[0].applied_cursor,2)
    assert.deepEqual(pending.inbox.slice(0,2),first.inbox)
    await read(panel,update.text)
    const applied=await snapshot(page)
    assert.equal(applied.documentProjection.length,1);assert.equal(applied.documentLedger.length,2)
    assert.equal(applied.inbox[2].state,'document_proven');assert.equal(applied.replayManifest[0].applied_cursor,3)
    assert.deepEqual(applied.inbox.slice(0,2),first.inbox);assert.deepEqual(applied.inbox.map(row=>JSON.parse(row.wire_item)),update.items)
    assert.deepEqual(applied.documentLedger.find(row=>row.event_id===update.items[2].event.event_id).previous_projection,first.documentProjection[0])
    assert.ok(!JSON.stringify(applied).includes(update.text));assert.equal(applied.cursors[0].ack_cursor,0)
    fixture.text=update.text;fixture.items=update.items;first=applied
    await read(panel);assert.deepEqual(await snapshot(page),first)
  }
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
