import assert from 'node:assert/strict'
export async function acceptStageApply({page,scope:originalScope,cases,reopenProfile,network}){
 const names=['identities','accountBindings','inbox','outbox','receipts','cursors','projectBindings','ownerHeads','localProjects','localNotes','legacyNoteHistory','legacyNoteIntents'],project='stage-apply-project',stamp='2026-10-14T00:00:00.000000Z',peer='44444444-4444-4444-8444-444444444444'
 const snap=()=>page.evaluate(async names=>Object.fromEntries(await Promise.all(names.map(async n=>[n,await nativeRows(n)]))),names)
 const counts=()=>Object.fromEntries(Object.entries(network).map(([k,v])=>[k,v.length])),start=counts()
 let retainedScope,retainedSnapshot,winner
 for(const scenario of ['safe','conflict']){
  await page.evaluate(async()=>{await meta.keys.dispose();await session.close()})
  const user=scenario==='safe'?'cccccccc-cccc-4ccc-8ccc-cccccccccccc':'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa'
  const scope=await page.evaluate(async user=>{globalThis.session=await c21.openBrowserPersistence(user);const id=await session.read(user);return{canonicalUserId:user,accountId:id.local_account_id,deviceId:id.device_id}},user)
  await page.evaluate(async({scope,project})=>{globalThis.meta=await c21.metadataFixture(session,scope);globalThis.sp=session.proven.bind(meta.auth,meta.bindings,meta.keys);globalThis.st=session.stages.bind(meta.auth,meta.bindings,meta.keys);globalThis.sn=session.notes.bind(meta.auth,meta.bindings,meta.keys);await session.localSource.bind(meta.auth,meta.bindings,meta.keys).initializeProject(scope,{project:{id:project,name:'PRIVATE_STAGE_PROJECT',goal:null,infinite:true,unit:'symbols',status:'active',created_at:null,updated_at:null,payload:{}},notes:[]});globalThis.sb=await session.projects.importRemote(scope,{project_id:project,bootstrap_id:crypto.randomUUID(),remote_state:'active',remote_high_water:0})},{scope,project})
  await page.evaluate(async({scope,project,stamp,peer})=>{
   const e={version:1,header:{account_id:scope.canonicalUserId,bootstrap_id:sb.bootstrap_id,device_id:peer,entity_id:project,event_id:crypto.randomUUID(),generation:1,operation:'create',parent_event_ids:[],project_id:project,revision:1,updated_at:stamp},metadata:{name:'PRIVATE_METADATA',goal:null,infinite:true,unit:'symbols',deadline:null,status:'active',personal_goal:1,auto_freeze:false,streak_enabled:false,work_method:'manual',stages_enabled:false,combine_stage_mindmaps:false},deleted_at:null}
   globalThis.sm=e.header.event_id
   const ref={event_id:sm,device_id:peer,server_sequence:1,project_id:project,entity_id:project,entity_type:'project_metadata',operation:'upsert',revision:1,updated_at:stamp,deleted_at:null},item=await meta.keys.leaseForAccount(scope.accountId).use(async amk=>{const frame=c21.frameProjectMetadata(e);try{const o=await c21.encryptObjectBytes(amk,{userId:scope.canonicalUserId,projectId:project,entityId:project,entityType:'project_metadata'},frame);return{event:ref,object:{crypto_version:1,aad_version:1,nonce:c21.encodeBase64Url(o.nonce),ciphertext:c21.encodeBase64Url(o.ciphertext)}}}finally{frame.fill(0)}})
   await session.sync.commitInboundPage(scope,0,JSON.stringify({protocol_version:3,encrypted_sync_version:3,items:[item],next_cursor:1,has_more:false}));await session.metadata.apply(scope,ref,meta.auth,meta.bindings,meta.keys)
  },{scope,project,stamp,peer})
  const stage=(seq,revision,parent,order=false,deletion=false)=>page.evaluate(async({scope,project,stamp,peer,seq,revision,parent,order,deletion})=>{
   const e={version:deletion||revision===1?1:2,header:{account_id:scope.canonicalUserId,project_id:project,bootstrap_id:sb.bootstrap_id,device_id:peer,entity_id:order?'stage_order':'same-stage',entity_type:order?'stage_order':'stage',event_id:crypto.randomUUID(),operation:deletion?'delete':revision===1?'create':'update',revision,generation:revision,parent_event_ids:parent?[parent]:[],updated_at:stamp,metadata_event_id:sm},stage:order||deletion?null:{name:('PRIVATE_STAGE_'+seq).repeat(30),goal:100,infinite:false,unit:'symbols',status:'active',deadline:null,personal_goal:1,auto_freeze:false,streak_enabled:false,work_method:'manual',created_at:null,completed_at:null},stage_ids:order?[]:null,stage_heads:order?{}:null,deleted_at:deletion?stamp:null}
   const item=await meta.keys.leaseForAccount(scope.accountId).use(async amk=>{const original=c21.frameStructuralEvent(e),frame=order||deletion?original:c21.compressCanonicalFrame(original);if(!order&&!deletion&&![0,1].includes(frame[11]))throw Error('expected_ID1');try{const o=await c21.encryptObjectBytes(amk,{userId:scope.canonicalUserId,projectId:project,entityId:e.header.entity_id,entityType:e.header.entity_type},frame);return{event:{event_id:e.header.event_id,device_id:peer,server_sequence:seq,project_id:project,entity_id:e.header.entity_id,entity_type:e.header.entity_type,operation:deletion?'delete':'upsert',revision,updated_at:stamp,deleted_at:deletion?stamp:null},object:{crypto_version:1,aad_version:1,nonce:c21.encodeBase64Url(o.nonce),ciphertext:c21.encodeBase64Url(o.ciphertext)}}}finally{frame.fill(0);original.fill(0)}})
   await session.sync.commitInboundPage(scope,seq-1,JSON.stringify({protocol_version:3,encrypted_sync_version:3,items:[item],next_cursor:seq,has_more:false}));return e
  },{scope,project,stamp,peer,seq,revision,parent,order,deletion})
  const note=seq=>page.evaluate(async({scope,project,stamp,peer,seq})=>{const event={event_id:crypto.randomUUID(),project_id:project,entity_id:'stage-note-'+seq,entity_type:'note',operation:'upsert',revision:1,updated_at:stamp,deleted_at:null},n={id:event.entity_id,project_id:project,stage_id:null,source_type:'project',source_map_id:null,source_node_id:null,content_format:'html',title:'PRIVATE_NOTE',content:'PRIVATE_BODY',checklist:[],color:'default',pinned:false,archived:false,sort_order:0,tags:[],created_at:stamp,updated_at:stamp,metadata:{}},item=await meta.keys.leaseForAccount(scope.accountId).use(async amk=>{const{plaintext}=c21.createNoteSyncPlaintext(event,null,n),bytes=c21.encodeNoteSyncPlaintext(plaintext);try{const o=await c21.encryptObjectBytes(amk,{userId:scope.canonicalUserId,projectId:project,entityId:event.entity_id,entityType:'note'},bytes);return{event:{...event,device_id:peer,server_sequence:seq},object:{crypto_version:1,aad_version:1,nonce:c21.encodeBase64Url(o.nonce),ciphertext:c21.encodeBase64Url(o.ciphertext)}}}finally{bytes.fill(0)}});await session.sync.commitInboundPage(scope,seq-1,JSON.stringify({protocol_version:3,encrypted_sync_version:3,items:[item],next_cursor:seq,has_more:false}));await sn.apply(scope,seq)},{scope,project,stamp,peer,seq})
  const beforeNetwork=counts(),a=await stage(2,1),before=await snap()
  const aborted=await page.evaluate(async scope=>{const p=await st.prepare(scope,2),put=IDBObjectStore.prototype.put;IDBObjectStore.prototype.put=function(v,...args){const r=put.call(this,v,...args);if(this.name==='inbox'&&v.state==='stage_proven')r.addEventListener('success',()=>this.transaction.abort(),{once:true});return r};try{await st.commit(p);return false}catch{return true}finally{IDBObjectStore.prototype.put=put}},scope)
  assert.equal(aborted,true);assert.deepEqual(await snap(),before)
  for(const kind of ['lock','logout']){assert.equal(await page.evaluate(async({scope,kind})=>{const p=await st.prepare(scope,2);if(kind==='lock')meta.keys.lock();else await meta.auth.logout();try{await st.commit(p);return false}catch{return true}},{scope,kind}),true);assert.deepEqual(await snap(),before);await page.evaluate(async scope=>{await meta.keys.dispose();globalThis.meta=await c21.metadataFixture(session,scope);globalThis.st=session.stages.bind(meta.auth,meta.bindings,meta.keys);globalThis.sp=session.proven.bind(meta.auth,meta.bindings,meta.keys);globalThis.sn=session.notes.bind(meta.auth,meta.bindings,meta.keys)},scope)}
  assert.equal(await page.evaluate(async s=>{try{await st.prepare(s,2);return false}catch{return true}},originalScope),true)
  assert.equal(await page.evaluate(scope=>st.apply(scope,2),scope),'applied')
  const b=await stage(3,scenario==='safe'?2:1,scenario==='safe'?a.header.event_id:null)
  if(scenario==='safe')assert.equal(await page.evaluate(scope=>st.apply(scope,3),scope),'applied')
  else{const old=await snap();assert.equal(await page.evaluate(async scope=>{try{await st.apply(scope,3);return false}catch{return true}},scope),true);assert.deepEqual(await snap(),old)}
  await note(4)
  assert.equal((await page.evaluate(scope=>sp.advance(scope),scope)).candidate_proven_prefix,scenario==='safe'?4:2)
  if(scenario==='safe'){
    assert.deepEqual(await page.evaluate(({scope,project})=>st.read(scope,project,'same-stage'),{scope,project}),b.stage)
    const old=await snap();assert.equal(await page.evaluate(scope=>st.apply(scope,2),scope),'already_applied');assert.deepEqual(await snap(),old)
    await stage(5,1,null,true);await note(6);assert.equal((await page.evaluate(scope=>sp.advance(scope),scope)).candidate_proven_prefix,4)
    const tombstone=await stage(7,3,b.header.event_id,false,true)
    assert.equal(tombstone.stage,null);assert.equal(tombstone.deleted_at,stamp)
    assert.equal(await page.evaluate(scope=>st.apply(scope,7),scope),'applied')
    const tombstoned=await snap();assert.equal(await page.evaluate(({scope,project})=>st.read(scope,project,'same-stage'),{scope,project}),null)
    const head=await page.evaluate(async name=>await nativeRows(name),'ownerHeads')
    const stageHead=head.filter(r=>r.account_id===scope.accountId&&r.owner_kind==='stage')
    assert.equal(stageHead.length,1);assert.deepEqual(stageHead[0].tip_event_ids,[tombstone.header.event_id])
    assert.deepEqual(stageHead[0].history.filter(p=>p.operation==='delete').map(p=>({event_id:p.event_id,operation:p.operation,revision:p.revision,parent_event_ids:p.parent_event_ids})),[{event_id:tombstone.header.event_id,operation:'delete',revision:3,parent_event_ids:[b.header.event_id]}])
    const replay=await snap();assert.equal(await page.evaluate(scope=>st.apply(scope,7),scope),'already_applied');assert.deepEqual(await snap(),replay);assert.deepEqual(await snap(),tombstoned)
  }
  assert.deepEqual(counts(),beforeNetwork)
  const target=scenario==='safe'?4:2
  await page.route('**/api/v1/sync/devices/*',r=>r.fulfill({status:200,contentType:'application/json',body:JSON.stringify({protocol_version:1,device_id:scope.deviceId,last_ack_cursor:0})}))
  await page.route('**/api/v2/sync/encrypted/capabilities',r=>r.fulfill({status:200,contentType:'application/json',body:JSON.stringify({supported_transport_version:2,writer_transport_version:3,cutover_epoch:2})}))
  await page.route('**/api/v3/sync/encrypted/compression-reader-capabilities',r=>r.fulfill({status:204}))
  await page.route('**/api/v1/cloud/projects/bootstrap',r=>r.fulfill({status:200,contentType:'application/json',body:JSON.stringify({projects:[{project_id:project,bootstrap_id:a.header.bootstrap_id,origin_device_id:peer,state:'active',initial_event_count:0,initial_max_server_sequence:0}],current_cursor:scenario==='safe'?7:4})}))
  await page.route('**/api/v3/sync/encrypted/ack',r=>{assert.equal(r.request().postDataJSON().cursor,target);return r.fulfill({status:204})})
  assert.deepEqual(await page.evaluate(async scope=>{const d=await session.device.bind(meta.auth,meta.bindings).registerVerified(scope),r=await session.readers.bind(meta.auth,meta.bindings).declare(scope,d),registry=(await session.registry.bind(meta.auth,meta.bindings,meta.keys).reconcile(scope)).token,p=await sp.prepareAck(scope,r,registry),result=await sp.ackOnce(p);if((await sp.ackOnce(p)).status!=='no_progress')throw Error('unexpected_ack_replay');return result},scope),{status:'advanced',cursor:target,ready:false})
  for(const pattern of ['**/api/v1/sync/devices/*','**/api/v2/sync/encrypted/capabilities','**/api/v3/sync/encrypted/compression-reader-capabilities','**/api/v1/cloud/projects/bootstrap','**/api/v3/sync/encrypted/ack'])await page.unroute(pattern)
  const snapshot=await snap();assert.equal(JSON.stringify(snapshot).includes('PRIVATE_STAGE_'),false)
  cases.push(scenario==='safe'?'Stage ID1 create2/update3 + Metadata1/Note4 proves4 and ACK4; readable order5 blocks later Note6; authenticated delete7 tombstones exactly once with null projection; no plaintext/order writer':'competing Stage3 stays received; later applied Note4 cannot jump; prefix2/ACK2 only')
  cases.push('Stage native-IDB atomic abort after proof write, key revocation, logout and account isolation all roll back; apply/proof/prefix produce zero HTTP')
  if(scenario==='safe'){retainedScope=scope;retainedSnapshot=snapshot;winner=null}
 }
 await page.evaluate(async()=>{await meta.keys.dispose();await session.close()})
 const open=()=>page.evaluate(async scope=>{globalThis.session=await c21.openBrowserPersistence(scope.canonicalUserId);globalThis.meta=await c21.metadataFixture(session,scope);globalThis.st=session.stages.bind(meta.auth,meta.bindings,meta.keys);globalThis.sp=session.proven.bind(meta.auth,meta.bindings,meta.keys)},retainedScope)
 await page.reload();await open();assert.deepEqual(await page.evaluate(({scope,project})=>st.read(scope,project,'same-stage'),{scope:retainedScope,project}),winner)
 page=await reopenProfile();await open();assert.deepEqual(await page.evaluate(({scope,project})=>st.read(scope,project,'same-stage'),{scope:retainedScope,project}),winner)
 const state=await page.evaluate(scope=>session.sync.readCheckpoint(scope),retainedScope);assert.equal(state.proven_prefix,4);assert.equal(state.ack_cursor,4);assert.equal(state.pull_cursor,7)
 const final=await snap();for(const name of names)assert.deepEqual(final[name].filter(r=>r.account_id===retainedScope.accountId||r.local_account_id===retainedScope.accountId),retainedSnapshot[name].filter(r=>r.account_id===retainedScope.accountId||r.local_account_id===retainedScope.accountId))
 cases.push('Stage tombstoned authority/history ciphertext and bounded prefix4/ACK4 survive renderer reload and disk profile reopen; deleted tip rehydrates as null projection after unlock')
 const end=counts();assert.equal(end.registration-start.registration,2);assert.equal(end.capability-start.capability,2);assert.equal(end.mode-start.mode,6);assert.equal(end.registry-start.registry,2);assert.equal(end.ack-start.ack,2)
 await page.evaluate(async scope=>{await meta.keys.dispose();await session.close();globalThis.session=await c21.openBrowserPersistence(scope.canonicalUserId);globalThis.meta=await c21.metadataFixture(session,scope)},originalScope)
 return page
}
