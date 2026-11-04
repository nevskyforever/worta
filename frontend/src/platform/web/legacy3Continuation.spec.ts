// @vitest-environment node
import {afterEach,it,expect,vi} from 'vitest'
import {continuationFixture} from './resolutionContinuation.testFixture'
import {scope} from './bootstrap.testFixture'
import {BrowserResolutionContinuation} from './resolutionContinuation'
import * as historical from './resolutionHistorical'
import {BrowserResolutionPrefix} from './resolutionPrefix'
import {BrowserV10Recovery} from './v10Recovery'
import {BrowserV10OrdinaryApply} from './v10OrdinaryApply'
import {bindPrivateMetadataPrefix} from './privateMetadataPrefix'
import {authenticateTerminalMetadataResolutions,authenticateMetadataHandoffResolutions} from './resolutionAuthentication'
import {amk} from './bootstrap.testFixture'
afterEach(()=>vi.restoreAllMocks())
const strategies=['choose_version','manual_merge','delete','keep_both'] as const
const setup=(strategy:typeof strategies[number]='keep_both')=>continuationFixture(strategy,true,undefined,false,2,true)
it('Content3 resurrection is authorized only by a Content Resolution root, never a Legacy3 root',async()=>{
 const legacy=await setup('delete'),content=await continuationFixture('delete',false,{format:'plain'})
 const child=await legacy.receive({protocol:3,unchecked:true}),before=legacy.before()
 await expect(legacy.runtime.apply(scope,child.sequence)).rejects.toMatchObject({code:'unsupported_local_source'})
 expect(legacy.before()).toEqual(before)
 const accepted=await content.receive({protocol:3})
 expect(await content.runtime.apply(scope,accepted.sequence)).toBe('applied')
 expect((await content.runtime.history(scope)).resolutions).toBe(1)
})
const inventory=(f:Awaited<ReturnType<typeof setup>>)=>{const d=f.f.db;return{history:d.legacyNoteHistory!,rows:d.inbox,bindings:d.projectBindings,owners:d.ownerHeads!,out:d.outbox,receipts:d.receipts,projects:d.localProjects!,localNotes:d.localNotes!,intents:d.legacyNoteIntents!,checkpoints:d.cursors,cursor:d.cursors[0] as any}}
for(const protocol of [2,3] as const)for(const strategy of strategies)it(`Legacy3 ${strategy} -> genuine ordinary${protocol}, retained role, later history and exact replay`,async()=>{
 const f=await setup(strategy),before=f.before(),root=f.received
 expect(root).toMatchObject({protocol_version:3,record_version:10,resolution_proof:{protocol_version:3,reference:{operation:'resolution'}}})
 expect(await f.runtime.history(scope)).toMatchObject({readOnly:true,resolutions:1,closedGroups:1})
 const original=await f.receive({protocol,deleted:strategy==='delete',compressed:true}),received=f.before(),revision=(received.localProjects![0] as any).source_revision
 expect(await f.runtime.apply(scope,original.sequence)).toBe('applied')
 expect((f.f.db.localProjects![0] as any).source_revision).toBe(revision+1)
 const childRow=f.f.db.inbox.find((r:any)=>r.event_id===original.event_id) as any
 expect(childRow.protocol_version).toBe(protocol);expect(childRow.state).toBe(protocol===2?'note_proven':'content_note_proven')
 let last=original
 if(strategy==='keep_both'){
  const retained=await f.receive({id:'retained-note',protocol,compressed:true});expect(await f.runtime.apply(scope,retained.sequence)).toBe('applied');last=retained
  expect(f.f.db.legacyNoteHistory!.find((h:any)=>h.record_kind==='head'&&h.note_id==='retained-note')).toMatchObject({linear_head_event_id:retained.event_id})
 }
 const later=await f.receive({id:last.plaintext.note.id,parent:last.event_id,revision:5,protocol,deleted:true,compressed:true})
 expect(await f.runtime.apply(scope,later.sequence)).toBe('applied')
 const after=f.before();expect(await f.runtime.reconcile(scope,original.sequence)).toBe('already_applied');expect(await f.runtime.reconcile(scope,last.sequence)).toBe('already_applied')
 expect(await f.runtime.history(scope)).toMatchObject({readOnly:true,resolutions:1})
 expect(f.before()).toEqual(after)
 for(const kind of ['resolution','group_transition'])expect(after.legacyNoteHistory!.filter((r:any)=>r.record_kind===kind)).toEqual(before.legacyNoteHistory!.filter((r:any)=>r.record_kind===kind))
 expect(f.received).toEqual(root)
 for(const store of ['outbox','receipts','projectBindings','ownerHeads','legacyNoteIntents'] as const)expect(after[store]).toEqual(before[store])
 for(const key of ['ack_cursor','proven_prefix','next_local_ordinal'])expect((after.cursors[0] as any)[key]).toBe((before.cursors[0] as any)[key])
})
for(const protocol of [2,3] as const)it(`ordinary${protocol} peer forks preserve source, block advancement and replay exactly`,async()=>{
 const f=await setup(),first=await f.receive({protocol});await f.runtime.apply(scope,first.sequence)
 const fork=await f.receive({protocol}),before=f.before()
 expect(await f.runtime.apply(scope,fork.sequence)).toBe('conflict_preserved')
 expect(f.f.db.localProjects).toEqual(before.localProjects);expect(f.f.db.localNotes).toEqual(before.localNotes)
 const second=await f.receive({protocol});expect(await f.runtime.apply(scope,second.sequence)).toBe('conflict_preserved')
 expect(await f.runtime.reconcile(scope,fork.sequence)).toBe('already_applied')
 const blocked=await f.receive({protocol,parent:first.event_id,revision:5}),stable=f.before()
 await expect(f.runtime.apply(scope,blocked.sequence)).rejects.toThrow();expect(f.before()).toEqual(stable)
})
it('no exported generic flag, invented token or another owner can request Legacy3 history',async()=>{
 const f=await setup(),other=new BrowserResolutionContinuation(f.f.transaction,f.f.localOwner,()=>true).bind(f.f.auth,f.f.bindings as never,f.f.keys as never,f.device),original=historical.authenticateHContinuationHistory
 await expect(original({} as never,f.runtime)).rejects.toThrow()
 await expect(historical.authenticateContinuationHistory(scope,inventory(f),amk,()=>true)).rejects.toThrow()
 let calls=0
 vi.spyOn(historical,'authenticateHContinuationHistory').mockImplementation(async(token,owner)=>{
  calls++;await expect(original(token,other)).rejects.toThrow()
  const result=await original(token,owner);await expect(original(token,owner)).rejects.toThrow();return result
 })
 const before=f.before();expect(await f.runtime.history(scope)).toMatchObject({readOnly:true,resolutions:1});expect(calls).toBe(1);expect(f.before()).toEqual(before)
})
it('successful H diagnostics do not authorize Q/K/O/private Metadata',async()=>{
 const f=await setup(),d=f.f,child=await f.receive({protocol:3});await f.runtime.apply(scope,child.sequence)
 const result=await f.runtime.history(scope),before=f.before(),args=[d.auth,d.bindings as never,d.keys as never,f.device] as const,g=new BrowserResolutionPrefix(d.transaction,()=>true),k=new BrowserV10Recovery(d.transaction,d.localOwner,()=>true)
 const gp=g.bind(...args);expect(await gp.calculate(await gp.prepare(scope))).toMatchObject({readOnly:true,ackAuthorized:false,counts:{resolution:1}})
 const q=g.bindMixedSources(...args);expect(await q.calculate(await q.prepare(scope))).toMatchObject({readOnly:true,ackAuthorized:false,counts:{resolution:1}})
 for(const action of [()=>k.bind(...args).run(scope),()=>k.bindPostFMetadata(...args).run(scope),()=>new BrowserV10OrdinaryApply(d.transaction,d.localOwner,()=>true).bind(...args).apply(scope,child.sequence),()=>bindPrivateMetadataPrefix(d.transaction,()=>true,...args).prepare(scope),()=>q.calculate(result as never),()=>g.bind(...args).calculate(result as never),()=>d.proven.prepare(scope)])await expect(action()).rejects.toThrow()
 await expect(authenticateTerminalMetadataResolutions(scope,inventory(f),amk,()=>true)).rejects.toThrow();await expect(authenticateMetadataHandoffResolutions(scope,inventory(f),amk,()=>true,{kind:'durable'})).rejects.toThrow()
 expect(f.before()).toEqual(before)
})
it('actual raw Legacy ordinary on3 stays received3 and explicitly unsupported',async()=>{
 const f=await setup(),child=await f.receive({protocol:3,rawLegacyOn3:true}),before=f.before()
 expect((before.inbox.at(-1) as any).protocol_version).toBe(3)
 expect(JSON.parse((before.inbox.at(-1) as any).wire_item).event.operation).toBe('upsert')
 await expect(f.runtime.apply(scope,child.sequence)).rejects.toMatchObject({code:'unsupported_local_source'});expect(f.before()).toEqual(before)
})

