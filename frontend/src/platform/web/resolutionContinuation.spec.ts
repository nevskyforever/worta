// @vitest-environment node
import {afterEach,it,expect,vi} from 'vitest'
import {continuationFixture} from './resolutionContinuation.testFixture'
import {scope} from './bootstrap.testFixture'
import {BrowserResolutionContinuation} from './resolutionContinuation'
import type {BrowserSyncTransaction} from './syncDurability'
afterEach(()=>vi.restoreAllMocks())
for(const legacy of [true,false])for(const strategy of ['choose_version','manual_merge','delete','keep_both'] as const)it(`genuine protocol${legacy?2:3} ${strategy} original continuation and history`,async()=>{
 const f=await continuationFixture(strategy,legacy),before=f.before(),child=await f.receive({deleted:strategy==='delete'})
 expect(await f.runtime.apply(scope,child.sequence)).toBe('applied')
 expect(await f.runtime.history(scope)).toMatchObject({readOnly:true,resolutions:1,closedGroups:1})
 const after=f.before();expect(await f.runtime.apply(scope,child.sequence)).toBe('already_applied');expect(f.before()).toEqual(after)
 expect(after.legacyNoteHistory!.filter((r:any)=>['resolution','group_transition'].includes(r.record_kind))).toEqual(before.legacyNoteHistory!.filter((r:any)=>['resolution','group_transition'].includes(r.record_kind)))
 await expect(f.applier.reconcile(scope,f.sequence)).rejects.toThrow() // F exact-current replay is unchanged.
})
for(const legacy of [true,false])it(`protocol${legacy?2:3} retained child, later descendant, old replay and independent fork`,async()=>{
 const f=await continuationFixture('keep_both',legacy),root=f.resolution.header.event_id
 const original=await f.receive();await f.runtime.apply(scope,original.sequence)
 const retained=await f.receive({id:'retained-note',compressed:true});await f.runtime.apply(scope,retained.sequence)
 const grandchild=await f.receive({id:'retained-note',parent:retained.event_id,revision:5,deleted:true});await f.runtime.apply(scope,grandchild.sequence)
 expect(await f.runtime.apply(scope,retained.sequence)).toBe('already_applied')
 const sibling=await f.receive({parent:root});expect(await f.runtime.apply(scope,sibling.sequence)).toBe('conflict_preserved')
 const secondSibling=await f.receive({parent:root});expect(await f.runtime.apply(scope,secondSibling.sequence)).toBe('conflict_preserved')
 expect((await f.runtime.history(scope)).closedGroups).toBe(1)
 expect(f.f.db.legacyNoteHistory.filter((r:any)=>r.record_kind==='group')).toHaveLength(2)
 const before=f.before();await expect(f.f.proven.prepare(scope)).rejects.toThrow();expect(f.before()).toEqual(before)
})
for(const route of [{stage:true,format:'plain' as const,compressed:true},{stage:false,format:'plain' as const},{compressed:true}])it(`accepted Content route ${JSON.stringify(route)}`,async()=>{
 const f=await continuationFixture('keep_both',false,route),child=await f.receive({compressed:true})
 expect(await f.runtime.apply(scope,child.sequence)).toBe('applied');expect((await f.runtime.history(scope)).resolutions).toBe(1)
})
for(const legacy of [true,false])it(`protocol${legacy?2:3} upsert/delete and explicit resurrection barrier`,async()=>{
 const f=await continuationFixture('choose_version',legacy),upsert=await f.receive();await f.runtime.apply(scope,upsert.sequence)
 const deleted=await f.receive({parent:upsert.event_id,revision:5,deleted:true});await f.runtime.apply(scope,deleted.sequence)
 const again=await f.receive({parent:deleted.event_id,revision:6,deleted:true});await f.runtime.apply(scope,again.sequence)
 const resurrect=await f.receive({parent:again.event_id,revision:7}),before=f.before()
 await expect(f.runtime.apply(scope,resurrect.sequence)).rejects.toMatchObject({code:'unsupported_local_source'});expect(f.before()).toEqual(before)
 expect(await f.runtime.apply(scope,deleted.sequence)).toBe('already_applied')
})
for(const mutate of ['Resolution4','inbox10','evidence','role','source','project','metadata','bootstrap','unknown','duplicate','clone','parent','revision'] as const)it(`rejects ${mutate} corruption without writes`,async()=>{
 const f=await continuationFixture(),child=await f.receive(),d=f.f.db
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
 const f=await continuationFixture(),child=await f.receive(),token=await f.runtime.prepare(scope,child.sequence),d=f.f.db
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
 const f=await continuationFixture(),child=await f.receive(),token=await f.runtime.prepare(scope,child.sequence)
 if(kind==='key')f.f.lock();else if(kind==='profile')f.close();else if(kind==='account')await f.f.auth.login('fixture','fixture');else await f.f.auth.logout()
 const before=f.before();await expect(f.runtime.commit(token)).rejects.toThrow();await expect(f.runtime.history(scope)).rejects.toThrow();expect(f.before()).toEqual(before)
})
for(const part of ['candidate','head','proof','projection'] as const)it(`corrupted applied successor ${part} rejects historical replay`,async()=>{
 const f=await continuationFixture(),child=await f.receive();await f.runtime.apply(scope,child.sequence);const d=f.f.db
 if(part==='candidate')d.legacyNoteHistory=d.legacyNoteHistory.filter((h:any)=>h.event_id!==child.event_id)
 if(part==='head')d.legacyNoteHistory=d.legacyNoteHistory.filter((h:any)=>h.record_kind!=='head'||h.note_id!=='content-note')
 if(part==='proof')d.inbox.at(-1).content_note_proof.outcome_digest='0'.repeat(64)
 if(part==='projection')d.localNotes.find((n:any)=>n.note_id==='content-note').envelope.ciphertext='AA'
 const before=f.before();await expect(f.runtime.reconcile(scope,child.sequence)).rejects.toThrow();expect(f.before()).toEqual(before)
})
it('late sibling of the closed pre-Resolution group remains received',async()=>{
 const f=await continuationFixture(),parent=f.parents[0]!.header.parent_event_id!,child=await f.receive({parent,revision:2}),before=f.before()
 await expect(f.runtime.apply(scope,child.sequence)).rejects.toThrow();expect(f.before()).toEqual(before)
})
it('ordinary production paths, local editing and prefix remain blocked',async()=>{
 const f=await continuationFixture(),child=await f.receive();await f.runtime.apply(scope,child.sequence);const before=f.before()
 await expect(f.f.content.apply(scope,child.sequence)).rejects.toThrow();await expect(f.f.proven.prepare(scope)).rejects.toThrow()
 const local=f.f.local,token=(await local.readSnapshot(scope,'p')).token
 await expect(local.updateNote(scope,token,'content-note',{content:'UNPUBLISHED'},'2026-10-20T00:00:00.000000Z')).rejects.toThrow();expect(f.before()).toEqual(before)
})

