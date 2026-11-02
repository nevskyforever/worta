// @vitest-environment node
import {afterEach,beforeEach,describe,it,expect,vi} from 'vitest'
import {BrowserLocalAuthorityRuntime} from './localAuthority'
import {ordinaryV10Fixture} from './v10OrdinaryApply.testFixture'
import {scope,amk} from './bootstrap.testFixture'
import type {BrowserSyncTransaction} from './syncDurability'
afterEach(()=>vi.restoreAllMocks())
for(const protocol of [2,3] as const)for(const compressed of protocol===2?[false]:[false,true])for(const strategy of ['choose_version','manual_merge','delete','keep_both'] as const)describe(`genuine create/update/delete and baseline update protocol${protocol}/ID${compressed?1:0}/${strategy}`,()=>{
 let f:Awaited<ReturnType<typeof ordinaryV10Fixture>>
 // Build the genuine accepted F/H prerequisite separately from O's five applies.
 // Both phases retain a bounded 20 s limit; no caching, synthetic outcomes or retries.
 beforeEach(async()=>{f=await ordinaryV10Fixture(strategy,protocol,compressed)},20000)
 it('applies five source transitions, preserves evidence and replays without writes',async()=>{
 const before=f.before(),initial=await f.witness.inspect(scope),source=(f.f.db.localProjects![0] as any).source_revision
 const created=await f.receive();expect(await f.owner.apply(scope,created.sequence)).toBe('applied')
 const updated=await f.receive('independent',created.event_id,2);expect(await f.owner.apply(scope,updated.sequence)).toBe('applied')
 const baseline=f.f.db.legacyNoteHistory!.find((r:any)=>r.record_kind==='candidate'&&r.note_id==='baseline-live'&&r.header.revision===2) as any
 const existing=await f.receive('baseline-live',baseline.event_id,3);expect(await f.owner.apply(scope,existing.sequence)).toBe('applied')
 const deleted=await f.receive('independent',updated.event_id,3,true);expect(await f.owner.apply(scope,deleted.sequence)).toBe('applied')
 const repeated=await f.receive('independent',deleted.event_id,4,true);expect(await f.owner.apply(scope,repeated.sequence)).toBe('applied')
 expect((await f.witness.inspect(scope)).ordinaryTransitions).toBe(initial.ordinaryTransitions+5)
 expect((f.f.db.localProjects![0] as any).source_revision).toBe(source+5)
 expect(f.f.db.localNotes!.find((r:any)=>r.note_id==='independent')).toMatchObject({deleted:true,note_revision:0})
 expect(f.f.db.localNotes!.find((r:any)=>r.note_id==='baseline-deleted')).toEqual(before.localNotes!.find((r:any)=>r.note_id==='baseline-deleted'))
 expect(f.f.db.legacyNoteHistory!.filter((r:any)=>r.record_kind==='resolution')).toEqual(before.legacyNoteHistory!.filter((r:any)=>r.record_kind==='resolution'))
 const stable=f.before(),calls=f.f.calls.length
 expect(await f.create().apply(scope,created.sequence)).toBe('already_applied') // Superseded proof preserved.
 expect(await f.j.replayInboundPage(scope,repeated.since,repeated.rawPage)).toMatchObject({new_events:0,replayed_events:1})
 expect(f.before()).toEqual(stable);expect(f.f.calls.slice(calls).every(c=>c.mode==='readonly')).toBe(true)
 expect(await f.h.history(scope)).toMatchObject({readOnly:true,resolutions:1});await expect(f.g.prepare(scope)).rejects.toThrow();await expect(f.runtime.run(scope)).rejects.toThrow()
 expect(f.before()).toEqual(stable)
 },20000)
})
for(const compressed of [false,true])it(`Content plain stage dependency actual apply ID${compressed?1:0}`,async()=>{
 const f=await ordinaryV10Fixture('keep_both',3,compressed,true),r=await f.receive('stage-plain',null,1,false,{format:'plain'})
 expect(await f.owner.apply(scope,r.sequence)).toBe('applied');expect(await f.witness.inspect(scope)).toMatchObject({ordinaryTransitions:1})
},20000)
it('concurrent Notes serialize on full source and require manual retry',async()=>{
 const f=await ordinaryV10Fixture(),a=await f.receive('a'),token=await f.owner.prepare(scope,a.sequence),b=await f.receive('b'),before=f.before()
 await expect(f.owner.commit(token)).rejects.toMatchObject({code:'stale_source'});expect(f.before()).toEqual(before)
 await expect(f.owner.apply(scope,b.sequence)).rejects.toMatchObject({code:'note_orphan'})
 expect(await f.owner.apply(scope,a.sequence)).toBe('applied');expect(await f.owner.apply(scope,b.sequence)).toBe('applied')
 expect(await f.witness.inspect(scope)).toMatchObject({ordinaryTransitions:2})
})
it('two prepared owners cannot commit the same source twice',async()=>{
 const f=await ordinaryV10Fixture(),r=await f.receive(),other=f.create(),a=await f.owner.prepare(scope,r.sequence),b=await other.prepare(scope,r.sequence)
 expect(await f.owner.commit(a)).toBe('applied');const before=f.before()
 await expect(other.commit(b)).rejects.toMatchObject({code:'stale_source'});expect(f.before()).toEqual(before)
 expect(await other.apply(scope,r.sequence)).toBe('already_applied')
})
for(const kind of ['original','retained','orphan','fork','resurrection','own','unrelated','dependency'] as const)it(`explicit ${kind} barrier without writes`,async()=>{
 const f=await ordinaryV10Fixture(),baseline=f.f.db.legacyNoteHistory!.find((r:any)=>r.record_kind==='candidate'&&r.note_id==='baseline-deleted'&&r.header.revision===2) as any
 const r=kind==='original'?await f.receive('content-note',f.children[0]!.event_id,5):kind==='retained'?await f.receive('retained-note',f.children[1]!.event_id,5):kind==='resurrection'?await f.receive('baseline-deleted',baseline.event_id,3):kind==='fork'?await f.receive('baseline-live',crypto.randomUUID(),3):kind==='orphan'?await f.receive('absent',crypto.randomUUID(),2):kind==='own'?await f.receive('independent',null,1,false,{device:scope.deviceId}):kind==='unrelated'?await f.receive('elsewhere',null,1,false,{project:'q'}):await f.receive('independent',null,1,false,{dependencies:[crypto.randomUUID()],stage:'missing'})
 const before=f.before();await expect(f.owner.apply(scope,r.sequence)).rejects.toThrow();expect(f.before()).toEqual(before)
})
for(const kind of ['key','logout','profile','relogin'] as const)it(`revoked ${kind} prepared plan never commits`,async()=>{
 const f=await ordinaryV10Fixture(),r=await f.receive(),p=await f.owner.prepare(scope,r.sequence),before=f.before()
 if(kind==='key')f.f.lock();if(kind==='logout')await f.f.auth.logout();if(kind==='profile')f.revoke();if(kind==='relogin')await f.f.auth.login('fixture','fixture')
 await expect(f.owner.commit(p)).rejects.toThrow();expect(f.before()).toEqual(before)
})
for(const protocol of [2,3] as const)it(`rollback all source/history/proof and retry protocol${protocol}`,async()=>{
 const f=await ordinaryV10Fixture('keep_both',protocol),r=await f.receive(),p=await f.owner.prepare(scope,r.sequence),before=f.before()
 f.f.abort();await expect(f.owner.commit(p)).rejects.toMatchObject({code:'storage_failed'});expect(f.before()).toEqual(before)
 f.f.abort(false);expect(await f.create().apply(scope,r.sequence)).toBe('applied');expect(await f.witness.inspect(scope)).toMatchObject({ordinaryTransitions:1})
})
for(const kind of ['project','note','cursor','dependency','history','binding','other-project'] as const)it(`complete pre-commit CAS pins ${kind}`,async()=>{
 const f=await ordinaryV10Fixture(),r=await f.receive(),p=await f.owner.prepare(scope,r.sequence),d=f.f.db
 if(kind==='project')(d.localProjects![0] as any).source_revision++
 if(kind==='note')(d.localNotes![0] as any).envelope.ciphertext='AA'
 if(kind==='cursor')(d.cursors[0] as any).proven_prefix--
 if(kind==='dependency')(d.ownerHeads![0] as any).tip_event_ids=[crypto.randomUUID()]
 if(kind==='history')(d.legacyNoteHistory![0] as any).record_digest='0'.repeat(64)
 if(kind==='binding')(d.projectBindings[0] as any).remote_high_water++
 if(kind==='other-project')(d.localProjects!.find((r:any)=>r.project_id==='q') as any).source_revision++
 const before=f.before();await expect(f.owner.commit(p)).rejects.toThrow();expect(f.before()).toEqual(before)
})
it('N diagnostics, forged DTO, foreign runtime and reused owner tokens never authorize source handoff',async()=>{
 const f=await ordinaryV10Fixture(),r=await f.receive(),before=f.before(),snapshot=await f.f.local.readSnapshot(scope,'p')
 for(const fake of [{},await f.witness.inspect(scope)])await expect(f.f.local.prepareProtectedV10OrdinaryApply(scope,snapshot.token,fake as never)).rejects.toMatchObject({code:'closed'})
 const p=await f.owner.prepare(scope,r.sequence)
 await expect(f.create().commit(p)).rejects.toMatchObject({code:'closed'})
 expect(f.before()).toEqual(before);expect(await f.owner.commit(p)).toBe('applied')
 await expect(f.owner.commit(p)).rejects.toMatchObject({code:'closed'})
})
it('full-account corruption outside the project blocks prepare',async()=>{
 const f=await ordinaryV10Fixture(),r=await f.receive();(f.f.db.localProjects!.find((p:any)=>p.project_id==='q') as any).envelope.ciphertext='AA'
 const before=f.before();await expect(f.owner.prepare(scope,r.sequence)).rejects.toThrow();expect(f.before()).toEqual(before)
})
it('mid-transaction key invalidation aborts all staged writes',async()=>{
 const f=await ordinaryV10Fixture(),r=await f.receive();let armed=false
 const tx:BrowserSyncTransaction=(s,stores,mode,schedule)=>f.transaction(s,stores,mode,(tx,result,abort)=>{
  const proxy=new Proxy(tx,{get:(target,key)=>key==='objectStore'?(name:string)=>{
   const store=target.objectStore(name);return new Proxy(store,{get:(target,key)=>{
    const value=Reflect.get(target,key);if(armed&&name==='inbox'&&key==='put')return(...args:unknown[])=>{const request=value.apply(target,args);f.f.lock();return request}
    return typeof value==='function'?value.bind(target):value
   }})
  }:Reflect.get(target,key)})
  schedule(proxy,result,abort)
 })
 const owner=f.create(tx),p=await owner.prepare(scope,r.sequence),before=f.before();armed=true
 await expect(owner.commit(p)).rejects.toThrow();expect(f.before()).toEqual(before)
})
it('no account HTTP, writer activation, cursor advancement or publication from the dormant owner',async()=>{
 const f=await ordinaryV10Fixture(),r=await f.receive(),before=f.before(),http=vi.spyOn(globalThis,'fetch')
 expect(await f.owner.apply(scope,r.sequence)).toBe('applied');expect(http).not.toHaveBeenCalled()
 for(const name of ['outbox','receipts','cursors','legacyNoteIntents','ownerHeads','projectBindings'])expect(f.f.db[name]).toEqual(before[name])
 expect(amk.some(n=>n!==0)).toBe(true)
})