// Existing H corruption/CAS/lifecycle checks against real received3 Legacy roots.
for(const mutate of ['Resolution4','inbox10','evidence','role','source','project','metadata','bootstrap','unknown','duplicate','clone','parent','revision'] as const)it(`rejects ${mutate} corruption without writes`,async()=>{
 const f=await setup(),child=await f.receive({protocol:mutate==='metadata'?3:2}),d=f.f.db
 if(mutate==='Resolution4')d.legacyNoteHistory=d.legacyNoteHistory.filter((h:any)=>h.record_kind!=='resolution')
 if(mutate==='inbox10')d.inbox=d.inbox.filter((r:any)=>r.record_version!==10)
 if(mutate==='evidence'){const r=d.legacyNoteHistory.find((h:any)=>h.record_kind==='resolution');r.evidence_envelope.ciphertext=(r.evidence_envelope.ciphertext[0]==='A'?'B':'A')+r.evidence_envelope.ciphertext.slice(1)}
 if(mutate==='role')d.legacyNoteHistory.find((h:any)=>h.record_kind==='head'&&h.note_id==='content-note').node_ref.result_role='retained'
 if(mutate==='source')d.localNotes.find((n:any)=>n.note_id==='content-note').source_revision++
 if(mutate==='project')d.localProjects[0].source_revision++
 if(mutate==='metadata')d.ownerHeads=[]
 if(mutate==='bootstrap')d.projectBindings[0].bootstrap_id=crypto.randomUUID()
 if(mutate==='unknown')d.legacyNoteHistory.push({...d.legacyNoteHistory[0],record_version:99})
 if(mutate==='duplicate')d.inbox.push(structuredClone(d.inbox.at(-1)))
 if(mutate==='clone')d.localNotes.push({...d.localNotes.find((n:any)=>n.note_id==='retained-note'),project_id:'foreign'})
 if(mutate==='parent'||mutate==='revision'){const row=d.inbox.at(-1),wire=JSON.parse(row.wire_item);if(mutate==='parent')wire.event.entity_id='retained-note';else wire.event.revision++;row.wire_item=JSON.stringify(wire)}
 const before=f.before();await expect(f.runtime.apply(scope,child.sequence)).rejects.toThrow();expect(f.before()).toEqual(before)
})
for(const mutate of ['head','source','metadata','inbox','group','cursor','identity'] as const)it(`full transaction CAS detects ${mutate} drift`,async()=>{
 const f=await setup(),child=await f.receive(),token=await f.runtime.prepare(scope,child.sequence),d=f.f.db
 if(mutate==='head')d.legacyNoteHistory.find((h:any)=>h.record_kind==='head').head_revision++
 if(mutate==='source')d.localNotes[0].source_revision++
 if(mutate==='metadata')d.ownerHeads[0].head_revision++
 if(mutate==='inbox')d.inbox.at(-1).wire_item+=' '
 if(mutate==='group')d.legacyNoteHistory.find((h:any)=>h.record_kind==='group').conflict_generation++
 if(mutate==='cursor')d.cursors[0].pull_cursor++
 if(mutate==='identity')await f.f.auth.logout()
 const before=f.before();await expect(f.runtime.commit(token)).rejects.toThrow();expect(f.before()).toEqual(before)
})
for(const kind of ['key','logout','profile','account'] as const)it(`stale ${kind} cannot commit or read historical authority`,async()=>{
 const f=await setup(),child=await f.receive(),token=await f.runtime.prepare(scope,child.sequence)
 if(kind==='key')f.f.lock();else if(kind==='profile')f.close();else if(kind==='account')await f.f.auth.login('fixture','fixture');else await f.f.auth.logout()
 const before=f.before();await expect(f.runtime.commit(token)).rejects.toThrow();await expect(f.runtime.history(scope)).rejects.toThrow();expect(f.before()).toEqual(before)
})
for(const part of ['candidate','head','proof','projection'] as const)it(`corrupted applied successor ${part} rejects historical replay`,async()=>{
 const f=await setup(),child=await f.receive();await f.runtime.apply(scope,child.sequence);const d=f.f.db
 if(part==='candidate')d.legacyNoteHistory=d.legacyNoteHistory.filter((h:any)=>h.event_id!==child.event_id)
 if(part==='head')d.legacyNoteHistory=d.legacyNoteHistory.filter((h:any)=>h.record_kind!=='head'||h.note_id!=='content-note')
 if(part==='proof')d.inbox.at(-1).note_proof.outcome_digest='0'.repeat(64)
 if(part==='projection')d.localNotes.find((n:any)=>n.note_id==='content-note').envelope.ciphertext='AA'
 const before=f.before();await expect(f.runtime.reconcile(scope,child.sequence)).rejects.toThrow();expect(f.before()).toEqual(before)
})