for(const conflict of [false,true])for(let fault=0;fault<(conflict?5:6);fault++)it(`atomic ${conflict?'fork':'apply'} failure boundary ${fault}`,async()=>{
 const f=await continuationFixture()
 if(conflict){const first=await f.receive();await f.runtime.apply(scope,first.sequence)}
 const child=await f.receive();let armed=false,n=0
 const transaction:BrowserSyncTransaction=(scope,stores,mode,schedule)=>f.f.transaction(scope,stores,mode,(tx:IDBTransaction,result:(r:any)=>void,abort:(e:unknown)=>void)=>{
  const wrapped=new Proxy(tx,{get:(tx,key)=>key==='objectStore'?(name:string)=>{
   const store=tx.objectStore(name)
   return new Proxy(store,{get:(store,key)=>{
    const method=Reflect.get(store,key)
    if(['add','put'].includes(String(key))&&mode==='readwrite')return(...args:any[])=>{const r=method.apply(store,args);if(armed&&n++===fault)throw new DOMException('injected','QuotaExceededError');return r}
    return typeof method==='function'?method.bind(store):method
   }})
  }:Reflect.get(tx,key)})
  schedule(wrapped,result,abort)
 })
 const runtime=new BrowserResolutionContinuation(transaction,f.f.localOwner,()=>true).bind(f.f.auth,f.f.bindings as never,f.f.keys as never,f.device),token=await runtime.prepare(scope,child.sequence),before=f.before();armed=true
 if(fault===(conflict?4:5))f.f.abort()
 await expect(runtime.commit(token)).rejects.toThrow();expect(f.before()).toEqual(before)
})

