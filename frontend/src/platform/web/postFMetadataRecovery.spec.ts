// @vitest-environment node
import {afterEach,beforeEach,describe,expect,it,vi} from 'vitest'
import {ordinaryV10Fixture} from './v10OrdinaryApply.testFixture'
import {createMetadataRecovery,createMetadataPrefix,pendingMetadata,applyFamilyMetadata,type MetadataFixture} from './postFMetadata.testFixture'
import {BrowserImportedMetadataApply} from './importedMetadataApply'
import {BrowserResolutionPrefix} from './resolutionPrefix'
import {BrowserResolutionContinuationRuntime} from './resolutionContinuation'
import {BrowserV10OrdinaryApplyRuntime} from './v10OrdinaryApply'
import {scope} from './bootstrap.testFixture'
import type {BrowserSyncTransaction} from './syncDurability'

afterEach(()=>vi.restoreAllMocks())
const sourceUnchanged=(a:ReturnType<MetadataFixture['before']>,b:ReturnType<MetadataFixture['before']>)=>{
 for(const name of ['localProjects','localNotes','legacyNoteHistory','legacyNoteIntents','outbox','receipts','projectBindings'] as const)expect(b[name]).toEqual(a[name])
}
for(const protocol of [2,3] as const)for(const strategy of ['choose_version','manual_merge','delete','keep_both'] as const)for(const compressed of [false,true])describe(`private Metadata K F${protocol}/${strategy}/Content ID${compressed?1:0}`,()=>{
 let f:MetadataFixture,ordinary:Awaited<ReturnType<MetadataFixture['receive']>>,baseline:Awaited<ReturnType<ReturnType<typeof createMetadataPrefix>['calculate']>>
 beforeEach(async()=>{f=await ordinaryV10Fixture(strategy,protocol,protocol===3&&compressed)},5000)
 beforeEach(async()=>{
  ordinary=await f.receive('genuine-retained-content',null,1,false,{protocol:3,compressed});await f.owner.apply(scope,ordinary.sequence)
  const g=new BrowserResolutionPrefix(f.transaction,()=>true).bindMixedSources(f.f.auth,f.f.bindings as never,f.f.keys as never,f.device)
  baseline=await g.calculate(await g.prepare(scope));await g.commit(await g.prepareCommit(await g.prepare(scope)))
 },5000)
 for(const operation of ['update','delete'] as const)it(`routes genuine ${operation}, independently commits its prefix and replays without mutations`,async()=>{
  const m=await pendingMetadata(f,operation),before=f.before(),oldOwner=vi.spyOn(BrowserImportedMetadataApply.prototype,'commitVerifiedImportedMetadataApply'),http=vi.spyOn(globalThis,'fetch'),nonces=vi.spyOn(crypto,'getRandomValues').mockImplementation(()=>{throw Error('NO_ROUTER_NONCES')})
  const result=await createMetadataRecovery(f).run(scope)
  expect(result).toMatchObject({applied:[{sequence:m.reference.server_sequence,route:'metadata',status:'applied'}],blocker:null,more:false,ready:false,ackAuthorized:false,declarationAllowed:false,readiness:null,ownerCommitMayHaveOccurred:false,prefixCommitMayHaveOccurred:false,prefix:{candidate_proven_prefix:m.reference.server_sequence,counts:{...baseline.counts,metadata:baseline.counts.metadata+1}}})
  expect((f.f.db.cursors[0] as any).proven_prefix).toBe(m.reference.server_sequence)
  expect(f.m.reference.server_sequence).toBeLessThan(ordinary.sequence);expect(ordinary.sequence).toBeLessThan(m.reference.server_sequence)
  sourceUnchanged(before,f.before());expect(oldOwner).not.toHaveBeenCalled();expect(http).not.toHaveBeenCalled();expect(nonces).not.toHaveBeenCalled()
  for(const row of before.inbox)expect((f.f.db.inbox.find((r:any)=>r.event_id===(row as any).event_id) as any).wire_item).toBe((row as any).wire_item)
  const after=f.before();expect((await createMetadataRecovery(f).run(scope)).applied).toEqual([]);expect(f.before()).toEqual(after)
 },5000)
})

