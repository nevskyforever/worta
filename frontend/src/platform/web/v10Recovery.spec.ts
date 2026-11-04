// @vitest-environment node
import {afterEach,it,expect,vi} from 'vitest'
import {recoveryFixture} from './v10Recovery.testFixture'
import {scope,amk} from './bootstrap.testFixture'
import {BrowserV10Transport} from './v10Transport'
import {encryptObjectBytes} from '@/crypto'
import {encodeBase64Url} from '@/api/base64url'
import {frameContentNote} from '@/cloud/contentNoteCodec'
import {encodeNoteSyncResolutionV2} from '@/cloud/noteSyncResolutionV2Codec'
import {createNoteSyncPlaintext} from '@/cloud/encryptedSyncProtocol'
import {encodeNoteSyncPlaintext} from '@/cloud/noteSyncCodec'
import type {BrowserSyncTransaction} from './syncDurability'
import {BrowserStageOrderRuntime} from './stageOrderApply'
import {BrowserPersistenceError} from './persistence'
afterEach(()=>vi.restoreAllMocks())
for(const protocol of [2,3] as const)for(const strategy of ['choose_version','manual_merge','delete','keep_both'] as const)it(`real pending-to-F/H/G protocol${protocol}/${strategy}`,async()=>{
 const f=await recoveryFixture(strategy,protocol,protocol===3),before=f.before()
 expect(before.legacyNoteHistory).toEqual([])
 await expect(f.planner.prepare(scope,f.r.sequence)).rejects.toThrow() // Genuine missing parents/publication.
 const result=await f.runtime.run(scope,{maxEvents:32,replay:{since:f.since,rawPage:f.rawPage}})
 expect(result).toMatchObject({ready:false,declarationAllowed:false,ackAuthorized:false,blocker:null,more:false,replayed_events:f.items.length,readiness:{ready:false}})
 expect(result.applied.map(r=>r.route)).toEqual(['metadata',protocol===2?'legacy':'content',protocol===2?'legacy':'content',protocol===2?'legacy':'content','resolution',...f.children.map(()=>'continuation')])
 expect((f.f.db.cursors[0] as any)).toMatchObject({pull_cursor:f.sequence,proven_prefix:f.sequence,ack_cursor:0})
 for(const row of f.f.db.inbox as any[])expect(row.wire_item).toBe((before.inbox.find((r:any)=>r.event_id===row.event_id) as any).wire_item)
 const after=f.before(),again=await f.runtime.run(scope,{replay:{since:f.since,rawPage:f.rawPage}})
 expect(again.applied).toEqual([]);expect(again.blocker).toBeNull();expect(f.before()).toEqual(after)
})
for(const protocol of [2,3] as const)it(`bounded passes and a fresh runtime resume protocol${protocol}`,async()=>{
 const f=await recoveryFixture('keep_both',protocol,true),seen:number[]=[]
 for(let i=0;i<10;i++){
  const r=await f.create().run(scope,{maxEvents:1});seen.push(...r.applied.map(x=>x.sequence))
  if(!r.more)break
  expect(r.blocker?.kind).toBe('budget_exhausted')
 }
 expect(seen).toEqual(Array.from({length:f.sequence},(_,i)=>i+1));expect((f.f.db.cursors[0] as any).proven_prefix).toBe(f.sequence)
})
it('missing binding is explicit; a genuine import enables a fresh dependency retry',async()=>{
 const f=await recoveryFixture(),binding=f.f.db.projectBindings[0] as any
 // Negative missing-authority fixture. The positive retry uses importRemote;
 // it never injects a proof, applied row, source projection or candidate.
 f.f.db.projectBindings=[];const before=f.before(),blocked=await f.runtime.run(scope)
 expect(blocked).toMatchObject({blocker:{sequence:1,kind:'dependency_unavailable',retryable:true},applied:[],more:true});expect(f.before()).toEqual(before)
 await f.f.projects.importRemote(scope,{project_id:binding.project_id,bootstrap_id:binding.bootstrap_id,remote_state:'active',remote_high_water:0})
 expect((await f.runtime.run(scope,{maxEvents:32})).blocker).toBeNull()
})
it('imports the actual binding before applying Stage and StageOrder after restart',async()=>{
 const f=await recoveryFixture('choose_version',3,false,true),binding=f.f.db.projectBindings[0] as any
 f.f.db.projectBindings=[]
 const before=f.before(),blocked=await f.runtime.run(scope,{maxEvents:32})
 expect(blocked).toMatchObject({blocker:{sequence:1,kind:'dependency_unavailable',retryable:true},applied:[]})
 expect(f.before()).toEqual(before)
 const order=(f.f.db.inbox as any[]).find(row=>JSON.parse(row.wire_item).event.entity_type==='stage_order')
 expect(order.state).toBe('received');expect(order.stage_order_proof).toBeUndefined()
 await f.f.projects.importRemote(scope,{project_id:binding.project_id,bootstrap_id:binding.bootstrap_id,remote_state:'active',remote_high_water:0})
 const result=await f.create().run(scope,{maxEvents:32})
 expect(result.blocker).toBeNull()
 const saved=(f.f.db.inbox as any[]).find(row=>row.event_id===order.event_id)
 expect(saved.state).toBe('stage_order_proven');expect(saved.stage_order_proof).toBeTruthy();expect(saved.wire_item).toBe(order.wire_item)
})
it('retains StageOrder and retries after its owner reports a temporarily missing Stage',async()=>{
 const f=await recoveryFixture('choose_version',3,false,true)
 const original=BrowserStageOrderRuntime.prototype.prepare
 const fault=vi.spyOn(BrowserStageOrderRuntime.prototype,'prepare').mockImplementationOnce(async()=>{throw new BrowserPersistenceError('stage_dependency_missing')})
 const blocked=await f.runtime.run(scope,{maxEvents:32})
 expect(blocked.blocker).toMatchObject({sequence:3,kind:'dependency_unavailable',retryable:true})
 const before=f.before(),row=(f.f.db.inbox as any[]).find(r=>r.server_sequence===3)
 expect(row.state).toBe('received');expect(row.stage_order_proof).toBeUndefined()
 fault.mockRestore()
 expect(BrowserStageOrderRuntime.prototype.prepare).toBe(original)
 expect((await f.create().run(scope,{maxEvents:32})).blocker).toBeNull()
 expect((f.f.db.inbox as any[]).find(r=>r.server_sequence===3).wire_item).toBe((before.inbox as any[]).find(r=>r.server_sequence===3).wire_item)
})
for(const max of [0,33,1.5,NaN,Infinity])it(`rejects event budget ${max}`,async()=>{
 const f=await recoveryFixture(),before=f.before();await expect(f.runtime.run(scope,{maxEvents:max})).rejects.toThrow();expect(f.before()).toEqual(before)
})
for(const key of ['accountId','canonicalUserId','deviceId'] as const)it(`rejects scope ${key}`,async()=>{
 const f=await recoveryFixture(),before=f.before();await expect(f.runtime.run({...scope,[key]:crypto.randomUUID()})).rejects.toThrow();expect(f.before()).toEqual(before)
})
for(const kind of ['key','logout','profile','epoch'] as const)it(`pass revocation ${kind}`,async()=>{
 const f=await recoveryFixture();if(kind==='key')f.f.lock();if(kind==='logout')await f.f.auth.logout();if(kind==='profile')f.close();if(kind==='epoch')await f.f.auth.login('fixture','fixture')
 const before=f.before();await expect(f.runtime.run(scope)).rejects.toThrow();expect(f.before()).toEqual(before)
})
for(const kind of ['wire','unknown_version','unknown_state','invalid_protocol','foreign_device','collision'] as const)it(`no mutation for malformed queue ${kind}`,async()=>{
 const f=await recoveryFixture(),row=f.f.db.inbox[1] as any
 if(kind==='wire')row.wire_item=row.wire_item.replace('nonce','bad_nonce')
 if(kind==='unknown_version')row.record_version=11
 if(kind==='unknown_state')row.state='applied'
 if(kind==='invalid_protocol')row.protocol_version=4
 if(kind==='foreign_device')row.device_id=crypto.randomUUID()
 if(kind==='collision')f.f.db.inbox.push(structuredClone(row))
 const before=f.before();await expect(f.runtime.run(scope,{maxEvents:32})).rejects.toThrow();expect(f.before()).toEqual(before)
})
it('a real gap is reported before later pending source writes',async()=>{
 const f=await recoveryFixture();f.f.db.inbox=f.f.db.inbox.filter((r:any)=>r.server_sequence!==1)
 const before=f.before(),r=await f.runtime.run(scope,{maxEvents:32});expect(r).toMatchObject({applied:[],blocker:{sequence:1,kind:'missing_sequence'}});expect(f.before()).toEqual(before)
})
it('invalid exact replay rejects before any apply, and never rewinds cursors',async()=>{
 const f=await recoveryFixture(),page=JSON.parse(f.rawPage);page.items.shift()
 const before=f.before();await expect(f.runtime.run(scope,{replay:{since:f.since,rawPage:JSON.stringify(page)}})).rejects.toThrow();expect(f.before()).toEqual(before)
})
it('same runtime cannot start overlapping recovery passes',async()=>{
 const f=await recoveryFixture(),p=f.runtime.run(scope,{maxEvents:1});await expect(f.runtime.run(scope)).rejects.toThrow();expect((await p).applied).toHaveLength(1)
})

