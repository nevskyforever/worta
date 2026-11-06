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
  const launch = async (profile = directory) => {
    context = await chromium.launchPersistentContext(profile, { headless: true })
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
        items: f.items.slice(since,f.anchor.registry.current_cursor), next_cursor: f.anchor.registry.current_cursor, has_more: false })
      encryptedSyncV3Api.recoveryRaw = async (_token,since)=>JSON.stringify({protocol_version:3,encrypted_sync_version:3,items:f.items.slice(since,f.anchor.registry.current_cursor),next_cursor:f.anchor.registry.current_cursor,has_more:false})
      window.store = await openBrowserReplayStorage(f.scope, new AbortController().signal, () => true)
      window.reader = { auth, bindings, keys }
    }, fixture)
    return page
  }
  let page=await launch()
  await page.evaluate(async f=>{
    let state=await store.start(f.anchor)
    state=await store.commitPage(state,0,JSON.stringify({protocol_version:3,encrypted_sync_version:3,items:f.items.slice(0,1),next_cursor:1,has_more:false}))
    await store.finish(state)
    await store.importRecoveryProject(f.project,reader.auth,reader.bindings,reader.keys)
    await store.recoverV10(reader.auth,reader.bindings,reader.keys,'recovery')
  },fixture)
  const populated=await page.evaluate(()=>store.read())
  assert.equal(populated.manifest.applied_cursor,1)
  await page.evaluate(()=>store.close())
  // Recreate the exact populated v6 layout, retaining every original encrypted row.
  await page.evaluate(async()=>{
    const req=q=>new Promise((ok,no)=>{q.onsuccess=()=>ok(q.result);q.onerror=()=>no(q.error)})
    const db=await req(indexedDB.open('worta-web-replay')),saved=[]
    for(const name of db.objectStoreNames){if(name.startsWith('document')||name.startsWith('map'))continue;const tx=db.transaction(name),st=tx.objectStore(name);saved.push({name,keyPath:st.keyPath,indexes:[...st.indexNames].map(n=>{const i=st.index(n);return {name:n,keyPath:i.keyPath,unique:i.unique}}),rows:await req(st.getAll())})}
    db.close();await req(indexedDB.deleteDatabase('worta-web-replay'))
    const q=indexedDB.open('worta-web-replay',6);q.onupgradeneeded=()=>{for(const entry of saved){const st=q.result.createObjectStore(entry.name,{keyPath:entry.keyPath});for(const i of entry.indexes)st.createIndex(i.name,i.keyPath,{unique:i.unique});for(const row of entry.rows)st.put(row)}}
    const old=await req(q);old.close()
  })
  await context.close();page=await launch()
  const migrated=await page.evaluate(()=>store.read())
  assert.deepEqual(migrated,populated)
  await page.evaluate(async f=>{
    const {cloudProjectsApi}=await import('/src/api/cloudProjects.ts'),{encryptedSyncV3Api}=await import('/src/api/encryptedSyncV3.ts')
    f.anchor.registry.current_cursor=2;cloudProjectsApi.listBootstraps=async()=>f.anchor.registry;encryptedSyncV3Api.recoveryRaw=async(_token,since)=>JSON.stringify({protocol_version:3,encrypted_sync_version:3,items:f.items.slice(since),next_cursor:2,has_more:false})
    window.nextAnchor=f.anchor
    const expected=await store.read(),raw=JSON.stringify({protocol_version:3,encrypted_sync_version:3,items:f.items.slice(1),next_cursor:2,has_more:false})
    await store.stageRecoveryExtension(expected,f.anchor,raw,async()=>{})
  },fixture)
  fixture.anchor.registry.current_cursor=2
  const before=await page.evaluate(()=>store.read())
  const aborted=await page.evaluate(async()=>{
    const put=IDBObjectStore.prototype.add;let fired=false
    IDBObjectStore.prototype.add=function(value,...args){const r=put.call(this,value,...args);if(this.name==='documentProjection'){fired=true;r.addEventListener('success',()=>this.transaction.abort(),{once:true})}return r}
    let refused=false;try{await store.recoverV10(reader.auth,reader.bindings,reader.keys,'recovery')}catch{refused=true}finally{IDBObjectStore.prototype.add=put}
    return {fired,refused,after:await store.read()}
  })
  assert.equal(aborted.fired,true);assert.equal(aborted.refused,true);assert.deepEqual(aborted.after,before)
  await page.evaluate(()=>store.close());await context.close();page=await launch()
  const dispatched=await page.evaluate(()=>store.recoverV10(reader.auth,reader.bindings,reader.keys,'recovery'));assert.deepEqual(dispatched.applied,[{sequence:2,route:'document',status:'applied'}])
  const accepted=await page.evaluate(()=>store.read())
  assert.equal(accepted.checkpoint.ack_cursor,0);assert.equal(accepted.checkpoint.next_local_ordinal,before.checkpoint.next_local_ordinal);assert.equal(accepted.manifest.applied_cursor,2);assert.equal(accepted.documents.ledger.length,1);assert.equal(accepted.rows[1].state,'document_proven');assert.equal(accepted.rows[1].wire_item,JSON.stringify(fixture.items[1]))
  assert.deepEqual((await page.evaluate(()=>store.authenticateDocuments(reader.auth,reader.bindings,reader.keys)))[0],fixture.expectedDocument)
  const duplicate=await page.evaluate(()=>store.recoverV10(reader.auth,reader.bindings,reader.keys,'recovery'));assert.deepEqual(duplicate.applied,[])
  assert.deepEqual(await page.evaluate(()=>store.read()),accepted)
  const redelivered=await page.evaluate(async f=>store.commitPage(await store.read(),0,JSON.stringify({protocol_version:3,encrypted_sync_version:3,items:f.items,next_cursor:2,has_more:false})),fixture)
  assert.deepEqual(redelivered,accepted)
  await page.evaluate(()=>store.close());await context.close();page=await launch()
  const reopened=await page.evaluate(()=>store.recoverV10(reader.auth,reader.bindings,reader.keys,'recovery'));assert.deepEqual(reopened.applied,[])
  assert.deepEqual(await page.evaluate(()=>store.read()),accepted)
  assert.deepEqual((await page.evaluate(()=>store.authenticateDocuments(reader.auth,reader.bindings,reader.keys)))[0],fixture.expectedDocument)
  // Migrate a genuinely applied encrypted Document from v7 to v8. A failed
  // versionchange must preserve the entire old profile, including its proofs.
  await page.evaluate(async()=>{
    await store.close()
    const req=q=>new Promise((ok,no)=>{q.onsuccess=()=>ok(q.result);q.onerror=()=>no(q.error)})
    const db=await req(indexedDB.open('worta-web-replay')),saved=[]
    for(const name of db.objectStoreNames){if(name.startsWith('map'))continue;const tx=db.transaction(name),st=tx.objectStore(name);saved.push({name,keyPath:st.keyPath,indexes:[...st.indexNames].map(n=>{const i=st.index(n);return{name:n,keyPath:i.keyPath,unique:i.unique}}),rows:await req(st.getAll())})}
    db.close();await req(indexedDB.deleteDatabase('worta-web-replay'))
    const old=indexedDB.open('worta-web-replay',7);old.onupgradeneeded=()=>{for(const entry of saved){const st=old.result.createObjectStore(entry.name,{keyPath:entry.keyPath});for(const i of entry.indexes)st.createIndex(i.name,i.keyPath,{unique:i.unique});for(const row of entry.rows)st.put(row)}}
    ;(await req(old)).close()
    const create=IDBDatabase.prototype.createObjectStore
    IDBDatabase.prototype.createObjectStore=function(name,...args){if(name==='mapProjection')throw new DOMException('test interrupted upgrade','AbortError');return create.call(this,name,...args)}
    let rejected=false
    try{const {openBrowserReplayStorage}=await import('/src/platform/web/replayStorage.ts');await openBrowserReplayStorage(saved.find(e=>e.name==='identities').rows.map(i=>({accountId:i.local_account_id,canonicalUserId:i.canonical_user_id,deviceId:i.device_id}))[0],new AbortController().signal,()=>true)}catch{rejected=true}finally{IDBDatabase.prototype.createObjectStore=create}
    if(!rejected)throw Error('migration abort was not exercised')
    const unchanged=await req(indexedDB.open('worta-web-replay',7))
    if(unchanged.version!==7||unchanged.objectStoreNames.contains('mapLedger'))throw Error('partial migration')
    for(const entry of saved){const tx=unchanged.transaction(entry.name),rows=await req(tx.objectStore(entry.name).getAll());if(JSON.stringify(rows)!==JSON.stringify(entry.rows))throw Error('migration changed prior data')}
    unchanged.close()
  })
  await context.close();page=await launch()
  assert.deepEqual(await page.evaluate(()=>store.read()),accepted)
  assert.deepEqual((await page.evaluate(()=>store.authenticateDocuments(reader.auth,reader.bindings,reader.keys)))[0],fixture.expectedDocument)
  assert.equal(await page.evaluate(()=>new Promise(resolve=>{const q=indexedDB.open('worta-web-replay',7);q.onerror=()=>resolve(q.error.name);q.onsuccess=()=>{q.result.close();resolve('incorrectly_opened')}})),'VersionError')
  await page.evaluate(async()=>{await store.close();const q=indexedDB.open('worta-web-replay',8);await new Promise(ok=>q.onsuccess=ok);const db=q.result;await new Promise((ok,no)=>{const tx=db.transaction('inbox','readwrite'),st=tx.objectStore('inbox'),r=st.getAll();r.onsuccess=()=>{const row=r.result.find(r=>r.state==='document_proven');row.document_proof.ledger_digest='0'.repeat(64);st.put(row)};tx.oncomplete=ok;tx.onabort=no});db.close()})
  await context.close();page=await launch()
  await assert.rejects(()=>page.evaluate(()=>store.recoverV10(reader.auth,reader.bindings,reader.keys,'recovery')))
  await page.evaluate(()=>store.close());await context.close()
  page=await launch(join(directory,'fresh-history'))
  const full=await page.evaluate(async f=>{
    let state=await store.start(f.anchor)
    state=await store.commitPage(state,0,JSON.stringify({protocol_version:3,encrypted_sync_version:3,items:f.items,next_cursor:2,has_more:false}))
    await store.finish(state);await store.importRecoveryProject(f.project,reader.auth,reader.bindings,reader.keys,'recovery')
    const outcomes=[]
    for(let i=0;i<3;i++){const result=await store.recoverV10(reader.auth,reader.bindings,reader.keys,'recovery');outcomes.push(...result.applied);if(!result.more)break}
    return {outcomes,snapshot:await store.read(),documents:await store.authenticateDocuments(reader.auth,reader.bindings,reader.keys)}
  },fixture)
  assert.deepEqual(full.outcomes.map(row=>row.route),['metadata','document']);assert.equal(full.snapshot.rows[1].state,'document_proven');assert.deepEqual(full.documents[0],fixture.expectedDocument)
  console.log('Native Document recoverV10 dispatch/ownerPort/migration/reopen/abort/redelivery/corrupt proof: PASS')
}finally{await context?.close();await vite?.close();await rm(directory,{recursive:true,force:true})}