it('published frozen Legacy parent remains authentic after peer advancement',async()=>{
 const f=await continuationFixture('choose_version',false,undefined,true),before=f.before(),child=await f.receive()
 expect(await f.runtime.apply(scope,child.sequence)).toBe('applied');expect((await f.runtime.history(scope)).closedGroups).toBe(1)
 expect(f.f.db.outbox).toEqual(before.outbox);expect(f.f.db.receipts).toEqual(before.receipts)
})
it('fake or another runtime continuation token never authorizes a write',async()=>{
 const f=await continuationFixture(),child=await f.receive(),token=await f.runtime.prepare(scope,child.sequence),other=new BrowserResolutionContinuation(f.f.transaction,f.f.localOwner,()=>true).bind(f.f.auth,f.f.bindings as never,f.f.keys as never,f.device),before=f.before()
 await expect(f.runtime.commit({} as never)).rejects.toThrow();await expect(other.commit(token)).rejects.toThrow();expect(f.before()).toEqual(before)
})

it('authenticated transport above local source bounds is retained without apply',async()=>{
 const f=await continuationFixture(),child=await f.receive({content:'x'.repeat(300001)}),before=f.before()
 await expect(f.runtime.apply(scope,child.sequence)).rejects.toThrow();expect(f.before()).toEqual(before)
})
for(const key of ['accountId','canonicalUserId','deviceId'] as const)it(`wrong ${key} cannot read or apply`,async()=>{
 const f=await continuationFixture(),child=await f.receive(),before=f.before(),other={...scope,[key]:crypto.randomUUID()}
 await expect(f.runtime.apply(other,child.sequence)).rejects.toThrow();await expect(f.runtime.history(other)).rejects.toThrow();expect(f.before()).toEqual(before)
})
it('stage dependency drift blocks preparation before source changes',async()=>{
 const f=await continuationFixture('keep_both',false,{stage:true,format:'plain'}),child=await f.receive()
 f.f.db.ownerHeads.find((h:any)=>h.owner_kind==='stage').tip_event_ids=[crypto.randomUUID()]
 const before=f.before();await expect(f.runtime.apply(scope,child.sequence)).rejects.toThrow();expect(f.before()).toEqual(before)
})

it('a retained publication above pull_cursor is corruption',async()=>{
 const f=await continuationFixture(),child=await f.receive();await f.runtime.apply(scope,child.sequence)
 f.f.db.cursors[0].pull_cursor=child.sequence-1;const before=f.before()
 await expect(f.runtime.history(scope)).rejects.toThrow();expect(f.before()).toEqual(before)
})
it('wrong AMK cannot authenticate continuation evidence',async()=>{
 const f=await continuationFixture(),child=await f.receive(),lease=f.f.keys.leaseForAccount(),before=f.before()
 vi.spyOn(lease,'use').mockImplementation(async(action:any)=>{const wrong=new Uint8Array(32).fill(9);try{return await action(wrong)}finally{wrong.fill(0)}})
 await expect(f.runtime.apply(scope,child.sequence)).rejects.toThrow();expect(f.before()).toEqual(before)
})

for(const count of [3,64])it(`continuation keeps the complete genuine ${count}-parent Resolution ancestry`,async()=>{
 const f=await continuationFixture('choose_version',count===64,undefined,false,count),child=await f.receive()
 expect(await f.runtime.apply(scope,child.sequence)).toBe('applied');expect((await f.runtime.history(scope)).resolutions).toBe(1)
},30000)
it('corrupted earlier ordinary proof cannot be hidden by the Resolution closure',async()=>{
 const f=await continuationFixture(),child=await f.receive(),row=f.f.db.inbox.find((r:any)=>r.state==='content_note_proven')
 row.content_note_proof.outcome_digest='0'.repeat(64);const before=f.before()
 await expect(f.runtime.apply(scope,child.sequence)).rejects.toThrow();expect(f.before()).toEqual(before)
})

it('recreates a Content note only from the authenticated deleted Resolution result',async()=>{
 const f=await continuationFixture('delete',false,{format:'plain'}),child=await f.receive(),before=f.before()
 const wire=(f.f.db.inbox.find((r:any)=>r.event_id===child.event_id) as any).wire_item
 expect(await f.runtime.apply(scope,child.sequence)).toBe('applied')
 expect((f.f.db.inbox.find((r:any)=>r.event_id===child.event_id) as any).wire_item).toBe(wire)
 const after=f.before();expect(after).not.toEqual(before)
 expect(await f.runtime.apply(scope,child.sequence)).toBe('already_applied');expect(f.before()).toEqual(after)
 expect((await f.runtime.history(scope)).resolutions).toBe(1)
})