async function append(f:Awaited<ReturnType<typeof recoveryFixture>>,kind:'fork'|'orphan'|'resurrection'|'resolution'|'own'|'new_note',protocol:2|3=3){
 const sequence=(f.f.db.cursors[0] as any).pull_cursor+1,r=structuredClone(f.resolution),child=f.children[0]!,original=r.result.note,id=kind==='new_note'?'unaccepted-new-note':'content-note'
 r.header.event_id=crypto.randomUUID()
 const h={event_id:crypto.randomUUID(),project_id:'p',entity_id:id,entity_type:'note' as const,operation:'upsert' as const,revision:kind==='new_note'?1:kind==='resurrection'?5:4,updated_at:r.header.updated_at,deleted_at:null}
 const parent=kind==='orphan'?crypto.randomUUID():kind==='resurrection'?child.event_id:kind==='new_note'?null:f.r.event_id
 const live='content'in original?original:f.parents[0]!.payload.note
 const plain=createNoteSyncPlaintext(h,parent,{...live,id}).plaintext
 const payload=kind==='resolution'?r:plain,peer=kind==='own'?scope.deviceId:'44444444-4444-4444-8444-444444444444',header=payload.header
 const frame=protocol===2?(kind==='resolution'?encodeNoteSyncResolutionV2(r):encodeNoteSyncPlaintext(plain)):frameContentNote({version:1,account_id:scope.canonicalUserId,device_id:peer,dependencies:{bootstrap_id:f.f.b.bootstrap_id,metadata_event_id:f.m.event.header.event_id,stage_event_ids:original.stage_id?(f.f.db.ownerHeads!.find((r:any)=>r.owner_kind==='stage') as any).tip_event_ids:[]},event:payload})
 const o=await encryptObjectBytes(amk,{userId:scope.canonicalUserId,projectId:'p',entityId:id,entityType:'note'},frame)
 try{await f.f.sync.commitInboundPage(scope,sequence-1,JSON.stringify({protocol_version:protocol,encrypted_sync_version:protocol,items:[{event:{event_id:header.event_id,device_id:peer,server_sequence:sequence,project_id:'p',entity_id:id,entity_type:'note',operation:protocol===2?header.operation:'event',revision:header.revision,updated_at:header.updated_at,deleted_at:null},object:{crypto_version:1,aad_version:1,nonce:encodeBase64Url(o.nonce),ciphertext:encodeBase64Url(o.ciphertext)}}],next_cursor:sequence,has_more:false}))}finally{frame.fill(0);o.nonce.fill(0);o.ciphertext.fill(0)}
 return sequence
}
for(const protocol of [2,3] as const)for(const kind of ['fork','orphan','resurrection','resolution','own','new_note'] as const)it(`protocol${protocol} explicit blocker ${kind}, retained wire and source`,async()=>{
 const f=await recoveryFixture(kind==='resurrection'?'delete':'keep_both',protocol);await f.runtime.run(scope,{maxEvents:32})
 const sequence=await append(f,kind,protocol),before=f.before(),r=await f.runtime.run(scope,{maxEvents:32})
 expect(r.ready).toBe(false);expect(r.blocker).toMatchObject({sequence,kind:kind==='fork'?'unresolved_fork':kind==='orphan'?'dependency_unavailable':kind==='resolution'?'unsupported_generation':'unsupported_route'})
 if(kind!=='fork')expect(f.before()).toEqual(before)
 else{
  const after=f.before();expect(after.localNotes).toEqual(before.localNotes);expect(after.localProjects).toEqual(before.localProjects)
  expect((await f.runtime.run(scope)).blocker).toMatchObject({kind:'unresolved_fork',sequence});expect(f.before()).toEqual(after)
 }
 expect((f.f.db.cursors[0] as any).ack_cursor).toBe(0)
})
for(const fault of ['abort','key','logout','profile','stale_inventory'] as const)it(`atomic owner boundary fails closed ${fault}, earlier events remain restartable`,async()=>{
 const f=await recoveryFixture();await f.runtime.run(scope,{maxEvents:3})
 let armed=false,txCount=0
 const transaction:BrowserSyncTransaction=(s,stores,mode,schedule)=>{
  if(fault==='stale_inventory'&&++txCount===4)(f.f.db.cursors[0] as any).next_local_ordinal++
  return f.transaction(s,stores,mode,(tx,result,abort)=>{
   const proxy=new Proxy(tx,{get:(tx,key)=>key==='objectStore'?(name:string)=>{
    const store=tx.objectStore(name);return new Proxy(store,{get:(store,key)=>{
     const value=Reflect.get(store,key)
     if(typeof value!=='function')return value
     return(...args:any[])=>{
      const request=value.apply(store,args)
      if(!armed&&mode==='readwrite'&&name==='inbox'&&key==='put'&&args[0].record_version===10){armed=true
       if(fault==='abort')abort(Error('native request failure'));if(fault==='key')f.f.lock();if(fault==='logout')void f.f.auth.logout();if(fault==='profile')f.close()
      }
      return request
     }
    }})
   }:Reflect.get(tx,key)})
   schedule(proxy,result,abort)
  })
 }
 const source=f.before();await expect(f.create(transaction).run(scope,{maxEvents:32})).rejects.toThrow()
 expect(f.f.db.localNotes).toEqual(source.localNotes);expect(f.f.db.localProjects).toEqual(source.localProjects)
 expect(f.f.db.inbox.find((r:any)=>r.server_sequence===f.r.sequence)).toMatchObject({record_version:1,state:'received'})
 if(fault==='abort')expect((await f.create().run(scope,{maxEvents:32})).blocker).toBeNull()
})
it('no new HTTP/ACK/capability surface and J replay remains read-only after recovery',async()=>{
 const f=await recoveryFixture(),fetch=vi.spyOn(globalThis,'fetch').mockRejectedValue(Error('unexpected HTTP'))
 await f.runtime.run(scope,{maxEvents:32});expect(fetch).not.toHaveBeenCalled();expect(Object.keys(f.runtime)).toEqual(['run'])
 const before=f.before(),j=new BrowserV10Transport(f.transaction,()=>true).bind(f.f.auth,f.f.bindings as never,f.f.keys as never,f.device)
 await j.replayInboundPage(scope,f.since,f.rawPage);expect(f.before()).toEqual(before);await expect(f.f.proven.prepare(scope)).rejects.toThrow()
})
for(const protocol of [2,3] as const)it(`authenticated pending ciphertext failure protocol${protocol} cannot mint source/proof`,async()=>{
 const f=await recoveryFixture('choose_version',protocol);await f.runtime.run(scope,{maxEvents:1})
 const row=f.f.db.inbox.find((r:any)=>r.event_id===f.root.event_id) as any,w=JSON.parse(row.wire_item)
 w.object.ciphertext=(w.object.ciphertext[0]==='A'?'B':'A')+w.object.ciphertext.slice(1);row.wire_item=JSON.stringify(w)
 const before=f.before();await expect(f.runtime.run(scope,{maxEvents:32})).rejects.toThrow();expect(f.before()).toEqual(before)
})
it('G cursor commit failure retains accepted source events and resumes without rewriting them',async()=>{
 const f=await recoveryFixture(),transaction:BrowserSyncTransaction=(s,stores,mode,schedule)=>f.transaction(s,stores,mode,(tx,result,abort)=>{
  const proxy=new Proxy(tx,{get:(tx,key)=>key==='objectStore'?(name:string)=>{
   const store=tx.objectStore(name);return new Proxy(store,{get:(store,key)=>{
    const value=Reflect.get(store,key);if(typeof value!=='function')return value
    return(...args:any[])=>{const request=value.apply(store,args);if(mode==='readwrite'&&name==='cursors'&&key==='put')abort(Error('G cursor failure'));return request}
   }})
  }:Reflect.get(tx,key)})
  schedule(proxy,result,abort)
 })
 await expect(f.create(transaction).run(scope,{maxEvents:5})).rejects.toThrow()
 const after=f.before();expect((after.cursors[0] as any).proven_prefix).toBe(0);expect(after.inbox.find((r:any)=>r.event_id===f.r.event_id)).toMatchObject({record_version:10})
 const old=after.legacyNoteHistory!.filter((r:any)=>['resolution','group_transition'].includes(r.record_kind))
 expect((await f.create().run(scope,{maxEvents:32})).blocker).toBeNull();expect(f.f.db.legacyNoteHistory).toEqual(expect.arrayContaining(old))
})
it('unknown future history cannot be hidden behind an empty received queue',async()=>{
 const f=await recoveryFixture();await f.runtime.run(scope,{maxEvents:32})
 const row=f.f.db.legacyNoteHistory!.find((r:any)=>r.record_kind==='head') as any;row.record_version=99
 const before=f.before();await expect(f.runtime.run(scope)).rejects.toThrow();expect(f.before()).toEqual(before)
})
it('an earlier G-unqualified applied family stops later continuation, without borrowing its source authority',async()=>{
 const f=await recoveryFixture('keep_both',3,false,true),transaction:BrowserSyncTransaction=(s,stores,mode,schedule)=>f.transaction(s,stores,mode,(tx,result,abort)=>{
  const proxy=new Proxy(tx,{get:(tx,key)=>key==='objectStore'?(name:string)=>{
   const store=tx.objectStore(name);if(name!=='cursors'||mode!=='readwrite')return store
   return new Proxy(store,{get:(store,key)=>key==='put'?()=>{abort(Error('G abort'));return store.get([s.accountId,s.deviceId])}:Reflect.get(store,key)})
  }:Reflect.get(tx,key)})
  schedule(proxy,result,abort)
 })
 await expect(f.create(transaction).run(scope,{maxEvents:32})).rejects.toThrow()
 f.f.db.ownerHeads=f.f.db.ownerHeads!.filter((r:any)=>r.owner_kind!=='stage_order')
 const sequence=await append(f,'orphan'),before=f.before(),r=await f.runtime.run(scope,{maxEvents:32})
 expect(r.blocker).toMatchObject({sequence:3,kind:'unqualified_family'});expect(r.applied).toEqual([])
 expect(sequence).toBeGreaterThan(3);(before.cursors[0] as any).proven_prefix=2;expect(f.before()).toEqual(before)
})
for(const compressed of [false,true])it(`real Metadata/Stage/StageOrder dependencies ID${compressed?1:0}`,async()=>{
 const f=await recoveryFixture('keep_both',3,compressed,true)
 const r=await f.runtime.run(scope,{maxEvents:32})
 expect(r.blocker).toBeNull();expect(r.applied.slice(0,3).map(x=>x.route)).toEqual(['metadata','stage','stage_order']);expect(r.prefix?.candidate_proven_prefix).toBe(f.sequence)
})