it('late sibling of the closed pre-Resolution group remains received',async()=>{
 const f=await setup(),parent=f.parents[0]!.header.parent_event_id!,child=await f.receive({parent,revision:2}),before=f.before()
 await expect(f.runtime.apply(scope,child.sequence)).rejects.toThrow();expect(f.before()).toEqual(before)
})
it('ordinary production paths, local editing and prefix remain blocked',async()=>{
 const f=await setup(),child=await f.receive();await f.runtime.apply(scope,child.sequence);const before=f.before()
 await expect(f.f.content.apply(scope,child.sequence)).rejects.toThrow();await expect(f.f.proven.prepare(scope)).rejects.toThrow()
 const local=f.f.local,token=(await local.readSnapshot(scope,'p')).token
 await expect(local.updateNote(scope,token,'content-note',{content:'UNPUBLISHED'},'2026-10-20T00:00:00.000000Z')).rejects.toThrow();expect(f.before()).toEqual(before)
})


it('fake or another runtime continuation token never authorizes a write',async()=>{
 const f=await setup(),child=await f.receive(),token=await f.runtime.prepare(scope,child.sequence),other=new BrowserResolutionContinuation(f.f.transaction,f.f.localOwner,()=>true).bind(f.f.auth,f.f.bindings as never,f.f.keys as never,f.device),before=f.before()
 await expect(f.runtime.commit({} as never)).rejects.toThrow();await expect(other.commit(token)).rejects.toThrow();expect(f.before()).toEqual(before)
})

