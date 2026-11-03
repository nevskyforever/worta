import assert from 'node:assert/strict'
import {readFile,mkdtemp,rm} from 'node:fs/promises'
import {tmpdir} from 'node:os'
import {join} from 'node:path'
import {createServer} from 'vite'
import {chromium} from '@playwright/test'
import {acceptReplayAuthor} from './browser-replay-author-local.mjs'
const fixture=JSON.parse(await readFile(process.argv[2],'utf8')),profile=await mkdtemp(join(tmpdir(),'worta-own-resolution-'))
const original=structuredClone(fixture),own=fixture.ownResolution
fixture.items=fixture.items.slice(0,4);fixture.anchor.registry.current_cursor=4
let context,vite
try{
 vite=await createServer({logLevel:'error',server:{host:'127.0.0.1',port:0,strictPort:true}});await vite.listen()
 const origin=`http://127.0.0.1:${vite.httpServer.address().port}`
 const launch=async()=>{context=await chromium.launchPersistentContext(profile,{headless:true});const p=await context.newPage();await p.goto(`${origin}/scripts/browser-cloud-account-harness.html`);return p}
 const attach=p=>p.evaluate(async f=>{
  const {NormalUserAuthRuntime}=await import('/src/auth/userAuth.ts'),{cloudProjectsApi}=await import('/src/api/cloudProjects.ts'),{encryptedSyncV2Api}=await import('/src/api/encryptedSyncV2.ts'),{encryptedSyncV3Api}=await import('/src/api/encryptedSyncV3.ts'),{syncApi}=await import('/src/api/sync.ts'),{openBrowserReplayStorage}=await import('/src/platform/web/replayStorage.ts')
  const auth=new NormalUserAuthRuntime({login:async()=>({access_token:'isolated',refresh_token:'isolated',access_expires_in:600}),me:async()=>({id:f.scope.canonicalUserId,username:'isolated',email:'fixture@example.test',email_verified:true,role:'user',status:'active',created_at:'now'}),logout:async()=>{}})
  await auth.login('isolated','isolated');const c=auth.requireContext(),bindings={ensureForCurrentUser:async()=>({context:c})},lease={localAccountId:f.scope.accountId,canonicalUserId:f.scope.canonicalUserId,authEpoch:c.authEpoch,isCurrent:()=>auth.isCurrent(c),use:async fn=>{const key=Uint8Array.from(f.key);try{return await fn(key)}finally{key.fill(0)}}},keys={leaseForAccount:()=>lease,onInvalidated:()=>()=>{}}
  cloudProjectsApi.listBootstraps=async()=>f.anchor.registry;encryptedSyncV2Api.capabilities=async()=>f.anchor.transport;syncApi.registerDevice=async(_token,device_id)=>({protocol_version:1,device_id,last_ack_cursor:0})
  encryptedSyncV3Api.pullRaw=async(_token,_device,since)=>JSON.stringify({protocol_version:3,encrypted_sync_version:3,items:f.items.slice(since),next_cursor:f.items.length,has_more:false})
  const store=await openBrowserReplayStorage(f.scope,new AbortController().signal,()=>true)
  if(!(await store.read()).manifest){let state=await store.start(f.anchor);state=await store.commitPage(state,0,JSON.stringify({protocol_version:3,encrypted_sync_version:3,items:f.items,next_cursor:f.items.length,has_more:false}));await store.finish(state)}
  window.readerAuth={auth,bindings,keys};window.adapter={store,run:()=>store.applyOrderedContentUpdate(auth,bindings,keys)};window.scope=f.scope
 },fixture)
 let page=await launch();await attach(page);await page.evaluate(()=>adapter.run())
 const before=await page.evaluate(()=>adapter.store.read())
 assert.equal(before.rows[3].state,'content_note_conflict_preserved')
 const failWrite=async(phase,action)=>page.evaluate(async({phase,raw,id,response})=>{
   const put=IDBObjectStore.prototype.put,add=IDBObjectStore.prototype.add;let fired=false
   const wrap=original=>function(v,...args){const r=original.call(this,v,...args);if(phase==='sealed'&&this.name==='outbox'||phase==='receipt'&&this.name==='receipts'||phase==='source'&&this.name==='localNotes'||phase==='group'&&this.name==='legacyNoteHistory'&&v.record_kind==='group_transition'||['outcome','revoked'].includes(phase)&&this.name==='inbox'&&v.state==='note_resolution_proven'||phase==='manifest'&&this.name==='replayManifest'&&v.applied_cursor===5){fired=true;r.addEventListener('success',()=>{if(phase==='revoked')void readerAuth.auth.logout();else this.transaction.abort()},{once:true})}return r}
   IDBObjectStore.prototype.put=wrap(put);IDBObjectStore.prototype.add=wrap(add)
   try{const owner=adapter.store.contentAuthorOwner(readerAuth.auth,readerAuth.bindings,readerAuth.keys).authority;if(phase==='sealed')await owner.stageResolution(scope,raw);else if(phase==='receipt')await owner.acceptResolution(scope,id,response);else await adapter.run();return false}catch(e){if(!fired)throw e;return true}finally{IDBObjectStore.prototype.put=put;IDBObjectStore.prototype.add=add}
 },{phase,raw:own.rawBody,id:original.items[4].event.event_id,response:own.response})
 assert.equal(await page.evaluate(async raw=>{const w=JSON.parse(raw);w.device_id=crypto.randomUUID();try{await adapter.store.contentAuthorOwner(readerAuth.auth,readerAuth.bindings,readerAuth.keys).authority.stageResolution(scope,JSON.stringify(w));return false}catch{return true}},own.rawBody),true)
 assert.deepEqual(await page.evaluate(()=>adapter.store.read()),before)
 assert.equal(await failWrite('sealed'),true);assert.deepEqual(await page.evaluate(()=>adapter.store.read()),before)
 await page.evaluate(async raw=>adapter.store.contentAuthorOwner(readerAuth.auth,readerAuth.bindings,readerAuth.keys).authority.stageResolution(scope,raw),own.rawBody)
 const sealed=await page.evaluate(()=>adapter.store.read());assert.equal(sealed.author.out[0].raw_body,own.rawBody);assert.deepEqual(sealed.sources,before.sources)
 fixture.items=original.items;fixture.anchor.registry.current_cursor=5
 const reopen=async p=>{await p.evaluate(()=>adapter.store.close());await context.close();const next=await launch();await attach(next);return next}
 page=await reopen(page)
 assert.equal(await page.evaluate(async()=>{try{await adapter.store.receiveContentAuthorEcho(readerAuth.auth,readerAuth.bindings,readerAuth.keys);return false}catch{return true}}),true)
 assert.deepEqual(await page.evaluate(()=>adapter.store.read()),sealed)
 assert.equal(await failWrite('receipt'),true);assert.deepEqual(await page.evaluate(()=>adapter.store.read()),sealed)
 await page.evaluate(async({id,response})=>{const p=adapter.store.contentAuthorOwner(readerAuth.auth,readerAuth.bindings,readerAuth.keys).authority.acceptResolution(scope,id,response);response.results[0].server_sequence+=100;return p},{id:original.items[4].event.event_id,response:own.response})
 await page.evaluate(()=>adapter.store.receiveContentAuthorEcho(readerAuth.auth,readerAuth.bindings,readerAuth.keys))
 const staged=await page.evaluate(()=>adapter.store.read())
 for(const phase of ['source','group','outcome','manifest']){assert.equal(await failWrite(phase),true);assert.deepEqual(await page.evaluate(()=>adapter.store.read()),staged)}
 assert.equal(await failWrite('revoked'),true);page=await reopen(page);assert.deepEqual(await page.evaluate(()=>adapter.store.read()),staged)
 await page.evaluate(()=>adapter.run());const applied=await page.evaluate(()=>adapter.store.read())
 assert.equal(applied.rows[4].state,'note_resolution_proven');assert.equal(applied.manifest.applied_cursor,5)
 assert.equal(applied.noteHistory.find(r=>r.record_kind==='resolution').source_device_id,fixture.scope.deviceId)
 const selected=await page.evaluate(async()=>{const local=adapter.store.contentAuthorOwner(readerAuth.auth,readerAuth.bindings,readerAuth.keys).local,s=await adapter.store.read(),project=s.sources[0].project_id,source=await local.readSnapshot(scope,project);return local.withDecryptedSnapshot(scope,source.token,async value=>{const {revision:_revision,...note}=value.notes[0].note;return note})});assert.deepEqual(selected,fixture.selectedNote)
 assert.equal(JSON.stringify(applied).includes('NATIVE_PRIVATE_'),false)
 await page.evaluate(()=>adapter.run());assert.deepEqual(await page.evaluate(()=>adapter.store.read()),applied)
 page=await reopen(page);await page.evaluate(()=>adapter.run());assert.deepEqual(await page.evaluate(()=>adapter.store.read()),applied)
 const originalWire=original.items[4]
 assert.equal(applied.rows[4].wire_item,JSON.stringify(originalWire))
 assert.deepEqual(applied.author.receipts[0],{record_version:1,account_id:fixture.scope.accountId,device_id:fixture.scope.deviceId,event_id:originalWire.event.event_id,protocol_version:3,server_sequence:own.response.results[0].server_sequence,duplicate:own.response.results[0].duplicate})
 await page.evaluate(async({id,response,raw})=>{const a=adapter.store.contentAuthorOwner(readerAuth.auth,readerAuth.bindings,readerAuth.keys).authority;await a.stageResolution(scope,raw);if(await a.acceptResolution(scope,id,response)!=='already_accepted')throw Error('duplicate_receipt')},{id:originalWire.event.event_id,response:own.response,raw:own.rawBody})
 assert.deepEqual(await page.evaluate(()=>adapter.store.read()),applied)
 for(const kind of ['proof','author','group','receipt','ciphertext','parent_proof']){
  assert.equal(await page.evaluate(async kind=>{
    const q=indexedDB.open('worta-web-replay',6);await new Promise((resolve,reject)=>{q.onsuccess=resolve;q.onerror=()=>reject(q.error)});const db=q.result
    const snapshot=()=>new Promise((resolve,reject)=>{const tx=db.transaction([...db.objectStoreNames]),v={};let n=0;for(const name of db.objectStoreNames){const r=tx.objectStore(name).getAll();r.onsuccess=()=>{v[name]=r.result;if(++n===db.objectStoreNames.length)resolve(v)}};tx.onabort=()=>reject(tx.error)})
    const before=await snapshot(),name=kind==='receipt'?'receipts':['author','group'].includes(kind)?'legacyNoteHistory':'inbox'
    const row=structuredClone(before[name].find(r=>kind==='receipt'?r.record_version===1:['author','group'].includes(kind)?r.record_kind==='resolution':r.server_sequence===(kind==='parent_proof'?3:5)))
    if(kind==='proof')row.resolution_proof.outcome_digest='0'.repeat(64)
    if(kind==='author')row.source_device_id=crypto.randomUUID()
    if(kind==='group')row.local_group_id=crypto.randomUUID()
    if(kind==='receipt')row.server_sequence++
    if(kind==='parent_proof')row.content_note_proof.outcome_digest='0'.repeat(64)
    if(kind==='ciphertext'){const w=JSON.parse(row.wire_item);w.object.ciphertext=(w.object.ciphertext[0]==='A'?'B':'A')+w.object.ciphertext.slice(1);row.wire_item=JSON.stringify(w)}
    const write=rows=>new Promise((resolve,reject)=>{const tx=db.transaction(name,'readwrite');for(const r of rows)tx.objectStore(name).put(r);tx.oncomplete=resolve;tx.onabort=()=>reject(tx.error)})
    await write([row]);const damaged=await snapshot();let rejected=false;try{await adapter.run()}catch{rejected=true}
    if(JSON.stringify(await snapshot())!==JSON.stringify(damaged))throw Error('negative_changed_inventory')
    await write(before[name]);db.close();return rejected
  },kind),true)
  assert.deepEqual(await page.evaluate(()=>adapter.store.read()),applied)
 }
 await page.evaluate(()=>readerAuth.auth.logout());assert.equal(await page.evaluate(async()=>{try{await adapter.run();return false}catch{return true}}),true)
 page=await reopen(page);await page.evaluate(()=>adapter.run());assert.deepEqual(await page.evaluate(()=>adapter.store.read()),applied)
 page=await acceptReplayAuthor({page,fixture,reopen})
 await page.evaluate(()=>adapter.store.close());console.log(JSON.stringify({pass:true,ownResolution:true}))
}finally{await context?.close();await vite?.close();await rm(profile,{recursive:true,force:true})}
