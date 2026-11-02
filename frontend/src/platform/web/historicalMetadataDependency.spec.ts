// @vitest-environment node
import {afterEach,beforeEach,describe,expect,it,vi} from 'vitest'
import {historicalMetadataAfterOrdinary,historicalMetadataFixture} from './historicalMetadataDependency.testFixture'
import {scope} from './bootstrap.testFixture'
import {metadataRow} from './metadataPrefix.testFixture'
import {ordinaryV10Fixture} from './v10OrdinaryApply.testFixture'
import {BrowserHistoricalMetadataDependency} from './historicalMetadataDependency'
import {BrowserV10Recovery} from './v10Recovery'
import {BrowserResolutionPrefix} from './resolutionPrefix'
import * as readers from './canonicalReaders'
import type {BrowserSyncTransaction} from './syncDurability'
afterEach(()=>vi.restoreAllMocks())
type Fixture=Awaited<ReturnType<typeof historicalMetadataFixture>>
for(const protocol of [2,3] as const)for(const strategy of ['choose_version','manual_merge','delete','keep_both'] as const)for(const compressed of protocol===2?[false]:[false,true])describe(`historical Metadata F${protocol}/ID${compressed?1:0}/${strategy}`,()=>{
 let f:Fixture
 beforeEach(async()=>{f=await historicalMetadataFixture(strategy,protocol,compressed)},20000)
 it('authenticates genuine O/H and F dependency components without changing storage',async()=>{
  const before=f.before()
  for(const id of [f.ordinary.event_id,f.original.event_id,...(f.retained?[f.retained.event_id]:[])])expect(await f.reader.inspect(scope,id)).toMatchObject({historicalMetadataAdmissible:true,reason:null,readOnly:true,fullHistoryAuthenticated:false,sourceAuthorized:false,firstApplyAllowed:false,commitAuthorized:false,ackAuthorized:false,ready:false})
  expect(await f.reader.inspect(scope,f.r.event_id)).toMatchObject({historicalMetadataAdmissible:protocol===3,reason:protocol===3?null:'not_content'})
  expect(await f.reader.inspect(scope,f.ordinary.event_id)).toMatchObject({historicalMetadataAdmissible:true})
  expect(f.before()).toEqual(before);expect(Object.keys(f.reader)).toEqual(['inspect'])
 })
})
describe('historical component barriers and zero mutation',()=>{
 let f:Fixture
 beforeEach(async()=>{f=await historicalMetadataFixture()},20000)
 for(const part of ['gap','duplicate-sequence','duplicate-event','missing-M','missing-proof','metadata-digest','metadata-parent','metadata-bootstrap','metadata-wire','duplicate-proof','note-wire','note-proof','projection','F-evidence','F-proof','unknown-generation','foreign-account','foreign-binding','local-intent'] as const)it(`rejects ${part} without modifying storage`,async()=>{
  const d=f.f.db,m=d.ownerHeads!.find((h:any)=>h.owner_kind==='project_metadata') as any,c=d.legacyNoteHistory!.find((c:any)=>c.event_id===f.ordinary.event_id) as any,row=d.inbox.find((r:any)=>r.event_id===f.ordinary.event_id) as any
  if(part==='gap')d.inbox.splice(0,1)
  else if(part==='duplicate-sequence')d.inbox.push(structuredClone(d.inbox[0]))
  else if(part==='duplicate-event'){const r=structuredClone(d.inbox[0]) as any;r.server_sequence=(d.cursors[0] as any).pull_cursor+1;d.inbox.push(r)}
  else if(part==='missing-M')d.ownerHeads=[]
  else if(part==='missing-proof')delete (d.inbox[0] as any).applied_proof
  else if(part==='metadata-digest')m.history[0].wire_digest='0'.repeat(64)
  else if(part==='metadata-parent')m.history.at(-1).parent_event_ids=[crypto.randomUUID()]
  else if(part==='metadata-bootstrap')m.bootstrap_id=crypto.randomUUID()
  else if(part==='metadata-wire'){const r=d.inbox[0] as any,w=JSON.parse(r.wire_item);w.object.ciphertext=w.object.ciphertext.slice(0,-2)+'AA';r.wire_item=JSON.stringify(w)}
  else if(part==='duplicate-proof')m.history.push(structuredClone(m.history[0]))
  else if(part==='note-wire'){const w=JSON.parse(row.wire_item);w.object.ciphertext=w.object.ciphertext.slice(0,-2)+'AA';row.wire_item=JSON.stringify(w)}
  else if(part==='note-proof')row.content_note_proof.dependency_digest='0'.repeat(64)
  else if(part==='projection')c.projection=null
  else if(part==='F-evidence')(d.legacyNoteHistory!.find((r:any)=>r.record_kind==='resolution') as any).evidence_envelope.ciphertext='AAAA'
  else if(part==='F-proof')(d.inbox.find((r:any)=>r.event_id===f.r.event_id) as any).resolution_proof.dependency_digest='0'.repeat(64)
  else if(part==='unknown-generation')(d.legacyNoteHistory!.find((r:any)=>r.record_kind==='resolution') as any).record_version=999
  else if(part==='foreign-account')row.account_id=crypto.randomUUID()
  else if(part==='foreign-binding')(d.projectBindings[0] as any).bootstrap_id=crypto.randomUUID()
  else d.legacyNoteIntents!.push({account_id:scope.accountId})
  const before=f.before(),r=await f.reader.inspect(scope,f.ordinary.event_id)
  expect(r).toMatchObject({historicalMetadataAdmissible:false,commitAuthorized:false,ready:false});expect(r.reason).toMatch(/^[a-z_]+$/);expect(f.before()).toEqual(before)
 })
 it('does not call writers, allocate nonces, use HTTP, or expose a certificate',async()=>{
  const tx:BrowserSyncTransaction=(s,n,mode,schedule)=>{expect(mode).toBe('readonly');return f.transaction(s,n,mode,schedule)}
  const before=f.before(),random=vi.spyOn(crypto,'getRandomValues').mockImplementation(()=>{throw Error('NO_NONCE')}),http=vi.spyOn(globalThis,'fetch').mockRejectedValue(Error('NO_HTTP'))
  const r=await f.create(tx).inspect(scope,f.ordinary.event_id)
  expect(r.historicalMetadataAdmissible).toBe(true);expect(random).not.toHaveBeenCalled();expect(http).not.toHaveBeenCalled();expect(Object.isFrozen(r)).toBe(true);expect(f.before()).toEqual(before)
 })
 it('a successful diagnostic cannot authorize O/H/Q commits',async()=>{
  const result=await f.reader.inspect(scope,f.ordinary.event_id),g=new BrowserResolutionPrefix(f.transaction,()=>true).bindMixedSources(f.f.auth,f.f.bindings as never,f.f.keys as never,f.device),before=f.before()
  await expect(f.owner.commit(result as never)).rejects.toThrow();await expect(f.h.commit(result as never)).rejects.toThrow();await expect(g.commit(result as never)).rejects.toThrow();expect(f.before()).toEqual(before)
 })
 it.each(['account','device'])('cannot rebind a genuine reader to a foreign %s',async field=>{
  const foreign={...scope,...(field==='account'?{accountId:crypto.randomUUID()}:{deviceId:crypto.randomUUID()})},before=f.before()
  await expect(f.reader.inspect(foreign,f.ordinary.event_id)).rejects.toThrow();expect(f.before()).toEqual(before)
 })
 it('decoder failures expose only a static sanitized reason',async()=>{
  vi.spyOn(readers,'openBrowserCanonicalObject').mockRejectedValue(Error('PRIVATE_UNTRUSTED_CODEC_DETAIL'))
  const before=f.before(),result=await f.reader.inspect(scope,f.ordinary.event_id)
  expect(result).toMatchObject({historicalMetadataAdmissible:false,reason:'invalid_evidence'});expect(JSON.stringify(result)).not.toContain('PRIVATE_');expect(f.before()).toEqual(before)
 })
 it.each(['lock','logout','profile','epoch'])('%s revokes the historical reader',async kind=>{
  const before=f.before();if(kind==='lock')f.f.lock();else if(kind==='logout')await f.f.auth.logout();else if(kind==='epoch')await f.f.auth.login('fixture','fixture');else f.revokeReader()
  await expect(f.reader.inspect(scope,f.ordinary.event_id)).rejects.toThrow();expect(f.before()).toEqual(before)
 })
 it('key revocation during Metadata AEAD cannot produce a stale successful diagnostic',async()=>{
  const original=readers.openBrowserCanonicalObject;let fired=false
  vi.spyOn(readers,'openBrowserCanonicalObject').mockImplementation(async(...args)=>{const r=await original(...args);if(!fired){fired=true;f.f.lock()}return r})
  const before=f.before();await expect(f.reader.inspect(scope,f.ordinary.event_id)).rejects.toMatchObject({code:'closed'});expect(f.before()).toEqual(before)
 })
 it('real Metadata owner change during the final reload invalidates the old snapshot',async()=>{
  let n=0
  const tx:BrowserSyncTransaction=async(s,stores,mode,schedule)=>{
   if(++n===2){const a=await metadataRow(f.f,f.deletion.reference.server_sequence+1,{revision:4,parent:f.deletion.event.header.event_id,change:e=>{e.header.operation='delete';e.metadata=null;e.deleted_at=e.header.updated_at}});await f.f.metadata.apply(scope,a.reference,f.f.auth,f.f.bindings as never,f.f.keys as never)}
   return f.transaction(s,stores,mode,schedule)
  }
  await expect(f.create(tx).inspect(scope,f.ordinary.event_id)).rejects.toMatchObject({code:'stale_source'})
 })
 for(const operation of ['delete','update'] as const)it(`a genuine later ${operation} is outside terminal deletion scope`,async()=>{
  const a=await metadataRow(f.f,f.deletion.reference.server_sequence+1,{revision:4,parent:f.deletion.event.header.event_id,change:e=>{if(operation==='delete'){e.header.operation='delete';e.metadata=null;e.deleted_at=e.header.updated_at}}})
  await f.f.metadata.apply(scope,a.reference,f.f.auth,f.f.bindings as never,f.f.keys as never)
  const before=f.before();expect((await f.reader.inspect(scope,f.ordinary.event_id)).historicalMetadataAdmissible).toBe(false);expect(f.before()).toEqual(before)
 })
 it('a contrary genuine received Metadata event cannot be hidden behind the successful head',async()=>{
  await metadataRow(f.f,f.deletion.reference.server_sequence+1,{revision:4,parent:f.update.event.header.event_id})
  const before=f.before();expect((await f.reader.inspect(scope,f.ordinary.event_id)).historicalMetadataAdmissible).toBe(false);expect(f.before()).toEqual(before)
 })
 it('genuine pending Content created after delete cannot gain a historical applied result',async()=>{
  const pending=await f.receive('later');const before=f.before();expect(await f.reader.inspect(scope,pending.event_id)).toMatchObject({historicalMetadataAdmissible:false,reason:'not_applied',firstApplyAllowed:false});expect(f.before()).toEqual(before)
 })
 it('old Q and K still reject the genuine .11 counterexample after a successful component check',async()=>{
  expect((await f.reader.inspect(scope,f.ordinary.event_id)).historicalMetadataAdmissible).toBe(true)
  const g=new BrowserResolutionPrefix(f.transaction,()=>true).bindMixedSources(f.f.auth,f.f.bindings as never,f.f.keys as never,f.device),k=new BrowserV10Recovery(f.transaction,f.f.localOwner,()=>true).bindMixedSources(f.f.auth,f.f.bindings as never,f.f.keys as never,f.device),before=f.before()
  await expect(g.prepare(scope)).rejects.toThrow();await expect(k.run(scope)).rejects.toThrow();expect(f.before()).toEqual(before)
 })
})
it('genuine Content received before deletion stays received afterward',async()=>{
 const f=await historicalMetadataFixture('keep_both',3,false,{pending:true}),before=f.before()
 expect(await f.reader.inspect(scope,f.pending!.event_id)).toMatchObject({historicalMetadataAdmissible:false,reason:'not_applied'});expect(f.before()).toEqual(before)
},20000)
it('genuine unresolved H fork remains a barrier',async()=>{
 const f=await historicalMetadataFixture('keep_both',3,false,{fork:true}),before=f.before()
 expect(await f.reader.inspect(scope,f.ordinary.event_id)).toMatchObject({historicalMetadataAdmissible:false,reason:'unresolved_fork'});expect(f.before()).toEqual(before)
},20000)
describe('genuine Stage-scoped history',()=>{
 let baseline:Awaited<ReturnType<typeof ordinaryV10Fixture>>,f:Fixture
 // Each real-owner phase has its own bounded hook budget. Nothing runs outside a hook/test.
 beforeEach(async()=>{baseline=await ordinaryV10Fixture('keep_both',3,false,true)},20000)
 beforeEach(async()=>{f=await historicalMetadataAfterOrdinary(baseline)},20000)
 it('genuine Stage-scoped F/H/O history remains unsupported',async()=>{
  const before=f.before()
  expect(await f.reader.inspect(scope,f.ordinary.event_id)).toMatchObject({historicalMetadataAdmissible:false,reason:'unsupported_stage'});expect(f.before()).toEqual(before)
 },20000)
})
it('genuine late Metadata reference accepted by the live owner does not establish historical ordering',async()=>{
 const f=await ordinaryV10Fixture(),old=f.m.event.header.event_id,later=crypto.randomUUID();f.m.event.header.event_id=later
 const e=await f.receive();f.m.event.header.event_id=old
 const m=await metadataRow(f.f,e.sequence+1,{revision:2,parent:old,change:v=>{v.header.event_id=later}})
 await f.f.metadata.apply(scope,m.reference,f.f.auth,f.f.bindings as never,f.f.keys as never);await f.owner.apply(scope,e.sequence)
 const d=await metadataRow(f.f,e.sequence+2,{revision:3,parent:later,change:v=>{v.header.operation='delete';v.metadata=null;v.deleted_at=v.header.updated_at}})
 await f.f.metadata.apply(scope,d.reference,f.f.auth,f.f.bindings as never,f.f.keys as never)
 const reader=new BrowserHistoricalMetadataDependency(f.transaction,()=>true).bind(f.f.auth,f.f.bindings as never,f.f.keys as never,f.device),before=f.before()
 expect(await reader.inspect(scope,e.event_id)).toMatchObject({historicalMetadataAdmissible:false,reason:'outside_interval'});expect(f.before()).toEqual(before)
})