it('authenticated transport above local source bounds is retained without apply',async()=>{
 const f=await setup(),child=await f.receive({content:'x'.repeat(300001)}),before=f.before()
 await expect(f.runtime.apply(scope,child.sequence)).rejects.toThrow();expect(f.before()).toEqual(before)
})
for(const key of ['accountId','canonicalUserId','deviceId'] as const)it(`wrong ${key} cannot read or apply`,async()=>{
 const f=await setup(),child=await f.receive(),before=f.before(),other={...scope,[key]:crypto.randomUUID()}
 await expect(f.runtime.apply(other,child.sequence)).rejects.toThrow();await expect(f.runtime.history(other)).rejects.toThrow();expect(f.before()).toEqual(before)
})

it('a retained publication above pull_cursor is corruption',async()=>{
 const f=await setup(),child=await f.receive();await f.runtime.apply(scope,child.sequence)
 f.f.db.cursors[0].pull_cursor=child.sequence-1;const before=f.before()
 await expect(f.runtime.history(scope)).rejects.toThrow();expect(f.before()).toEqual(before)
})
it('wrong AMK cannot authenticate continuation evidence',async()=>{
 const f=await setup(),child=await f.receive(),lease=f.f.keys.leaseForAccount(),before=f.before()
 vi.spyOn(lease,'use').mockImplementation(async(action:any)=>{const wrong=new Uint8Array(32).fill(9);try{return await action(wrong)}finally{wrong.fill(0)}})
 await expect(f.runtime.apply(scope,child.sequence)).rejects.toThrow();expect(f.before()).toEqual(before)
})


