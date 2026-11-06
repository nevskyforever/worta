import assert from 'node:assert/strict'
export async function acceptReplayAuthor({page,fixture,reopen,recovery=false}){
 const before=await page.evaluate(()=>adapter.store.read())
 await page.evaluate(async()=>{
  const a=readerAuth
  window.authorWriter=adapter.store.contentAuthorOwner(a.auth,a.bindings,a.keys)
 })
 const abortLifecycle=async(phase)=>page.evaluate(async({phase,s,project,note})=>{
   const put=IDBObjectStore.prototype.put,add=IDBObjectStore.prototype.add;let fired=false
   const wrap=original=>function(value,...args){const r=original.call(this,value,...args);if(phase==='intent'&&this.name==='legacyNoteIntents'||phase==='seal'&&this.name==='outbox'||phase==='receipt'&&this.name==='receipts'||phase==='receive'&&this.name==='inbox'&&value.server_sequence===6||phase==='manifest'&&this.name==='replayManifest'&&value.applied_cursor===6){fired=true;r.addEventListener('success',()=>this.transaction.abort(),{once:true})}return r};IDBObjectStore.prototype.put=wrap(put);IDBObjectStore.prototype.add=wrap(add)
   let rejected=false
   try{if(phase==='intent'){const source=await authorWriter.local.readSnapshot(s,project);await authorWriter.local.updateNote(s,source.token,note,{content:'ABORTED_AUTHOR'},'2026-11-03T00:00:00.000000Z')}
     else if(phase==='seal')await authorWriter.authority.seal(s,authorIntent.event_id)
     else if(phase==='receipt')await authorWriter.authority.accept(s,authorIntent.event_id,{protocol_version:3,encrypted_sync_version:3,current_cursor:6,results:[{event_id:authorIntent.event_id,server_sequence:6,duplicate:false}]})
     else if(phase==='receive')await adapter.store.receiveContentAuthorEcho(readerAuth.auth,readerAuth.bindings,readerAuth.keys)
     else await adapter.run()
   }catch{rejected=true}finally{IDBObjectStore.prototype.put=put;IDBObjectStore.prototype.add=add}
   return fired&&rejected
 },{phase,s:fixture.scope,project:fixture.items[1].event.project_id,note:fixture.items[1].event.entity_id})
 assert.equal(await abortLifecycle('intent'),true);assert.deepEqual(await page.evaluate(()=>adapter.store.read()),before)
 const project=fixture.items[1].event.project_id,note=fixture.items[1].event.entity_id
 await page.evaluate(async({s,project,note})=>{
  const w=authorWriter,source=await w.local.readSnapshot(s,project)
  await w.local.updateNote(s,source.token,note,{content:'BROWSER_AUTHOR_AFTER_RESOLUTION'},'2026-11-03T00:00:00.000000Z')
  window.authorIntent=(await w.list(s,project))[0]
 },{s:fixture.scope,project,note})
 const intent=await page.evaluate(()=>authorIntent)
 assert.equal(intent.parent_event_id,before.rows[4].event_id)
 const pending=await page.evaluate(()=>adapter.store.read());assert.equal(await abortLifecycle('seal'),true);assert.deepEqual(await page.evaluate(()=>adapter.store.read()),pending)
 await page.evaluate(async s=>{await authorWriter.authority.seal(s,authorIntent.event_id)},fixture.scope)
 const sealed=await page.evaluate(()=>adapter.store.read()),out=sealed.author.out.find(o=>o.event_id===intent.event_id),wire=JSON.parse(out.raw_body).items[0]
 fixture.items.push({event:{...wire.event,device_id:fixture.scope.deviceId,server_sequence:6},object:wire.object});fixture.anchor.registry.current_cursor=6
 assert.equal(await abortLifecycle('receipt'),true);assert.deepEqual(await page.evaluate(()=>adapter.store.read()),sealed)
 await page.evaluate(async({s,id})=>authorWriter.authority.accept(s,id,{protocol_version:3,encrypted_sync_version:3,current_cursor:6,results:[{event_id:id,server_sequence:6,duplicate:false}]}),{s:fixture.scope,id:intent.event_id})
 // Reattach the ordinary isolated transport with the real owner-produced wire.
 page=await reopen(page,fixture)
 const accepted=await page.evaluate(()=>adapter.store.read());assert.equal(await abortLifecycle('receive'),true);assert.deepEqual(await page.evaluate(()=>adapter.store.read()),accepted)
 for(const kind of ['reader_gate','offline','device','unsupported']){
   assert.equal(await page.evaluate(async kind=>{
     const {encryptedSyncV3Api}=await import('/src/api/encryptedSyncV3.ts'),{ApiError}=await import('/src/api/client.ts')
     const pull=encryptedSyncV3Api.pullRaw
     encryptedSyncV3Api.pullRaw=async(...args)=>{if(kind==='reader_gate')throw new ApiError(409,'content_note_reader_required','isolated gate');if(kind==='offline')throw new TypeError('isolated disconnect');const page=JSON.parse(await pull(...args));if(kind==='device')page.items[0].event.device_id=crypto.randomUUID();else page.items[0].event.entity_type='document';return JSON.stringify(page)}
     try{await adapter.store.receiveContentAuthorEcho(readerAuth.auth,readerAuth.bindings,readerAuth.keys);return false}catch{return true}finally{encryptedSyncV3Api.pullRaw=pull}
   },kind),true)
   assert.deepEqual(await page.evaluate(()=>adapter.store.read()),accepted)
 }
 const received=await page.evaluate(async()=>adapter.store.receiveContentAuthorEcho(readerAuth.auth,readerAuth.bindings,readerAuth.keys))
 assert.equal(received.new_events,1)
 const staged=await page.evaluate(()=>adapter.store.read())
 assert.equal(staged.manifest.applied_cursor,5);assert.equal(staged.rows[5].state,'received')
 const failed=await page.evaluate(async recovery=>{
  const put=IDBObjectStore.prototype.put;let fired=false
  IDBObjectStore.prototype.put=function(value,...args){const r=put.call(this,value,...args);if(this.name==='inbox'&&value.state==='content_note_self_echo_proven'){fired=true;r.addEventListener('success',()=>this.transaction.abort(),{once:true})}return r}
  try{if(recovery)await adapter.store.recoverV10(readerAuth.auth,readerAuth.bindings,readerAuth.keys);else await adapter.run();return false}catch{return fired}finally{IDBObjectStore.prototype.put=put}
 },recovery)
 assert.equal(failed,true);assert.deepEqual(await page.evaluate(()=>adapter.store.read()),staged)
 if(recovery){
  const revoked=await page.evaluate(async()=>{const put=IDBObjectStore.prototype.put;let fired=false
   IDBObjectStore.prototype.put=function(value,...args){const r=put.call(this,value,...args);if(this.name==='inbox'&&value.state==='content_note_self_echo_proven'){fired=true;r.addEventListener('success',()=>void readerAuth.auth.logout(),{once:true})}return r}
   try{await adapter.store.recoverV10(readerAuth.auth,readerAuth.bindings,readerAuth.keys);return false}catch{return fired}finally{IDBObjectStore.prototype.put=put}})
  assert.equal(revoked,true);page=await reopen(page,fixture);assert.deepEqual(await page.evaluate(()=>adapter.store.read()),staged)
  const result=await page.evaluate(()=>adapter.store.recoverV10(readerAuth.auth,readerAuth.bindings,readerAuth.keys))
  assert.equal(result.applied[0]?.route,'content_self_echo',JSON.stringify(result))
 }else{assert.equal(await abortLifecycle('manifest'),true);assert.deepEqual(await page.evaluate(()=>adapter.store.read()),staged);await page.evaluate(()=>adapter.run())}
 const applied=await page.evaluate(()=>adapter.store.read())
 assert.equal(applied.rows[5].state,'content_note_self_echo_proven');assert.equal(applied.manifest.applied_cursor,6)
 // Prefix owner intentionally does not yet qualify author P2/inbox9 evidence.
 if(recovery){assert.equal(applied.checkpoint.proven_prefix,5);assert.equal(applied.checkpoint.ack_cursor,0);assert.equal(applied.rows[5].wire_item,JSON.stringify(fixture.items[5]));assert.equal(applied.checkpoint.next_local_ordinal,staged.checkpoint.next_local_ordinal)}
 assert.deepEqual(applied.notes,staged.notes);assert.deepEqual(applied.sources,staged.sources);assert.deepEqual(applied.noteHistory,staged.noteHistory)
 assert.deepEqual(applied.rows.slice(0,5),before.rows)
 assert.equal(applied.author.out.find(o=>o.event_id===intent.event_id).raw_body,out.raw_body)
 assert.deepEqual(applied.author.receipts,staged.author.receipts)
 if(recovery)await page.evaluate(()=>adapter.store.recoverV10(readerAuth.auth,readerAuth.bindings,readerAuth.keys));else await page.evaluate(()=>adapter.run());assert.deepEqual(await page.evaluate(()=>adapter.store.read()),applied)
 assert.equal((await page.evaluate(()=>adapter.store.receiveContentAuthorEcho(readerAuth.auth,readerAuth.bindings,readerAuth.keys))).new_events,0)
 page=await reopen(page,fixture);if(recovery)await page.evaluate(()=>adapter.store.recoverV10(readerAuth.auth,readerAuth.bindings,readerAuth.keys));else await page.evaluate(()=>adapter.run());assert.deepEqual(await page.evaluate(()=>adapter.store.read()),applied)
 for(const kind of ['receipt','receipt_sequence','authorship','author_device','dependency','parent','proof','ciphertext']){
   assert.equal(await page.evaluate(async ({kind,recovery})=>{
     const q=indexedDB.open('worta-web-replay', 8);await new Promise((resolve,reject)=>{q.onsuccess=resolve;q.onerror=()=>reject(q.error)});const db=q.result
     const snapshot=()=>new Promise((resolve,reject)=>{const tx=db.transaction([...db.objectStoreNames]),v={};let count=0;for(const name of db.objectStoreNames){const r=tx.objectStore(name).getAll();r.onsuccess=()=>{v[name]=r.result;if(++count===db.objectStoreNames.length)resolve(v)}};tx.onabort=()=>reject(tx.error)})
     const before=await snapshot(),name=kind.startsWith('receipt')?'receipts':['proof','ciphertext'].includes(kind)?'inbox':'legacyNoteHistory'
     const row=structuredClone(before[name].find(r=>kind.startsWith('receipt')?r.record_version===2:kind==='proof'||kind==='ciphertext'?r.server_sequence===6:r.record_version===3))
     if(kind==='receipt')row.receipt_digest='0'.repeat(64)
     if(kind==='receipt_sequence')row.server_sequence++
     if(kind==='author_device')row.source_device_id=crypto.randomUUID()
     if(kind==='authorship')row.authorship_envelope.ciphertext=(row.authorship_envelope.ciphertext[0]==='A'?'B':'A')+row.authorship_envelope.ciphertext.slice(1)
     if(kind==='dependency')row.intent.dependencies.metadata_event_id=crypto.randomUUID()
     if(kind==='parent')row.header.parent_event_id=crypto.randomUUID()
     if(kind==='proof')row.content_note_self_echo_proof.outcome_digest='0'.repeat(64)
     if(kind==='ciphertext'){const w=JSON.parse(row.wire_item);w.object.ciphertext=(w.object.ciphertext[0]==='A'?'B':'A')+w.object.ciphertext.slice(1);row.wire_item=JSON.stringify(w)}
     const write=rows=>new Promise((resolve,reject)=>{const tx=db.transaction(name,'readwrite');for(const r of rows)tx.objectStore(name).put(r);tx.oncomplete=resolve;tx.onabort=()=>reject(tx.error)})
     await write([row]);const damaged=await snapshot();let failed=false
     try{if(recovery)await adapter.store.recoverV10(readerAuth.auth,readerAuth.bindings,readerAuth.keys);else await adapter.run()}catch{failed=true}
     if(JSON.stringify(await snapshot())!==JSON.stringify(damaged))throw Error('negative_changed_inventory')
     await write(before[name]);db.close();return failed
   },{kind,recovery}),true)
   assert.deepEqual(await page.evaluate(()=>adapter.store.read()),applied)
 }
 await page.evaluate(()=>readerAuth.auth.logout())
 assert.equal(await page.evaluate(async()=>{try{await adapter.run();return false}catch{return true}}),true)
 page=await reopen(page,fixture);await page.evaluate(()=>adapter.run());assert.deepEqual(await page.evaluate(()=>adapter.store.read()),applied)
 for(let generation=0;generation<3;generation++){
   const old=await page.evaluate(()=>adapter.store.read())
   const info=await page.evaluate(async({s,project,note,deleted})=>{
     const w=adapter.store.contentAuthorOwner(readerAuth.auth,readerAuth.bindings,readerAuth.keys),token=(await w.local.readSnapshot(s,project)).token
     if(deleted)await w.local.tombstoneNote(s,token,note,'2026-11-03T00:00:00.000000Z')
     else await w.local.updateNote(s,token,note,{content:'BROWSER_NEXT_AUTHOR'},'2026-11-03T00:00:00.000000Z')
     const i=(await w.list(s,project))[0];await w.authority.seal(s,i.event_id)
     const state=await adapter.store.read(),out=state.author.out.find(o=>o.event_id===i.event_id),sequence=state.rows.length+1
     await w.authority.accept(s,i.event_id,{protocol_version:3,encrypted_sync_version:3,current_cursor:sequence,results:[{event_id:i.event_id,server_sequence:sequence,duplicate:false}]})
     return {wire:JSON.parse(out.raw_body).items[0],sequence}
   },{s:fixture.scope,project,note,deleted:generation===2})
   fixture.items.push({event:{...info.wire.event,device_id:fixture.scope.deviceId,server_sequence:info.sequence},object:info.wire.object});fixture.anchor.registry.current_cursor=info.sequence
   page=await reopen(page,fixture)
   await page.evaluate(()=>adapter.store.receiveContentAuthorEcho(readerAuth.auth,readerAuth.bindings,readerAuth.keys))
   const staged=await page.evaluate(()=>adapter.store.read());await page.evaluate(()=>adapter.run());const next=await page.evaluate(()=>adapter.store.read())
   assert.deepEqual(next.sources,staged.sources);assert.deepEqual(next.notes,staged.notes);assert.deepEqual(next.rows.slice(0,-1),old.rows)
   assert.equal(next.manifest.applied_cursor,info.sequence);assert.equal(next.rows.at(-1).state,'content_note_self_echo_proven')
   page=await reopen(page,fixture);await page.evaluate(()=>adapter.run());assert.deepEqual(await page.evaluate(()=>adapter.store.read()),next)
 }
 return page
}