describe('private Metadata route ordering, durable interruption and live owners',()=>{
 let f:MetadataFixture
 beforeEach(async()=>{f=await ordinaryV10Fixture()},5000)
 beforeEach(async()=>{const e=await f.receive('retained-content',null,1,false,{protocol:3,compressed:true});await f.owner.apply(scope,e.sequence)},5000)
 it('commits a genuine update and then terminal delete while retaining the same Content history',async()=>{
  const k=createMetadataRecovery(f),u=await pendingMetadata(f);await k.run(scope)
  const d=await pendingMetadata(f,'delete'),before=f.before(),result=await k.run(scope)
  expect(result).toMatchObject({applied:[{sequence:d.reference.server_sequence,route:'metadata'}],blocker:null,prefix:{counts:{metadata:3,resolution:1},candidate_proven_prefix:d.reference.server_sequence}})
  expect(u.reference.server_sequence).toBeLessThan(d.reference.server_sequence);sourceUnchanged(before,f.before())
  const old=new BrowserResolutionPrefix(f.transaction,()=>true).bindMixedSources(f.f.auth,f.f.bindings as never,f.f.keys as never,f.device)
  await expect(old.prepare(scope)).rejects.toThrow()
 },5000)
 it('does not route later Metadata ahead of earlier received Content',async()=>{
  const e=await f.receive('unapplied-before-D'),d=await pendingMetadata(f,'delete'),before=f.before()
  expect(e.sequence).toBeLessThan(d.reference.server_sequence);await expect(createMetadataRecovery(f).run(scope)).rejects.toThrow();expect(f.before()).toEqual(before)
 })
 for(const order of ['before','after'] as const)it(`never promotes received Content ${order} deletion into historical first apply`,async()=>{
  const e=order==='before'?await f.receive('received-before-D'):undefined,d=await pendingMetadata(f,'delete')
  if(order==='before')await applyFamilyMetadata(f,d)
  else await createMetadataRecovery(f).run(scope)
  const pending=e??await f.receive('received-after-D'),before=f.before(),r=await createMetadataRecovery(f).run(scope)
  expect(r).toMatchObject({applied:[],blocker:{sequence:pending.sequence,reason:'deleted_metadata_live_apply'},prefix:{candidate_proven_prefix:order==='before'?pending.sequence-1:d.reference.server_sequence,first_blocking_sequence:pending.sequence},more:true})
  expect(f.f.db.inbox.find((r:any)=>r.event_id===pending.event_id)).toMatchObject({state:'received'});sourceUnchanged(before,f.before())
  await expect(f.owner.apply(scope,pending.sequence)).rejects.toThrow()
 },5000)
 it('does not use Legacy transport to bypass terminal Metadata for a new source mutation',async()=>{
  await pendingMetadata(f,'delete');await createMetadataRecovery(f).run(scope)
  const pending=await f.receive('legacy-after-D',null,1,false,{protocol:2}),before=f.before()
  expect(await createMetadataRecovery(f).run(scope)).toMatchObject({applied:[],blocker:{sequence:pending.sequence,reason:'deleted_metadata_live_apply'}})
  expect(f.before()).toEqual(before)
 },5000)
 it('keeps live H and O ownership after a genuine Metadata update',async()=>{
  await pendingMetadata(f);await createMetadataRecovery(f).run(scope)
  const a=await f.receive('new-live-content'),h=await f.receive('content-note',f.children[0]!.event_id,5)
  const ordinary=vi.spyOn(BrowserV10OrdinaryApplyRuntime.prototype,'commit'),continuation=vi.spyOn(BrowserResolutionContinuationRuntime.prototype,'commit')
  expect(await createMetadataRecovery(f).run(scope)).toMatchObject({applied:[{sequence:a.sequence,route:'content'},{sequence:h.sequence,route:'continuation'}],blocker:null})
  expect(ordinary).toHaveBeenCalledOnce();expect(continuation).toHaveBeenCalledOnce()
 },20000)
 it('uses the existing pass budget and reloads before reporting the later pending Note',async()=>{
  const m=await pendingMetadata(f),later=await f.receive('after-update')
  expect(await createMetadataRecovery(f).run(scope,{maxEvents:1})).toMatchObject({applied:[{sequence:m.reference.server_sequence,route:'metadata'}],blocker:{sequence:later.sequence,kind:'budget_exhausted'},prefix:{candidate_proven_prefix:m.reference.server_sequence},more:true})
  expect(await createMetadataRecovery(f).run(scope)).toMatchObject({applied:[{sequence:later.sequence,route:'content'}],blocker:null})
 },5000)
 it('rolls back an interrupted owner commit and grants no prefix crossing',async()=>{
  await pendingMetadata(f);f.f.abort();const before=f.before();await expect(createMetadataRecovery(f).run(scope)).rejects.toThrow();expect(f.before()).toEqual(before)
 })
 it('preserves the owner outcome when the separate cursor transaction aborts; a fresh runtime resumes',async()=>{
  const d=await pendingMetadata(f,'delete');let writes=0
  const tx:BrowserSyncTransaction=(...args)=>{if(args[2]==='readwrite'&&++writes===2)f.f.abort();return f.transaction(...args)}
  const before=f.before(),r=await createMetadataRecovery(f,tx).run(scope)
  expect(r).toMatchObject({applied:[{sequence:d.reference.server_sequence,route:'metadata'}],blocker:{reason:'historical_prefix_verification_failed'},prefixCommitMayHaveOccurred:false})
  expect(writes).toBe(2);expect((f.f.db.cursors[0] as any).proven_prefix).toBe((before.cursors[0] as any).proven_prefix)
  expect(f.f.db.inbox.find((r:any)=>r.event_id===d.reference.event_id)).toMatchObject({state:'applied'});sourceUnchanged(before,f.before())
  f.f.abort(false);const owner=f.before().ownerHeads
  expect(await createMetadataRecovery(f).run(scope)).toMatchObject({applied:[],blocker:null,prefix:{candidate_proven_prefix:d.reference.server_sequence}});expect(f.before().ownerHeads).toEqual(owner)
 },5000)
 it('reports a durable cursor when its later verification fails, then reauthenticates on restart',async()=>{
  const d=await pendingMetadata(f,'delete');let writes=0
  const tx:BrowserSyncTransaction=async(...args)=>{if(writes===2&&args[2]==='readonly')throw Error('AFTER_CURSOR_COMMIT');const r=await f.transaction(...args);if(args[2]==='readwrite')writes++;return r}
  const r=await createMetadataRecovery(f,tx).run(scope)
  expect(r).toMatchObject({applied:[{sequence:d.reference.server_sequence}],blocker:{reason:'historical_prefix_verification_failed'},prefixCommitMayHaveOccurred:true})
  expect((f.f.db.cursors[0] as any).proven_prefix).toBe(d.reference.server_sequence);const after=f.before()
  expect(await createMetadataRecovery(f).run(scope)).toMatchObject({applied:[],blocker:null});expect(f.before()).toEqual(after)
 },5000)
 it('reports owner commit uncertainty without falsely claiming rollback or a verified outcome',async()=>{
  const d=await pendingMetadata(f,'delete');let written=false
  const tx:BrowserSyncTransaction=async(...args)=>{if(written&&args[2]==='readonly')throw Error('AFTER_OWNER_COMMIT');const r=await f.transaction(...args);if(args[2]==='readwrite')written=true;return r}
  expect(await createMetadataRecovery(f,tx).run(scope)).toMatchObject({applied:[],ownerCommitMayHaveOccurred:true,blocker:{reason:'metadata_post_commit_verification_failed'}})
  expect(f.f.db.inbox.find((r:any)=>r.event_id===d.reference.event_id)).toMatchObject({state:'applied'});expect(await createMetadataRecovery(f).run(scope)).toMatchObject({applied:[],blocker:null})
 },5000)
 it('rejects two pending Metadata outcomes without narrowing the retained inventory',async()=>{
  const first=await pendingMetadata(f);await pendingMetadata(f);const before=f.before()
  await expect(createMetadataRecovery(f).run(scope)).rejects.toThrow();expect(f.before()).toEqual(before);expect(first.row.state).toBe('received')
 })
})
