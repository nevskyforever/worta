// @vitest-environment node
import {afterEach,it,expect,vi} from 'vitest'
import {contentRow} from './noteTransport.testFixture'
import {continuationFixture} from './resolutionContinuation.testFixture'
import {scope,amk} from './bootstrap.testFixture'
import type {NoteSyncResolutionV2} from '@/cloud/noteSyncResolutionV2Codec'
import {BrowserResolutionPlanner} from './resolutionPlanner'
import {BrowserLocalAuthority} from './localAuthority'
import type {BrowserSyncTransaction} from './syncDurability'
import {frameContentNote} from '@/cloud/contentNoteCodec'
import {encryptObjectBytes} from '@/crypto'
import {encodeBase64Url} from '@/api/base64url'
afterEach(()=>vi.restoreAllMocks())
async function fixture(change?:(r:NoteSyncResolutionV2)=>void){
 const f=await continuationFixture('choose_version',false,{format:'plain',compressed:true})
 const a=await f.receive({content:'SECOND_A'});await f.runtime.apply(scope,a.sequence)
 const b=await f.receive({content:'SECOND_B',alter:(_p,w,r)=>{w.device_id='77777777-7777-4777-8777-777777777777';r.device_id=w.device_id}});await f.runtime.apply(scope,b.sequence)
 const group=f.f.db.legacyNoteHistory.find((r:any)=>r.record_kind==='group'&&r.common_parent_event_id===f.resolution.header.event_id) as any
 const resolution=structuredClone(f.resolution),ids=group.tip_event_ids
 Object.assign(resolution.header,{event_id:crypto.randomUUID(),parent_event_id:ids[0],additional_parent_event_ids:ids.slice(1),revision:5})
 resolution.resolution={conflict_group_id:group.group_id,conflict_generation:group.conflict_generation,resolved_event_ids:ids,strategy:'choose_version',selected_event_id:a.event_id}
 resolution.result={operation:'upsert',note:a.plaintext.note as never}
 change?.(resolution)
 const sequence=b.sequence+1,wrapper={...f.content,event:resolution},bytes=frameContentNote(wrapper)
 const object=await encryptObjectBytes(amk,{userId:scope.canonicalUserId,projectId:'p',entityId:'content-note',entityType:'note'},bytes)
 try{await f.f.sync.commitInboundPage(scope,b.sequence,JSON.stringify({protocol_version:3,encrypted_sync_version:3,items:[{event:{event_id:resolution.header.event_id,device_id:wrapper.device_id,server_sequence:sequence,project_id:'p',entity_id:'content-note',entity_type:'note',operation:'event',revision:5,updated_at:resolution.header.updated_at,deleted_at:null},object:{crypto_version:1,aad_version:1,nonce:encodeBase64Url(object.nonce),ciphertext:encodeBase64Url(object.ciphertext)}}],next_cursor:sequence,has_more:false}))}finally{bytes.fill(0);object.nonce.fill(0);object.ciphertext.fill(0)}
 const planner=f.factory.bind(f.f.auth,f.f.bindings as never,f.f.keys as never,f.device)
 return{f,sequence,planner}
}
it('applies a second choose-version through the real planner/apply owner and authenticates both generations',async()=>{
 const {f,sequence,planner}=await fixture(),before=f.before(),wire=(f.f.db.inbox.at(-1) as any).wire_item
 const p=await planner.prepareSuccessor(scope,sequence),owner=planner.atomicApply()
 expect(await owner.commit(await owner.prepare(p.token))).toEqual({status:'applied'})
 expect((f.f.db.inbox.at(-1) as any).wire_item).toBe(wire)
 expect(f.f.db.legacyNoteHistory.filter((r:any)=>r.record_kind==='resolution')).toHaveLength(2)
 expect(await f.runtime.history(scope)).toMatchObject({resolutions:2,closedGroups:2})
 const saved=f.before();expect(await owner.reconcile(scope,sequence)).toEqual({status:'already_applied'});expect(f.before()).toEqual(saved)
 expect(f.f.db.legacyNoteHistory.filter((r:any)=>r.record_kind==='group')).toEqual(before.legacyNoteHistory.filter((r:any)=>r.record_kind==='group'))
})