for(const kind of ['counter','project','projection'] as const)it(`predicted post-state rejects ${kind} tampering before any write`,async()=>{
 const f=await ordinaryV10Fixture(),r=await f.receive(),before=f.before(),calls=f.f.calls.length
 const actual=BrowserLocalAuthorityRuntime.prototype.prepareProtectedV10OrdinaryApply
 vi.spyOn(BrowserLocalAuthorityRuntime.prototype,'prepareProtectedV10OrdinaryApply').mockImplementation(async function(this:BrowserLocalAuthorityRuntime,...args){
  const prepared=await actual.apply(this,args)
  // Negative only: mutate the returned preview after a genuine private handoff.
  if(kind==='counter')prepared.source_revision++
  if(kind==='project')prepared.project.envelope.ciphertext='AA'
  if(kind==='projection')prepared.note.envelope.ciphertext='AA'
  return prepared
 })
 await expect(f.owner.prepare(scope,r.sequence)).rejects.toThrow()
 expect(f.before()).toEqual(before);expect(f.f.calls.slice(calls).every(c=>c.mode==='readonly')).toBe(true)
})
it('preparation uses a final fresh complete inventory and rejects real concurrent receipt',async()=>{
 const f=await ordinaryV10Fixture(),r=await f.receive();let reads=0
 const tx:BrowserSyncTransaction=async(...args)=>{
  if(++reads===2)await f.receive('racing-note') // local readSnapshot uses its own transaction.
  return f.transaction(...args)
 }
 const before=f.before();await expect(f.create(tx).prepare(scope,r.sequence)).rejects.toMatchObject({code:'stale_source'})
 expect(f.f.db.localProjects).toEqual(before.localProjects);expect(f.f.db.localNotes).toEqual(before.localNotes)
 expect(f.f.db.legacyNoteHistory).toEqual(before.legacyNoteHistory)
})
it('forged registered-device proof cannot construct an effective owner',async()=>{
 const f=await ordinaryV10Fixture(),r=await f.receive(),before=f.before()
 const {BrowserV10OrdinaryApply}=await import('./v10OrdinaryApply')
 const forged=new BrowserV10OrdinaryApply(f.transaction,f.f.localOwner,()=>true).bind(f.f.auth,f.f.bindings as never,f.f.keys as never,{} as never)
 await expect(forged.apply(scope,r.sequence)).rejects.toMatchObject({code:'closed'});expect(f.before()).toEqual(before)
})