for(const protocol of [2,3] as const)for(const mutation of ['ciphertext','nonce','aad','operation','device','project','revision'] as const)it(`Legacy3 H authenticates ordinary${protocol} ${mutation}`,async()=>{
 const f=await setup(),child=await f.receive({protocol}),row=f.f.db.inbox.at(-1) as any,w=JSON.parse(row.wire_item)
 if(mutation==='ciphertext'||mutation==='nonce')w.object[mutation]=(w.object[mutation][0]==='A'?'B':'A')+w.object[mutation].slice(1)
 if(mutation==='aad')w.object.aad_version=2
 if(mutation==='operation')w.event.operation='resolution'
 if(mutation==='device')w.event.device_id=scope.deviceId
 if(mutation==='project')w.event.project_id='foreign'
 if(mutation==='revision')w.event.revision++
 row.wire_item=JSON.stringify(w);const before=f.before()
 await expect(f.runtime.apply(scope,child.sequence)).rejects.toThrow();expect(f.before()).toEqual(before)
})
for(const mutation of ['proof','protocol','codec','descriptor','root_wire','root_nonce','parent','publication','retained','membership','pending','generation'] as const)it(`Legacy3 H rejects ${mutation} authority corruption`,async()=>{
 const f=await setup(),child=await f.receive(),d=f.f.db,root=d.inbox.find((r:any)=>r.event_id===f.resolution.header.event_id) as any,r=d.legacyNoteHistory!.find((h:any)=>h.record_kind==='resolution') as any
 if(mutation==='proof')root.resolution_proof.outcome_digest='0'.repeat(64)
 if(mutation==='protocol')root.protocol_version=2
 if(mutation==='codec')r.transport_kind='content_note_resolution_v2'
 if(['descriptor','root_wire','root_nonce'].includes(mutation)){const w=JSON.parse(root.wire_item);if(mutation==='descriptor')w.event.operation='event';else if(mutation==='root_wire')w.object.ciphertext='AA';else w.object.nonce='AA';root.wire_item=JSON.stringify(w)}
 if(mutation==='parent')d.legacyNoteHistory=d.legacyNoteHistory!.filter((h:any)=>h.event_id!==f.parents[0]!.event_id)
 if(mutation==='publication')d.inbox=d.inbox.filter((h:any)=>h.event_id!==f.parents[0]!.event_id)
 if(mutation==='retained')(d.localNotes!.find((n:any)=>n.note_id==='retained-note') as any).envelope.ciphertext='AA'
 if(mutation==='membership')d.localNotes=d.localNotes!.filter((n:any)=>n.note_id!=='retained-note')
 if(mutation==='pending')d.legacyNoteIntents!.push({account_id:scope.accountId,device_id:scope.deviceId,project_id:'p',note_id:'content-note'} as never)
 if(mutation==='generation')d.legacyNoteHistory!.push({...r,event_id:crypto.randomUUID()})
 const before=f.before();await expect(f.runtime.apply(scope,child.sequence)).rejects.toThrow();await expect(f.runtime.history(scope)).rejects.toThrow();expect(f.before()).toEqual(before)
})
for(const protocol of [2,3] as const)for(const route of ['stage','map','own','revision','parent','resurrection'] as const)it(`genuine ordinary${protocol} unsupported ${route} stays received`,async()=>{
 const f=await setup(route==='resurrection'?'delete':'keep_both'),child=await f.receive({protocol,unchecked:true,revision:route==='revision'?6:4,parent:route==='parent'?crypto.randomUUID():undefined,alter:(plain,wrapper,ref)=>{
  if(route==='stage')plain.note.stage_id='missing-stage'
  if(route==='map'){plain.note.source_type='mindmap';plain.note.source_map_id='m';plain.note.source_node_id='node'}
  if(route==='own'){wrapper.device_id=scope.deviceId;ref.device_id=scope.deviceId}
 }}),before=f.before()
 await expect(f.runtime.apply(scope,child.sequence)).rejects.toThrow();expect(f.before()).toEqual(before)
})
for(const dependency of ['bootstrap','metadata','stage'] as const)it(`Content3 ${dependency} dependency must be genuinely authorized`,async()=>{
 const f=await setup(),child=await f.receive({protocol:3,unchecked:true,alter:(_p,w)=>{
  if(dependency==='bootstrap')w.dependencies.bootstrap_id=crypto.randomUUID()
  if(dependency==='metadata')w.dependencies.metadata_event_id=crypto.randomUUID()
  if(dependency==='stage')w.dependencies.stage_event_ids=[crypto.randomUUID()]
 }}),before=f.before()
 await expect(f.runtime.apply(scope,child.sequence)).rejects.toThrow();expect(f.before()).toEqual(before)
})
it('one-use H apply capability and diagnostics cannot authorize another write',async()=>{
 const f=await setup(),child=await f.receive(),token=await f.runtime.prepare(scope,child.sequence)
 await f.runtime.commit(token);const before=f.before()
 await expect(f.runtime.commit(token)).rejects.toThrow();await expect(f.runtime.commit(await f.runtime.history(scope) as never)).rejects.toThrow();expect(f.before()).toEqual(before)
})
for(const count of [64])it(`Legacy3 H retains the complete ${count}-parent ancestry`,async()=>{
 const f=await continuationFixture('choose_version',true,undefined,false,count,true),child=await f.receive({protocol:3})
 expect(await f.runtime.apply(scope,child.sequence)).toBe('applied');expect((await f.runtime.history(scope)).resolutions).toBe(1)
},30000)
it('revoked verified-device authority cannot commit or read',async()=>{
 const f=await setup(),child=await f.receive(),token=await f.runtime.prepare(scope,child.sequence)
 vi.spyOn(f.f.auth,'isCurrent').mockReturnValue(false)
 const before=f.before();await expect(f.runtime.commit(token)).rejects.toThrow();await expect(f.runtime.history(scope)).rejects.toThrow();expect(f.before()).toEqual(before)
})
it('invalidation during the private handoff rejects and cleans its capability',async()=>{
 const f=await setup(),original=historical.authenticateHContinuationHistory;let captured:Parameters<typeof original>|undefined
 vi.spyOn(historical,'authenticateHContinuationHistory').mockImplementation(async(...args)=>{captured=args;f.close();return original(...args)})
 const before=f.before();await expect(f.runtime.history(scope)).rejects.toThrow();expect(f.before()).toEqual(before)
 await expect(original(...captured!)).rejects.toThrow()
})
it('an authentic additional received Resolution remains unsupported by H',async()=>{
 const f=await setup(),{sealResolutionFixture}=await import('./resolution.testFixture'),{encodeNoteSyncResolutionV2}=await import('@/cloud/noteSyncResolutionV2Codec')
 const payload=structuredClone(f.resolution);payload.header.event_id=crypto.randomUUID()
 // Valid codec/AEAD, a competing decision over the original genuine parent pair.
 // It must remain received; H does not become another-generation F authority.
 const bytes=encodeNoteSyncResolutionV2(payload);bytes.fill(0)
 const object=await sealResolutionFixture(payload),sequence=f.sequence+1,event={...JSON.parse(f.received.wire_item).event,event_id:payload.header.event_id,server_sequence:sequence}
 await f.f.sync.commitInboundPage(scope,f.sequence,JSON.stringify({protocol_version:3,encrypted_sync_version:3,items:[{event,object}],next_cursor:sequence,has_more:false}))
 const before=f.before();expect((before.inbox.at(-1) as any).protocol_version).toBe(3)
 await expect(f.runtime.apply(scope,sequence)).rejects.toThrow();expect(f.before()).toEqual(before)
})