it('rejects unknown parents before creating successor owner proofs',async()=>{
 const {f,sequence,planner}=await fixture(r=>{const ids=[r.resolution.resolved_event_ids[0]!,crypto.randomUUID()].sort();r.header.parent_event_id=ids[0]!;r.header.additional_parent_event_ids=ids.slice(1);r.resolution.resolved_event_ids=ids})
 const before=f.before();await expect(planner.prepareSuccessor(scope,sequence)).rejects.toThrow();expect(f.before()).toEqual(before)
})
it('rejects a sibling proof joined to the closed conflict group',async()=>{
 const {f,sequence,planner}=await fixture()
 const groups=f.f.db.legacyNoteHistory.filter((r:any)=>r.record_kind==='group') as any[]
 const closed=groups.find(g=>g.common_parent_event_id!==f.resolution.header.event_id)
 const row=f.f.db.inbox.find((r:any)=>r.server_sequence===sequence-1) as any
 row.content_note_conflict_proof.group_id=closed.group_id
 const before=f.before();await expect(planner.prepareSuccessor(scope,sequence)).rejects.toThrow();expect(f.before()).toEqual(before)
})
it('keeps unsupported successor strategies out of the new entry',async()=>{
 const {f,sequence,planner}=await fixture(r=>{r.resolution={conflict_group_id:r.resolution.conflict_group_id,conflict_generation:r.resolution.conflict_generation,resolved_event_ids:r.resolution.resolved_event_ids,strategy:'manual_merge'}})
 const before=f.before();await expect(planner.prepareSuccessor(scope,sequence)).rejects.toThrow();expect(f.before()).toEqual(before)
})
it('aborts the whole second outcome and succeeds with a fresh preparation',async()=>{
 const {f,sequence}=await fixture();let armed=true
 const tx:BrowserSyncTransaction=(s,stores,mode,schedule)=>f.f.transaction(s,stores,mode,(t:IDBTransaction,result:(v:unknown)=>void,abort:(e:unknown)=>void)=>schedule(t,v=>{if(mode==='readwrite'&&armed)abort(Error('second_resolution_abort'));else result(v)},abort))
 const planner=new BrowserResolutionPlanner(tx,new BrowserLocalAuthority(tx),()=>true).bind(f.f.auth,f.f.bindings as never,f.f.keys as never,f.device),owner=planner.atomicApply()
 const p=await planner.prepareSuccessor(scope,sequence),token=await owner.prepare(p.token),before=f.before()
 await expect(owner.commit(token)).rejects.toThrow('second_resolution_abort');expect(f.before()).toEqual(before)
 armed=false;const retry=await planner.prepareSuccessor(scope,sequence);await owner.commit(await owner.prepare(retry.token))
 expect(await f.runtime.history(scope)).toMatchObject({resolutions:2,closedGroups:2})
})
it('does not weaken the frozen initial Resolution planner',async()=>{
 const {f,sequence,planner}=await fixture(),before=f.before()
 await expect(planner.prepare(scope,sequence)).rejects.toThrow();expect(f.before()).toEqual(before)
})

it('retains a post-second-Resolution event without committing an unaccepted continuation',async()=>{
 const {f,sequence,planner}=await fixture(),owner=planner.atomicApply(),p=await planner.prepareSuccessor(scope,sequence)
 await owner.commit(await owner.prepare(p.token))
 const parent=(f.f.db.inbox.at(-1) as any).event_id
 await contentRow(f.f as never,sequence+1,{parent,revision:6,mutation:'update',change:e=>{if(e.event.version===1)e.event.note.content_format='plain'}})
 const before=f.before();await expect(f.runtime.apply(scope,sequence+1)).rejects.toThrow();expect(f.before()).toEqual(before)
})
it('revocation invalidates a prepared second Resolution before its transaction',async()=>{
 const {f,sequence,planner}=await fixture(),owner=planner.atomicApply(),p=await planner.prepareSuccessor(scope,sequence)
 const token=await owner.prepare(p.token),before=f.before();f.f.lock()
 await expect(owner.commit(token)).rejects.toThrow();expect(f.before()).toEqual(before)
})
