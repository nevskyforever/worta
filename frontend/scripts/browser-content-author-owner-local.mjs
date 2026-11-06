// Local owner acceptance only. No capability PUT, server eligibility or user writer composition.
import assert from 'node:assert/strict'
import { readFile, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { createServer } from 'vite'
import { chromium } from '@playwright/test'
const fixture = JSON.parse(await readFile(process.argv[2], 'utf8'))
const profile = await mkdtemp(join(tmpdir(), 'worta-content-author-'))
let vite, context
try {
 vite = await createServer({ logLevel:'error', server:{host:'127.0.0.1',port:0,strictPort:true} }); await vite.listen()
 const origin = `http://127.0.0.1:${vite.httpServer.address().port}`
 const launch = async () => { context = await chromium.launchPersistentContext(profile,{headless:true}); const p=await context.newPage(); await p.goto(`${origin}/scripts/browser-cloud-account-harness.html`); return p }
 let page = await launch()
 const attach = async seed => page.evaluate(async ({f,seed}) => {
  const {NormalUserAuthRuntime}=await import('/src/auth/userAuth.ts')
  const {cloudProjectsApi}=await import('/src/api/cloudProjects.ts')
  const {encryptedSyncV2Api}=await import('/src/api/encryptedSyncV2.ts')
  const {encryptedSyncV3Api}=await import('/src/api/encryptedSyncV3.ts')
  const {syncApi}=await import('/src/api/sync.ts')
  const {openBrowserReplayStorage}=await import('/src/platform/web/replayStorage.ts')
  const {runBrowserTransaction}=await import('/src/platform/web/persistence.ts')
  const {BrowserLocalAuthority}=await import('/src/platform/web/localAuthority.ts')
  const {BrowserContentNoteIntents}=await import('/src/platform/web/contentNoteIntents.ts')
  const {BrowserLegacyNoteApply}=await import('/src/platform/web/legacyNoteApply.ts')
  const {BrowserContentNoteApply}=await import('/src/platform/web/contentNoteApply.ts')
  const {BrowserSyncDurability}=await import('/src/platform/web/syncDurability.ts')
  const auth=new NormalUserAuthRuntime({login:async()=>({access_token:'isolated',refresh_token:'isolated',access_expires_in:600}),me:async()=>({id:f.scope.canonicalUserId,username:'local',email:'local@example.test',email_verified:true,role:'user',status:'active',created_at:'now'}),logout:async()=>{}})
  await auth.login('local','local'); const c=auth.requireContext(),bindings={ensureForCurrentUser:async()=>({context:c})}
  let unlocked=true
  const lease={localAccountId:f.scope.accountId,canonicalUserId:f.scope.canonicalUserId,authEpoch:c.authEpoch,isCurrent:()=>unlocked&&auth.isCurrent(c),use:async fn=>{const key=Uint8Array.from(f.key);try{return await fn(key)}finally{key.fill(0)}}},keys={leaseForAccount:()=>lease,onInvalidated:()=>()=>{}}
  if(seed){
   const items=f.items.slice(0,1),anchor=structuredClone(f.anchor);anchor.registry.current_cursor=1
   cloudProjectsApi.listBootstraps=async()=>anchor.registry;encryptedSyncV2Api.capabilities=async()=>anchor.transport
   syncApi.registerDevice=async(_token,device_id)=>({protocol_version:1,device_id,last_ack_cursor:0})
   encryptedSyncV3Api.pullRaw=async(_token,_device,since)=>JSON.stringify({protocol_version:3,encrypted_sync_version:3,items:items.slice(since),next_cursor:1,has_more:false})
   const replay=await openBrowserReplayStorage(f.scope,new AbortController().signal,()=>true)
   let state=await replay.start(anchor);state=await replay.commitPage(state,0,JSON.stringify({protocol_version:3,encrypted_sync_version:3,items,next_cursor:1,has_more:false}));await replay.finish(state)
   await replay.applyMetadata(auth,bindings,keys);await replay.seedNoteSource(f.items[1].event.project_id,auth,bindings,keys);await replay.close()
   const original=await new Promise((resolve,reject)=>{const r=indexedDB.open('worta-web-replay');r.onsuccess=()=>resolve(r.result);r.onerror=()=>reject(r.error)})
   const names=[...original.objectStoreNames].filter(n=>n!=='replayManifest'),rows={},schema=[]
   await runBrowserTransaction(original,names,'readonly',(tx,result,abort)=>{let count=0;for(const name of names){const s=tx.objectStore(name);schema.push({name,keyPath:s.keyPath,indexes:[...s.indexNames].map(name=>{const i=s.index(name);return{name,keyPath:i.keyPath,unique:i.unique}})});const r=s.getAll();r.onsuccess=()=>{rows[name]=r.result;if(++count===names.length)result()}}});original.close()
   const db=await new Promise((resolve,reject)=>{const r=indexedDB.open('isolated-content-author-owner',1);r.onupgradeneeded=()=>{for(const s of schema){const store=r.result.createObjectStore(s.name,{keyPath:s.keyPath});for(const i of s.indexes)store.createIndex(i.name,i.keyPath,{unique:i.unique})}};r.onsuccess=()=>resolve(r.result);r.onerror=()=>reject(r.error)})
   await runBrowserTransaction(db,names,'readwrite',(tx,result)=>{for(const name of names)for(const row of rows[name])tx.objectStore(name).put(row);result()});db.close()
  }
  const db=await new Promise((resolve,reject)=>{const r=indexedDB.open('isolated-content-author-owner',1);r.onsuccess=()=>resolve(r.result);r.onerror=()=>reject(r.error)})
  const transaction=(_scope,names,mode,schedule)=>runBrowserTransaction(db,names,mode,schedule)
  const local=new BrowserLocalAuthority(transaction),writer=new BrowserContentNoteIntents(transaction,local,()=>true).bind(auth,bindings,keys)
  const content=new BrowserContentNoteApply(new BrowserLegacyNoteApply(transaction,local,()=>true)).bind(auth,bindings,keys),sync=new BrowserSyncDurability(transaction)
  if(seed){
   const {BrowserProjectBindings}=await import('/src/platform/web/projectBindings.ts')
   await new BrowserProjectBindings(transaction).importRemote(f.scope,{project_id:f.items[1].event.project_id,bootstrap_id:f.anchor.registry.projects[0].bootstrap_id,remote_state:'active',remote_high_water:0})
   await sync.commitInboundPage(f.scope,1,JSON.stringify({protocol_version:3,encrypted_sync_version:3,items:[f.items[1]],next_cursor:2,has_more:false}))
   await content.apply(f.scope,2)
  }
  window.test={f,db,writer,content,sync,auth,lock:()=>{unlocked=false},transaction,project:f.items[1].event.project_id,note:f.items[1].event.entity_id}
  window.snapshot=()=>runBrowserTransaction(db,[...db.objectStoreNames],'readonly',(tx,result)=>{const rows={};let count=0;for(const n of db.objectStoreNames){const r=tx.objectStore(n).getAll();r.onsuccess=()=>{rows[n]=r.result;if(++count===db.objectStoreNames.length)result(rows)}}})
  const existing=await snapshot(),candidate=existing.legacyNoteHistory.find(c=>c.record_version===3)
  if(candidate){test.intent=candidate.intent;test.out=existing.outbox.find(o=>o.event_id===candidate.event_id);test.wire=JSON.parse(test.out.raw_body).items[0];test.response={protocol_version:3,encrypted_sync_version:3,current_cursor:3,results:[{event_id:candidate.event_id,server_sequence:3,duplicate:false}]}}
  await writer.authority.authenticate(f.scope)
 },{f:fixture,seed})
 await attach(true)
 const before=await page.evaluate(()=>snapshot())
 await page.evaluate(async()=>{const t=test,s=t.f.scope,source=await t.writer.local.readSnapshot(s,t.project);await t.writer.local.updateNote(s,source.token,t.note,{content:'BROWSER_PRIVATE_AUTHOR'},'2026-11-03T00:00:00.000000Z');t.intent=(await t.writer.list(s,t.project))[0]})
 for(const phase of ['seal','accept','echo']){
  const saved=await page.evaluate(()=>snapshot())
  const failed=await page.evaluate(async phase=>{
   const t=test,s=t.f.scope,id=t.intent.event_id
   if(phase!=='seal'){const rows=await snapshot();t.out=rows.outbox[0];t.wire=JSON.parse(t.out.raw_body).items[0];t.response={protocol_version:3,encrypted_sync_version:3,current_cursor:3,results:[{event_id:id,server_sequence:3,duplicate:false}]}}
   const put=IDBObjectStore.prototype.put
   IDBObjectStore.prototype.put=function(value,...args){const r=put.call(this,value,...args);if(phase==='seal'&&this.name==='outbox'||phase==='accept'&&this.name==='receipts'||phase==='echo'&&this.name==='inbox')r.addEventListener('success',()=>this.transaction.abort(),{once:true});return r}
   try{if(phase==='seal')await t.writer.authority.seal(s,id);else if(phase==='accept')await t.writer.authority.accept(s,id,t.response);else await t.content.apply(s,3);return false}catch{return true}finally{IDBObjectStore.prototype.put=put}
  },phase)
  assert.equal(failed,true);assert.deepEqual(await page.evaluate(()=>snapshot()),saved)
  await page.evaluate(async phase=>{const t=test,s=t.f.scope,id=t.intent.event_id;if(phase==='seal')await t.writer.authority.seal(s,id);else if(phase==='accept'){await t.writer.authority.accept(s,id,t.response);await t.sync.commitInboundPage(s,2,JSON.stringify({protocol_version:3,encrypted_sync_version:3,items:[{event:{...t.wire.event,device_id:s.deviceId,server_sequence:3},object:t.wire.object}],next_cursor:3,has_more:false}))}else{const p=await t.content.prepare(s,3);await t.content.commit(p);if(await t.content.commit(p)!=='already_applied')throw Error('duplicate_commit')}},phase)
  if(phase!=='echo'){const checkpoint=await page.evaluate(()=>snapshot());await page.evaluate(()=>test.db.close());await context.close();page=await launch();await attach(false);assert.deepEqual(await page.evaluate(()=>snapshot()),checkpoint)}
 }
 const retained=await page.evaluate(()=>snapshot())
 assert.equal(retained.inbox[2].state,'content_note_self_echo_proven')
 assert.deepEqual(retained.inbox.slice(0,2),before.inbox)
 assert.deepEqual(retained.legacyNoteHistory.filter(c=>c.record_kind==='candidate'&&c.record_version===2),before.legacyNoteHistory.filter(c=>c.record_kind==='candidate'))
 await page.evaluate(async()=>{if(await test.content.apply(test.f.scope,3)!=='already_applied')throw Error('duplicate_delivery');await test.writer.authority.authenticate(test.f.scope)})
 assert.deepEqual(await page.evaluate(()=>snapshot()),retained)
 assert.equal(JSON.stringify(retained).includes('BROWSER_PRIVATE_AUTHOR'),false)
 await page.evaluate(()=>test.db.close());await context.close();page=await launch();await attach(false)
 assert.deepEqual(await page.evaluate(()=>snapshot()),retained)
 for(const variant of ['ciphertext','receipt','proof','parent','revoked']){
  const code=await page.evaluate(async variant=>{
   const t=test,s=t.f.scope,backup=await snapshot()
   if(variant==='revoked')await t.auth.logout()
   else await t.transaction(s,['inbox','receipts','legacyNoteHistory'],'readwrite',(tx,result)=>{
    if(variant==='ciphertext'){const r=structuredClone(backup.inbox[2]),w=JSON.parse(r.wire_item);w.object.ciphertext=(w.object.ciphertext[0]==='A'?'B':'A')+w.object.ciphertext.slice(1);r.wire_item=JSON.stringify(w);tx.objectStore('inbox').put(r)}
    if(variant==='proof'){const r=structuredClone(backup.inbox[2]);r.content_note_self_echo_proof.outcome_digest='0'.repeat(64);tx.objectStore('inbox').put(r)}
    if(variant==='receipt'){const r=structuredClone(backup.receipts[0]);r.receipt_digest='0'.repeat(64);tx.objectStore('receipts').put(r)}
    if(variant==='parent'){const r=structuredClone(backup.legacyNoteHistory.find(c=>c.record_version===3));r.header.parent_event_id=crypto.randomUUID();tx.objectStore('legacyNoteHistory').put(r)}result()
   })
   const damaged=await snapshot();let failed=false
   try{await t.content.apply(s,3)}catch{failed=true}
   if(JSON.stringify(await snapshot())!==JSON.stringify(damaged))throw Error('negative_mutated')
   if(variant!=='revoked')await t.transaction(s,['inbox','receipts','legacyNoteHistory'],'readwrite',(tx,result)=>{for(const n of ['inbox','receipts','legacyNoteHistory'])for(const r of backup[n])tx.objectStore(n).put(r);result()})
   return failed
  },variant)
  assert.equal(code,true)
 }
 console.log(JSON.stringify({pass:true,scope:'genuine Native metadata/Content parent + browser I2/C3/R2/E2 owners',cases:['seal/accept/echo native IDB abort','exact duplicates','ciphertext/proof preservation','persistent Chromium reopen','corruption/parent/refusal','logout'],serverToBrowser:false,ready:false,declarationAllowed:false,writerAuthorized:false}))
} finally { await context?.close();await vite?.close();await rm(profile,{recursive:true,force:true}) }
