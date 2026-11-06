import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { createServer } from 'vite'
import { createInterface } from 'node:readline'
import { chromium } from '@playwright/test'

const fixture = JSON.parse(await readFile(process.argv[2], 'utf8'))
const backend = process.argv[3], profile = process.argv[4]
let vite, context
const forbidden = [], recovery = []
let fault = false, selectedStage = false
const initialCursor=fixture.items.length
try {
  vite = await createServer({ logLevel: 'error', server: { host: '127.0.0.1', port: 0, strictPort: true } }); await vite.listen()
  const origin = `http://127.0.0.1:${vite.httpServer.address().port}`
  const launch = async (profilePath = profile) => {
    context = await chromium.launchPersistentContext(profilePath, { headless: true })
    await context.route(url => url.pathname.startsWith('/api/'), async route => {
      const request = route.request(), url = new URL(request.url())
      if (request.method() === 'PUT' && url.pathname.includes('capabilities') || url.pathname.endsWith('/ack') || url.pathname.startsWith('/api/v1/sync/devices/')) {
        forbidden.push(`${request.method()} ${url.pathname}`)
        return route.abort()
      }
      if (url.pathname === '/api/v3/recovery/encrypted/history') recovery.push(url.search)
      if (fault && url.pathname === '/api/v3/recovery/encrypted/history') {
        const response=await route.fetch({url:backend+url.pathname+url.search}),body=await response.json()
        const item=body.items.findLast(item=>item.event.entity_type==='map')
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
  const read = async (panel, expected) => {
    await panel.locator('[data-testid="cloud-notes-open"]').click()
    if(fixture.stageId){
      await panel.locator('[data-testid="cloud-stage-list"]').waitFor()
      if(!selectedStage){
        await panel.locator('[data-stage-id="S2"]').click()
        assert.equal(await panel.locator('[data-testid="cloud-map-read"]').count(),0)
        await panel.locator('[data-stage-id="S1"]').click();selectedStage=true
      }
      assert.equal(await panel.locator('[data-stage-id="S1"]').getAttribute('aria-pressed'),'true')
    }
    await Promise.race([
      panel.locator('[data-testid="cloud-map-read"]').waitFor(),
      panel.locator('[role="alert"]').waitFor().then(async()=>{throw Error('map recovery refused: '+await panel.locator('[role="alert"]').getAttribute('data-read-code'))}),
    ])
    const frame=panel.frameLocator('[data-testid="cloud-map-read"] iframe')
    await frame.getByText(expected.data.nodeData.topic,{exact:true}).waitFor()
    assert.equal(await frame.locator('[contenteditable="true"]').count(),0)
    for(const annotation of Object.values(expected.annotations)) assert.ok((await panel.locator('[data-testid="cloud-map-annotations"]').textContent()).includes(annotation.title))
  }
  const snapshot = page => page.evaluate(async () => {
    const q = indexedDB.open('worta-web-replay', 8)
    const db = await new Promise((ok, no) => { q.onsuccess = () => ok(q.result); q.onerror = () => no(q.error) })
    try { return await new Promise((ok, no) => {
      const tx = db.transaction([...db.objectStoreNames]), rows = {}
      for (const name of db.objectStoreNames) { const r = tx.objectStore(name).getAll(); r.onsuccess = () => { rows[name] = r.result } }
      tx.oncomplete = () => ok(rows); tx.onabort = () => no(tx.error)
    }) } finally { db.close() }
  })
  let panel=await loginAndOpen(page);await read(panel,fixture.map)
  const first=await snapshot(page)
  assert.deepEqual(first.inbox.map(row=>JSON.parse(row.wire_item)),fixture.items)
  assert.equal(first.inbox[initialCursor-1].state,'map_proven');assert.equal(first.mapLedger.length,1);assert.equal(first.mapProjection.length,1)
  assert.equal(first.replayManifest[0].applied_cursor,initialCursor);assert.equal(first.cursors[0].ack_cursor,0)
  assert.ok(!JSON.stringify(first).includes(fixture.map.data.nodeData.topic))
  await read(panel,fixture.map);assert.deepEqual(await snapshot(page),first)
  const input=createInterface({input:process.stdin}),updateLine=new Promise(ok=>input.once('line',ok))
  console.log(JSON.stringify({phase:'ready_for_update'}))
  const update=JSON.parse(await updateLine);input.close()
  fault=true;await panel.locator('[data-testid="cloud-notes-open"]').click();await panel.locator('[role="alert"]').waitFor();fault=false
  assert.deepEqual(await snapshot(page),first);assert.equal(await panel.locator('[data-testid="cloud-map-read"]').count(),0)
  await page.evaluate(eventId=>{
    const add=IDBObjectStore.prototype.add,put=IDBObjectStore.prototype.put
    window.mapAdd=add;window.mapPut=put;window.mapAborted=false
    IDBObjectStore.prototype.add=function(value,...args){
      if(this.name==='mapLedger'&&value.event_id===eventId){const before={};window.mapBefore=before;for(const name of this.transaction.objectStoreNames){const r=this.transaction.objectStore(name).getAll();r.onsuccess=()=>{before[name]=r.result}}}
      return add.call(this,value,...args)
    }
    IDBObjectStore.prototype.put=function(value,...args){const r=put.call(this,value,...args);if(this.name==='mapProjection'&&value.head_event_id===eventId){window.mapAborted=true;r.addEventListener('success',()=>this.transaction.abort(),{once:true})}return r}
  },update.items[initialCursor].event.event_id)
  await panel.locator('[data-testid="cloud-notes-open"]').click();await panel.locator('[role="alert"]').waitFor()
  const abort=await page.evaluate(()=>{IDBObjectStore.prototype.add=window.mapAdd;IDBObjectStore.prototype.put=window.mapPut;return {fired:window.mapAborted,before:window.mapBefore}})
  assert.equal(abort.fired,true);assert.deepEqual(await snapshot(page),abort.before)
  await context.close();page=await launch();panel=await loginAndOpen(page);await read(panel,update.map)
  const accepted=await snapshot(page)
  assert.deepEqual(accepted.inbox.map(row=>JSON.parse(row.wire_item)),update.items)
  assert.equal(accepted.inbox[initialCursor].state,'map_proven');assert.equal(accepted.mapLedger.length,2);assert.equal(accepted.mapProjection.length,1)
  assert.deepEqual(accepted.replayManifest[0].source_seeds,first.replayManifest[0].source_seeds)
  assert.deepEqual(accepted.projectBindings,first.projectBindings)
  assert.deepEqual(accepted.mapLedger.find(row=>row.event_id===update.items[initialCursor].event.event_id).previous_projection,first.mapProjection[0])
  assert.equal(accepted.cursors[0].ack_cursor,0);assert.equal(accepted.replayManifest[0].applied_cursor,initialCursor+1)
  // Independent test assertion compares the encrypted persisted snapshot with
  // the Native owner snapshot; this key is never injected into project opening.
  const projection=await page.evaluate(async({row,key})=>{
    const {decryptObjectBytes}=await import('/src/crypto/index.ts'),{decodeBase64Url}=await import('/src/api/base64url.ts')
    // Canonical account ID is already bound in persisted identity, never inferred from local alias.
    const q=indexedDB.open('worta-web-replay',8),db=await new Promise((ok,no)=>{q.onsuccess=()=>ok(q.result);q.onerror=no})
    const identity=await new Promise(ok=>{const r=db.transaction('identities').objectStore('identities').getAll();r.onsuccess=()=>ok(r.result[0])});db.close()
    const amk=Uint8Array.from(key),nonce=decodeBase64Url(row.envelope.nonce),ciphertext=decodeBase64Url(row.envelope.ciphertext)
    let bytes
    try{bytes=await decryptObjectBytes(amk,{userId:identity.canonical_user_id,projectId:row.project_id,entityId:row.entity_id,entityType:'map'},{...row.envelope,nonce,ciphertext});return JSON.parse(new TextDecoder().decode(bytes)).map}
    finally{amk.fill(0);nonce.fill(0);ciphertext.fill(0);bytes?.fill(0)}
  },{row:accepted.mapProjection[0],key:fixture.key})
  assert.deepEqual(projection,update.map)
  await read(panel,update.map);assert.deepEqual(await snapshot(page),accepted)
  await context.close();page=await launch();panel=await loginAndOpen(page);await read(panel,update.map)
  assert.deepEqual(await snapshot(page),accepted)
  if(fixture.stageId){
    const stage=accepted.inbox.find(row=>row.state==='stage_proven'&&JSON.parse(row.wire_item).event.entity_id==='S1')
    const replace=async row=>page.evaluate(async row=>{const q=indexedDB.open('worta-web-replay',8),db=await new Promise(ok=>q.onsuccess=()=>ok(q.result));await new Promise((ok,no)=>{const tx=db.transaction('inbox','readwrite');tx.objectStore('inbox').put(row);tx.oncomplete=ok;tx.onabort=no});db.close()},row)
    const bad=structuredClone(stage);bad.stage_proof.wire_digest='0'.repeat(64);await replace(bad)
    await panel.locator('[data-testid="cloud-notes-open"]').click();await panel.locator('[role="alert"]').waitFor()
    assert.equal(await panel.locator('[data-testid="cloud-map-read"]').count(),0)
    await replace(stage);await read(panel,update.map);assert.deepEqual(await snapshot(page),accepted)
  }
  await page.evaluate(async()=>{
    const q=indexedDB.open('worta-web-replay',8),db=await new Promise((ok,no)=>{q.onsuccess=()=>ok(q.result);q.onerror=no})
    await new Promise((ok,no)=>{const tx=db.transaction('inbox','readwrite'),s=tx.objectStore('inbox'),r=s.getAll();r.onsuccess=()=>{const row=r.result.find(r=>r.state==='map_proven');row.map_proof.ledger_digest='0'.repeat(64);s.put(row)};tx.oncomplete=ok;tx.onabort=no});db.close()
  })
  await panel.locator('[data-testid="cloud-notes-open"]').click();await panel.locator('[role="alert"]').waitFor()
  assert.equal(await panel.locator('[data-testid="cloud-map-read"]').count(),0)
  assert.deepEqual(forbidden,[]);assert.ok(recovery.length>0)
  console.log(JSON.stringify({pass:true,mapRead:true,mapUpdate:true,reopen:true,rollback:true,corruption:true,nativeProjectionEqual:true}))
} finally { await context?.close();await vite?.close() }
