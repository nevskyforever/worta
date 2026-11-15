// @vitest-environment node
import {afterEach,beforeEach,describe,it,expect,vi} from 'vitest'
import {ordinaryV10Fixture} from './v10OrdinaryApply.testFixture'
import {recoveryFixture} from './v10Recovery.testFixture'
import {continuationFixture} from './resolutionContinuation.testFixture'
import {scope,amk} from './bootstrap.testFixture'
import {BrowserResolutionContinuation} from './resolutionContinuation'
import {BrowserMixedSourceWitness} from './mixedSourceWitness'
import {BrowserV10OrdinaryApply} from './v10OrdinaryApply'
import {BrowserLocalAuthorityRuntime} from './localAuthority'
import {canonical} from '@/cloud/projectMetadataCodec'
import {encryptObjectBytes} from '@/crypto'
import {encodeBase64Url} from '@/api/base64url'
import * as historical from './resolutionHistorical'
import * as mixed from './mixedSourceValidation'
import type {BrowserSyncTransaction} from './syncDurability'
afterEach(()=>vi.restoreAllMocks())
const projectRevision=(f:Awaited<ReturnType<typeof ordinaryV10Fixture>>)=>(f.f.db.localProjects!.find((p:any)=>p.project_id==='p') as any).source_revision as number
for(const protocol of [2,3] as const)for(const compressed of protocol===2?[false]:[false,true])for(const strategy of ['choose_version','manual_merge','delete','keep_both'] as const)describe(`mixed H protocol${protocol}/ID${compressed?1:0}/${strategy}`,()=>{
 let f:Awaited<ReturnType<typeof ordinaryV10Fixture>>,ordinary:Awaited<ReturnType<Awaited<ReturnType<typeof ordinaryV10Fixture>>['receive']>>
 beforeEach(async()=>{f=await ordinaryV10Fixture(strategy,protocol,compressed);ordinary=await f.receive('independent');expect(await f.owner.apply(scope,ordinary.sequence)).toBe('applied')},5000)
 it('interleaves real O/H changes, immutable replay and strict G/K barriers',async()=>{
  const before=f.before(),revision=projectRevision(f),initial=await f.witness.inspect(scope)
  const child=await f.receive('content-note',f.children[0]!.event_id,5,strategy==='delete')
  expect(await f.h.apply(scope,child.sequence)).toBe('applied')
  const update=await f.receive('independent',ordinary.event_id,2);expect(await f.owner.apply(scope,update.sequence)).toBe('applied')
  const next=await f.receive('content-note',child.event_id,6,strategy==='delete');expect(await f.h.apply(scope,next.sequence)).toBe('applied')
  expect(projectRevision(f)).toBe(revision+3)
  expect(await f.witness.inspect(scope)).toMatchObject({readOnly:true,commitAuthorized:false,ackAuthorized:false,ready:false,hTransitions:initial.hTransitions+2,ordinaryTransitions:initial.ordinaryTransitions+1})
  expect(await f.h.history(scope)).toMatchObject({readOnly:true,resolutions:1,closedGroups:1})
  const stable=f.before(),calls=f.f.calls.length
  expect(await f.h.reconcile(scope,child.sequence)).toBe('already_applied')
  expect(await f.j.replayInboundPage(scope,next.since,next.rawPage)).toMatchObject({new_events:0,replayed_events:1})
  expect(f.before()).toEqual(stable);expect(f.f.calls.slice(calls).every(c=>c.mode==='readonly')).toBe(true)
  for(const row of before.inbox)expect(f.f.db.inbox.find((r:any)=>r.event_id===(row as any).event_id)).toEqual(row)
  for(const row of before.legacyNoteHistory!.filter((r:any)=>r.record_kind!=='head'))expect(f.f.db.legacyNoteHistory!.find((r:any)=>r.record_kind===(row as any).record_kind&&r.record_id===(row as any).record_id&&r.note_id===(row as any).note_id)).toEqual(row)
  expect(f.f.db.localNotes!.find((r:any)=>r.note_id==='baseline-deleted')).toEqual(before.localNotes!.find((r:any)=>r.note_id==='baseline-deleted'))
  await expect(f.g.prepare(scope)).rejects.toThrow();await expect(f.runtime.run(scope)).rejects.toThrow();expect(f.before()).toEqual(stable)
 })
 if(strategy==='keep_both')it('keeps retained Head2 ownership and tombstones through later original/O changes',async()=>{
  const initial=projectRevision(f),retained=await f.receive('retained-note',f.children[1]!.event_id,5,true)
  expect(await f.h.apply(scope,retained.sequence)).toBe('applied')
  const original=await f.receive('content-note',f.children[0]!.event_id,5);expect(await f.h.apply(scope,original.sequence)).toBe('applied')
  const update=await f.receive('independent',ordinary.event_id,2);expect(await f.owner.apply(scope,update.sequence)).toBe('applied')
  const repeated=await f.receive('retained-note',retained.event_id,6,true);expect(await f.h.apply(scope,repeated.sequence)).toBe('applied')
  expect(projectRevision(f)).toBe(initial+4)
  expect(f.f.db.localNotes!.find((r:any)=>r.note_id==='retained-note')).toMatchObject({deleted:true,note_revision:0})
  expect(f.f.db.legacyNoteHistory!.find((r:any)=>r.record_kind==='head'&&r.note_id==='retained-note')).toMatchObject({record_version:2,node_ref:{kind:'candidate',event_id:repeated.event_id}})
  expect(await f.witness.inspect(scope)).toMatchObject({ordinaryTransitions:2})
  const before=f.before();expect(await f.h.reconcile(scope,retained.sequence)).toBe('already_applied');expect(f.before()).toEqual(before)
 })
})
for(const protocol of [2,3] as const)for(const compressed of protocol===2?[false]:[false,true])describe(`mixed fork protocol${protocol}/ID${compressed?1:0}`,()=>{
 let f:Awaited<ReturnType<typeof ordinaryV10Fixture>>
 beforeEach(async()=>{f=await ordinaryV10Fixture('keep_both',protocol,compressed);const ordinary=await f.receive();await f.owner.apply(scope,ordinary.sequence)},5000)
 it('preserves real H siblings with no source projection or revision, never N/O/G authority',async()=>{
  const parent=f.children[0]!.event_id,first=await f.receive('content-note',parent,5);await f.h.apply(scope,first.sequence)
  for(let n=0;n<2;n++){
   const sibling=await f.receive('content-note',parent,5),before=f.before(),revision=projectRevision(f)
   expect(await f.h.apply(scope,sibling.sequence)).toBe('conflict_preserved')
   expect(projectRevision(f)).toBe(revision);expect(f.f.db.localProjects).toEqual(before.localProjects);expect(f.f.db.localNotes).toEqual(before.localNotes)
   expect(f.f.db.legacyNoteHistory!.find((r:any)=>r.record_kind==='candidate'&&r.event_id===sibling.event_id)).toMatchObject({projection:null,projection_digest:null})
   expect(await f.h.reconcile(scope,sibling.sequence)).toBe('already_applied')
  }
  const stable=f.before();expect(await f.h.history(scope)).toMatchObject({closedGroups:1})
  await expect(f.witness.inspect(scope)).rejects.toMatchObject({code:'unresolved_fork'})
  await expect(f.owner.reconcile(scope,(f.f.db.inbox.find((r:any)=>JSON.parse(r.wire_item).event.entity_id==='independent') as any).server_sequence)).rejects.toMatchObject({code:'unresolved_fork'})
  await expect(f.g.prepare(scope)).rejects.toThrow();await expect(f.runtime.run(scope)).rejects.toThrow();expect(f.before()).toEqual(stable)
  const retained=await f.receive('retained-note',f.children[1]!.event_id,5),pending=f.before()
  await expect(f.h.apply(scope,retained.sequence)).rejects.toMatchObject({code:'note_conflict_unproven'});expect(f.before()).toEqual(pending)
 },20_000)
})
describe('mixed H source-owner barriers',()=>{
 let f:Awaited<ReturnType<typeof ordinaryV10Fixture>>,preO:ReturnType<Awaited<ReturnType<typeof ordinaryV10Fixture>>['before']>,ordinary:Awaited<ReturnType<Awaited<ReturnType<typeof ordinaryV10Fixture>>['receive']>>
 beforeEach(async()=>{f=await ordinaryV10Fixture();preO=f.before();ordinary=await f.receive();await f.owner.apply(scope,ordinary.sequence)},5000)
 for(const kind of ['counter','membership','baseline-proof','O-proof','H-proof','gap','bootstrap','unrelated-source'] as const)it(`rejects ${kind} without legacy fallback or writes`,async()=>{
  const child=await f.receive('content-note',f.children[0]!.event_id,5),d=f.f.db
  if(kind==='counter')(d.localProjects![0] as any).source_revision++
  if(kind==='membership')d.localNotes=d.localNotes!.filter((n:any)=>n.note_id!=='baseline-deleted')
  if(kind==='baseline-proof')(d.inbox.find((r:any)=>JSON.parse(r.wire_item).event.entity_id==='baseline-live') as any).content_note_proof.outcome_digest='0'.repeat(64)
  if(kind==='O-proof')(d.inbox.find((r:any)=>r.event_id===ordinary.event_id) as any).content_note_proof.outcome_digest='0'.repeat(64)
  if(kind==='H-proof')(d.inbox.find((r:any)=>r.event_id===f.children[0]!.event_id) as any).content_note_proof.outcome_digest='0'.repeat(64)
  if(kind==='gap')d.inbox=d.inbox.filter((r:any)=>r.server_sequence!==f.children[1]!.sequence)
  if(kind==='bootstrap')(d.projectBindings[0] as any).bootstrap_id=crypto.randomUUID()
  if(kind==='unrelated-source')(d.localProjects!.find((p:any)=>p.project_id==='q') as any).envelope.ciphertext='AA'
  const legacy=vi.spyOn(historical,'verifyContinuationSources'),before=f.before()
  await expect(f.h.prepare(scope,child.sequence)).rejects.toThrow();expect(legacy).not.toHaveBeenCalled();expect(f.before()).toEqual(before)
 })
 for(const kind of ['counter','membership'] as const)it(`rejects valid-AEAD ${kind} source drift without fallback`,async()=>{
  const child=await f.receive('content-note',f.children[0]!.event_id,5),row=f.f.db.localProjects!.find((p:any)=>p.project_id==='p') as any
  const bytes=await historical.openHistoricalJson(scope,'p','p','project_metadata',row.envelope,amk,()=>true)
  let payload:any;try{payload=JSON.parse(new TextDecoder().decode(bytes))}finally{bytes.fill(0)}
  if(kind==='counter'){payload.source_revision++;row.source_revision++}else payload.member_ids.push('unqualified-extra-member')
  const raw=new TextEncoder().encode(canonical(payload)),o=await encryptObjectBytes(amk,{userId:scope.canonicalUserId,projectId:'p',entityId:'p',entityType:'project_metadata'},raw)
  try{row.envelope={crypto_version:1,aad_version:1,nonce:encodeBase64Url(o.nonce),ciphertext:encodeBase64Url(o.ciphertext)}}finally{raw.fill(0);o.nonce.fill(0);o.ciphertext.fill(0)}
  const legacy=vi.spyOn(historical,'verifyContinuationSources'),before=f.before()
  await expect(f.h.prepare(scope,child.sequence)).rejects.toMatchObject({code:'source_mismatch'});expect(legacy).not.toHaveBeenCalled();expect(f.before()).toEqual(before)
 })
 for(const kind of ['project','projection'] as const)it(`rejects corrupted predicted ${kind} before committing`,async()=>{
  const child=await f.receive('content-note',f.children[0]!.event_id,5),before=f.before(),calls=f.f.calls.length
  const actual=BrowserLocalAuthorityRuntime.prototype.prepareProtectedContinuationApply
  vi.spyOn(BrowserLocalAuthorityRuntime.prototype,'prepareProtectedContinuationApply').mockImplementation(async function(this:BrowserLocalAuthorityRuntime,...args){
   const result=await actual.apply(this,args)
   if(kind==='project')result.project.envelope.ciphertext='AA';else result.note.envelope.ciphertext='AA'
   return result
  })
  await expect(f.h.prepare(scope,child.sequence)).rejects.toThrow();expect(f.before()).toEqual(before)
  expect(f.f.calls.slice(calls).every(c=>c.mode==='readonly')).toBe(true)
 })
 it('does not resurrect a genuine H tombstone in mixed history',async()=>{
  const deleted=await f.receive('content-note',f.children[0]!.event_id,5,true);await f.h.apply(scope,deleted.sequence)
  const resurrection=await f.receive('content-note',deleted.event_id,6),before=f.before()
  await expect(f.h.apply(scope,resurrection.sequence)).rejects.toMatchObject({code:'unsupported_local_source'});expect(f.before()).toEqual(before)
 })
 it('erased mixed evidence plus real old-source rollback cannot disguise a post-F gap as legacy',async()=>{
  const d=f.f.db
  d.inbox=d.inbox.filter((r:any)=>r.event_id!==ordinary.event_id)
  d.legacyNoteHistory=d.legacyNoteHistory!.filter((r:any)=>r.note_id!=='independent')
  d.localProjects=structuredClone(preO.localProjects!);d.localNotes=structuredClone(preO.localNotes!) // NEGATIVE only: actual old ciphertext, no fabricated positive proof.
  const legacy=vi.spyOn(historical,'verifyContinuationSources'),before=f.before()
  await expect(f.h.history(scope)).rejects.toMatchObject({code:'corrupt_record'});expect(legacy).not.toHaveBeenCalled();expect(f.before()).toEqual(before)
 })
 it('pins full pre/post inventory against competing genuine H owners and stale plans',async()=>{
  const child=await f.receive('content-note',f.children[0]!.event_id,5),other=new BrowserResolutionContinuation(f.transaction,f.f.localOwner,()=>true).bind(f.f.auth,f.f.bindings as never,f.f.keys as never,f.device)
  const first=await f.h.prepare(scope,child.sequence),second=await other.prepare(scope,child.sequence)
  expect(await f.h.commit(first)).toBe('applied');const before=f.before()
  await expect(other.commit(second)).rejects.toMatchObject({code:'stale_source'});expect(f.before()).toEqual(before)
  expect(await other.reconcile(scope,child.sequence)).toBe('already_applied')
 })
 it('does not skip an earlier received ordinary event',async()=>{
  const prior=await f.receive('other-note'),child=await f.receive('content-note',f.children[0]!.event_id,5),before=f.before()
  await expect(f.h.prepare(scope,child.sequence)).rejects.toMatchObject({code:'note_orphan'});expect(f.before()).toEqual(before)
  await f.owner.apply(scope,prior.sequence);expect(await f.h.apply(scope,child.sequence)).toBe('applied')
 })
 for(const kind of ['key','logout','profile','relogin'] as const)it(`revokes mixed prepared authority on ${kind}`,async()=>{
  const child=await f.receive('content-note',f.children[0]!.event_id,5),token=await f.h.prepare(scope,child.sequence),before=f.before()
  if(kind==='key')f.f.lock();if(kind==='logout')await f.f.auth.logout();if(kind==='profile')f.close();if(kind==='relogin')await f.f.auth.login('fixture','fixture')
  await expect(f.h.commit(token)).rejects.toThrow();expect(f.before()).toEqual(before)
 })
 for(const conflict of [false,true])it(`rolls back complete mixed ${conflict?'fork':'apply'} outcome and allows fresh retry`,async()=>{
  if(conflict){const first=await f.receive('content-note',f.children[0]!.event_id,5);await f.h.apply(scope,first.sequence)}
  const child=await f.receive('content-note',f.children[0]!.event_id,5),token=await f.h.prepare(scope,child.sequence),before=f.before()
  f.f.abort();await expect(f.h.commit(token)).rejects.toMatchObject({code:'storage_failed'});expect(f.before()).toEqual(before)
  f.f.abort(false);expect(await f.h.apply(scope,child.sequence)).toBe(conflict?'conflict_preserved':'applied')
 })
 it('validates the complete predicted state, never a raw N diagnostic/source DTO',async()=>{
  const child=await f.receive('content-note',f.children[0]!.event_id,5),check=vi.spyOn(mixed,'validateMixedContinuationInventory'),legacy=vi.spyOn(historical,'verifyContinuationSources')
  const before=f.before(),token=await f.h.prepare(scope,child.sequence)
  expect(check).toHaveBeenCalledTimes(2);expect(legacy).not.toHaveBeenCalled();expect(f.before()).toEqual(before)
  await expect(f.h.commit(await f.witness.inspect(scope) as never)).rejects.toThrow();expect(await f.h.commit(token)).toBe('applied')
 })
 it('aborts staged writes when key authority is revoked during the mixed transaction',async()=>{
  const child=await f.receive('content-note',f.children[0]!.event_id,5);let armed=false
  const tx:BrowserSyncTransaction=(s,stores,mode,schedule)=>f.transaction(s,stores,mode,(tx,result,abort)=>{
   const proxy=new Proxy(tx,{get:(target,key)=>key==='objectStore'?(name:string)=>{
    const store=target.objectStore(name);return new Proxy(store,{get:(target,key)=>{
     const value=Reflect.get(target,key)
     if(armed&&name==='inbox'&&key==='put')return(...args:unknown[])=>{const request=value.apply(target,args);f.f.lock();return request}
     return typeof value==='function'?value.bind(target):value
    }})
   }:Reflect.get(target,key)})
   schedule(proxy,result,abort)
  })
  const owner=new BrowserResolutionContinuation(tx,f.f.localOwner,()=>true).bind(f.f.auth,f.f.bindings as never,f.f.keys as never,f.device),token=await owner.prepare(scope,child.sequence),before=f.before();armed=true
  await expect(owner.commit(token)).rejects.toThrow();expect(f.before()).toEqual(before)
 })
 it('sends no HTTP, ACK, capability or publication and does not advance cursors',async()=>{
  const child=await f.receive('content-note',f.children[0]!.event_id,5),before=f.before(),http=vi.spyOn(globalThis,'fetch')
  await f.h.apply(scope,child.sequence);expect(http).not.toHaveBeenCalled()
  for(const name of ['cursors','outbox','receipts','projectBindings','ownerHeads','legacyNoteIntents'])expect(f.f.db[name]).toEqual(before[name])
  expect(amk.some(n=>n!==0)).toBe(true)
 })
})
for(const protocol of [2,3] as const)it(`preserves accepted out-of-order F/H-only protocol${protocol} without mixed fallback`,async()=>{
 const f=await recoveryFixture('keep_both',protocol,false,false,true,true);await f.runtime.run(scope,{maxEvents:f.r.sequence})
 const h=new BrowserResolutionContinuation(f.transaction,f.f.localOwner,()=>true).bind(f.f.auth,f.f.bindings as never,f.f.keys as never,f.device)
 await h.apply(scope,f.children[1]!.sequence);await h.apply(scope,f.children[0]!.sequence)
 const check=vi.spyOn(mixed,'validateMixedContinuationInventory'),before=f.before()
 expect(await h.history(scope)).toMatchObject({readOnly:true,resolutions:1});expect(check).not.toHaveBeenCalled();expect(f.before()).toEqual(before)
 const n=new BrowserMixedSourceWitness(f.transaction,()=>true).bind(f.f.auth,f.f.bindings as never,f.f.keys as never,f.device)
 await expect(n.inspect(scope)).rejects.toMatchObject({code:'revision_order'})
 const o=new BrowserV10OrdinaryApply(f.transaction,f.f.localOwner,()=>true).bind(f.f.auth,f.f.bindings as never,f.f.keys as never,f.device)
 await expect(o.prepare(scope,f.children[0]!.sequence)).rejects.toMatchObject({code:'revision_order'});expect(f.before()).toEqual(before)
})
it('retains legacy H open-conflict behavior including independent retained-role apply',async()=>{
 const f=await continuationFixture('keep_both'),first=await f.receive();await f.runtime.apply(scope,first.sequence)
 const sibling=await f.receive();expect(await f.runtime.apply(scope,sibling.sequence)).toBe('conflict_preserved')
 const retained=await f.receive({id:'retained-note'});expect(await f.runtime.apply(scope,retained.sequence)).toBe('applied')
 expect(await f.runtime.history(scope)).toMatchObject({readOnly:true,closedGroups:1})
})

it('reuses authenticated Stage/StageOrder dependencies for mixed H and Content plain O',async()=>{
 const f=await ordinaryV10Fixture('keep_both',3,true,true),ordinary=await f.receive('independent-stage',null,1,false,{format:'plain'})
 await f.owner.apply(scope,ordinary.sequence)
 const child=await f.receive('content-note',f.children[0]!.event_id,5),before=f.before()
 expect(await f.h.apply(scope,child.sequence)).toBe('applied');expect(await f.witness.inspect(scope)).toMatchObject({ordinaryTransitions:1})
 expect(f.f.db.ownerHeads).toEqual(before.ownerHeads)
},20_000)
