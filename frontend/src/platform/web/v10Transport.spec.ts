// @vitest-environment node
import {afterEach,it,expect,vi} from 'vitest'
import {transportFixture,keyedTransaction} from './v10Transport.testFixture'
import {plannerFixture} from './resolutionPlanner.testFixture'
import {scope} from './bootstrap.testFixture'
import {BrowserV10Transport} from './v10Transport'
import type {BrowserSyncTransaction} from './syncDurability'
afterEach(()=>vi.restoreAllMocks())
async function fixture(...args:Parameters<typeof transportFixture>){
 const f=await transportFixture(...args),create=(transaction=f.transaction)=>new BrowserV10Transport(transaction,f.current).bind(f.f.auth,f.f.bindings as never,f.f.keys as never,f.device)
 return{...f,transport:create(),create}
}

it('F2 exact join authenticates accepted H descendants and current source after pull advancement',async()=>{
 const f=await fixture('keep_both',2),row=f.received,page=JSON.stringify({protocol_version:3,encrypted_sync_version:3,items:[JSON.parse(row.wire_item)],next_cursor:f.sequence,has_more:false}),before=f.before()
 expect((before.cursors[0] as any).pull_cursor).toBeGreaterThan(f.sequence)
 expect(await f.transport.joinRetainedF2(scope,f.sequence-1,page,row.event_id)).toMatchObject({matched:true,retainedProtocol:2,commitAuthorized:false,ackAuthorized:false})
 expect(f.before()).toEqual(before)
 const child=f.f.db.inbox.find((r:any)=>r.server_sequence===f.children[0]) as any
 child.note_proof.outcome_digest='0'.repeat(64)
 const corrupt=f.before();await expect(f.transport.joinRetainedF2(scope,f.sequence-1,page,row.event_id)).rejects.toThrow();expect(f.before()).toEqual(corrupt)
})

it('reproduces both old transport failures after genuine F/H/G, without fabricated applied records',async()=>{
 const f=await transportFixture(),before=f.before()
 expect(f.received).toMatchObject({record_version:10,state:'note_resolution_proven'})
 expect((before.cursors[0] as any).proven_prefix).toBe(f.orphan-1)
 // The scheduling driver models the native account interval for this call.
 const {BrowserSyncDurability}=await import('./syncDurability'),old=new BrowserSyncDurability(f.transaction)
 await expect(old.commitInboundPage(scope,f.since,f.rawPage)).rejects.toMatchObject({code:'corrupt_record'})
 await expect(old.listReceived(scope)).rejects.toMatchObject({code:'corrupt_record'})
 expect(f.before()).toEqual(before)
})

for(const protocol of [2,3] as const)for(const strategy of ['choose_version','manual_merge','delete','keep_both'] as const)it(`exact original page after actual F/H/G protocol${protocol} ${strategy}`,async()=>{
 const f=await fixture(strategy,protocol,protocol===3),before=f.before()
 expect(await f.transport.replayInboundPage(scope,f.since,f.rawPage)).toEqual({committed_cursor:f.orphan,new_events:0,replayed_events:f.items.length,has_more:true})
 expect(await f.transport.listReceived(scope)).toEqual([expect.objectContaining({server_sequence:f.orphan,state:'received',record_version:1})])
 expect(await f.transport.listReceived(scope,1,f.orphan-1)).toHaveLength(1)
 expect(await f.transport.listReceived(scope,1,f.orphan)).toEqual([])
 expect(await f.transport.listReceived(scope,200,Number.MAX_SAFE_INTEGER)).toEqual([])
 expect(f.before()).toEqual(before)
 await expect(f.h.apply(scope,f.orphan)).rejects.toThrow()
 expect(await f.g.calculate(await f.g.prepare(scope))).toMatchObject({candidate_proven_prefix:f.orphan-1,first_blocking_sequence:f.orphan,ackAuthorized:false})
 await expect(f.f.proven.prepare(scope)).rejects.toThrow()
 expect(f.f.calls.slice(-1)[0]!.mode).toBe('readonly')
})
for(const compressed of [false,true])it(`mixed genuine Metadata/Stage/StageOrder/Legacy/Content/R/H/retained/conflict ID${compressed?1:0}`,async()=>{
 const f=await fixture('keep_both',3,compressed,true,true),before=f.before()
 expect(new Set(before.inbox.map((r:any)=>JSON.parse(r.wire_item).event.entity_type))).toEqual(new Set(['project_metadata','stage','stage_order','note']))
 expect(await f.transport.replayInboundPage(scope,f.since,f.rawPage)).toMatchObject({new_events:0,replayed_events:f.items.length})
 expect(await f.transport.listReceived(scope)).toEqual([expect.objectContaining({server_sequence:f.orphan})]);expect(f.before()).toEqual(before)
 expect(await f.g.calculate(await f.g.prepare(scope))).toMatchObject({first_blocking_sequence:f.children.at(-1),blocker:'unproven_family'})
})
for(const mutation of ['nonce','ciphertext','descriptor','event','sequence','protocol','truncated','since','next_cursor','empty'] as const)it(`page rejects ${mutation} with zero mutation`,async()=>{
 const f=await fixture(),p=JSON.parse(f.rawPage);let since=f.since
 if(mutation==='nonce'||mutation==='ciphertext'){const o=p.items[0].object;o[mutation]=(o[mutation][0]==='A'?'B':'A')+o[mutation].slice(1)}
 if(mutation==='descriptor')p.items[0].event.revision++
 if(mutation==='event')p.items[0].event.event_id=crypto.randomUUID()
 if(mutation==='sequence')p.items[0].event.server_sequence++
 if(mutation==='protocol')p.protocol_version=p.encrypted_sync_version=4
 if(mutation==='truncated')p.items.shift()
 if(mutation==='since')since--
 if(mutation==='next_cursor')p.next_cursor--
 if(mutation==='empty')p.items=[]
 const before=f.before();await expect(f.transport.replayInboundPage(scope,since,JSON.stringify(p))).rejects.toThrow();expect(f.before()).toEqual(before)
})
for(const mutation of ['missing','extra','duplicate_event','duplicate_sequence','foreign_account','foreign_device','malformed10','forged_proof','missing_proof_field','unknown_version','unknown_state','raw_mapping','protocol','wire','receipt_without_outbox'] as const)it(`stored ${mutation} fails replay and scan closed`,async()=>{
 const f=await fixture(),d=f.f.db,r=f.received as any
 if(mutation==='missing')d.inbox=d.inbox.filter((x:any)=>x.event_id!==r.event_id)
 if(mutation==='extra'){const extra=structuredClone(d.inbox.at(-1)) as any;extra.server_sequence=f.since+0.5;d.inbox.push(extra)}
 if(mutation==='duplicate_event')d.inbox.push({...r,server_sequence:f.orphan})
 if(mutation==='duplicate_sequence')d.inbox.push({...r,event_id:crypto.randomUUID()})
 if(mutation==='foreign_account')r.account_id=crypto.randomUUID()
 if(mutation==='foreign_device')r.device_id=crypto.randomUUID()
 if(mutation==='malformed10')r.extra=true
 if(mutation==='forged_proof')r.resolution_proof.outcome_digest='0'.repeat(64)
 if(mutation==='missing_proof_field')delete r.resolution_proof.output_digests
 if(mutation==='unknown_version')r.record_version=11
 if(mutation==='unknown_state')r.state='applied'
 if(mutation==='raw_mapping'){const w=JSON.parse(r.wire_item);w.event.operation='resolution';r.wire_item=JSON.stringify(w)}
 if(mutation==='protocol')r.protocol_version=2
 if(mutation==='wire')r.wire_item=r.wire_item.replace('nonce','nonce_broken')
 if(mutation==='receipt_without_outbox')d.receipts.push({event_id:r.event_id,server_sequence:r.server_sequence,account_id:scope.accountId})
 const before=f.before();await expect(f.transport.replayInboundPage(scope,f.since,f.rawPage)).rejects.toThrow();await expect(f.transport.listReceived(scope)).rejects.toThrow();expect(f.before()).toEqual(before)
})
for(const key of ['accountId','canonicalUserId','deviceId'] as const)it(`rejects foreign scope ${key}`,async()=>{
 const f=await fixture(),s={...scope,[key]:crypto.randomUUID()},before=f.before()
 await expect(f.transport.replayInboundPage(s,f.since,f.rawPage)).rejects.toThrow();await expect(f.transport.listReceived(s)).rejects.toThrow();expect(f.before()).toEqual(before)
})
for(const kind of ['key','rotation','logout','expired','profile'] as const)it(`lifecycle ${kind}`,async()=>{
 const f=await fixture()
 if(kind==='key')f.f.lock();if(kind==='rotation')await f.f.auth.login('fixture','fixture');if(kind==='logout')await f.f.auth.logout();if(kind==='expired')vi.spyOn(f.f.auth,'isCurrent').mockReturnValue(false);if(kind==='profile')f.close()
 const before=f.before();await expect(f.transport.replayInboundPage(scope,f.since,f.rawPage)).rejects.toThrow();await expect(f.transport.listReceived(scope)).rejects.toThrow();expect(f.before()).toEqual(before)
})
it('later pull advancement never authorizes arbitrary older-page replay or rewind',async()=>{
 const f=await fixture(),row=structuredClone(f.f.db.inbox.at(-1)) as any
 row.server_sequence++;row.event_id=crypto.randomUUID();const w=JSON.parse(row.wire_item);w.event.server_sequence++;w.event.event_id=row.event_id;row.wire_item=JSON.stringify(w)
 f.f.db.inbox.push(row);(f.f.db.cursors[0] as any).pull_cursor++
 const before=f.before();await expect(f.transport.replayInboundPage(scope,f.since,f.rawPage)).rejects.toMatchObject({code:'stale_cursor'});expect(await f.transport.listReceived(scope,1,f.orphan)).toEqual([row]);expect(f.before()).toEqual(before)
})
for(const [limit,after] of [[0,0],[201,0],[1,-1],[1,1.5],[1,NaN],[1,Infinity]])it(`bounded arguments ${limit}/${after}`,async()=>{
 const f=await fixture();await expect(f.transport.listReceived(scope,limit,after)).rejects.toThrow()
})
for(const fault of ['request','abort','key','logout','profile','stale_inventory','bad_event_index','bad_primary_key'] as const)it(`native scheduling boundary ${fault}`,async()=>{
 const f=await fixture();let transactions=0,armed=false
 const transaction:BrowserSyncTransaction=(s,stores,mode,schedule)=>{
  const final=++transactions===2
  if(final&&fault==='stale_inventory')(f.f.db.cursors[0] as any).next_local_ordinal++
  return f.transaction(s,stores,mode,(tx,result,abort)=>{
  expect(mode).toBe('readonly')
  const proxy=new Proxy(tx,{get:(tx,key)=>key==='objectStore'?(name:string)=>{
   const store=tx.objectStore(name);return new Proxy(store,{get:(store,key)=>{
    if(key==='index'&&final&&fault==='bad_event_index')return()=>({get:()=>store.get([s.accountId,f.orphan])})
    const value=Reflect.get(store,key)
    if(typeof value==='function')return(...args:any[])=>{
     const request=value.apply(store,args)
     if(final&&!armed&&name==='inbox'){
      armed=true
      if(fault==='request')throw new DOMException('injected','UnknownError')
      if(fault==='abort')abort(Error('injected abort'))
      if(fault==='key')f.f.lock();if(fault==='logout')void f.f.auth.logout();if(fault==='profile')f.close()
     }
     if(final&&fault==='bad_primary_key'&&key==='openCursor'){
      const descriptor=Object.getOwnPropertyDescriptor(request,'onsuccess')!;Object.defineProperty(request,'onsuccess',{get:descriptor.get,set(handler){descriptor.set!.call(request,function(){if(request.result)request.result.primaryKey=[s.accountId,-1];handler()})}})
     }
     return request
    }
    return value
   }})
  }:Reflect.get(tx,key)})
  schedule(proxy,result,abort)
 })}
 const runtime=f.create(transaction),before=f.before()
 await expect(fault==='bad_primary_key'?runtime.listReceived(scope):runtime.replayInboundPage(scope,f.since,f.rawPage)).rejects.toThrow()
 if(fault==='stale_inventory')(before.cursors[0] as any).next_local_ordinal++
 expect(f.before()).toEqual(before)
})

for(const protocol of [2,3] as const)it(`genuine pending protocol${protocol} Resolution remains discoverable without apply authority`,async()=>{
 const f=await plannerFixture('choose_version',2,false,protocol===2),transport=new BrowserV10Transport(keyedTransaction(f.f.transaction),()=>true).bind(f.f.auth,f.f.bindings as never,f.f.keys as never,f.device),before=f.before()
 expect(await transport.listReceived(scope)).toEqual([f.received])
 expect(f.received).toMatchObject({record_version:1,state:'received'});expect(f.received).not.toHaveProperty('resolution_proof');expect(f.before()).toEqual(before)
})
it('account inventory row budget fails explicitly rather than truncating the received queue',async()=>{
 const f=await fixture(),row={account_id:scope.accountId,server_sequence:1}
 // Negative oversized account, never used as an applied positive specimen.
 f.f.db.inbox=Array.from({length:16385},()=>row)
 const before=f.before();await expect(f.transport.listReceived(scope)).rejects.toMatchObject({code:'resource_limit'});expect(f.before()).toEqual(before)
})

function fullPages(f:Awaited<ReturnType<typeof fixture>>) {
 const rows=[...f.f.db.inbox].sort((a:any,b:any)=>a.server_sequence-b.server_sequence) as any[]
 return rows.map((r,i)=>JSON.stringify({protocol_version:r.protocol_version,encrypted_sync_version:r.protocol_version,
  items:[JSON.parse(r.wire_item)],next_cursor:r.server_sequence,has_more:i<rows.length-1}))
}
for(const protocol of [2,3] as const)for(const strategy of ['choose_version','manual_merge','delete','keep_both'] as const)it(`complete original account pages protocol${protocol} ${strategy} authenticate without writes`,async()=>{
 const f=await fixture(strategy,protocol,protocol===3),before=f.before(),pages=fullPages(f)
 expect(await f.transport.replayInboundHistory(scope,pages)).toEqual({committed_cursor:f.orphan,new_events:0,replayed_events:pages.length,has_more:false})
 expect(f.before()).toEqual(before)
 expect(await f.transport.replayInboundHistory(scope,pages)).toMatchObject({new_events:0})
 expect(f.before()).toEqual(before)
})
for(const kind of ['missing','reorder','wire','more','tail','protocol','proof','source','logout'] as const)it(`complete page reconciliation rejects ${kind} without writes`,async()=>{
 const f=await fixture(),pages=fullPages(f)
 if(kind==='missing')pages.splice(1,1)
 if(kind==='reorder')[pages[0],pages[1]]=[pages[1]!,pages[0]!]
 if(['wire','more','tail','protocol'].includes(kind)){
  const p=JSON.parse(pages[0]!);if(kind==='wire')p.items[0].object.ciphertext='AA';if(kind==='more')p.has_more=false;if(kind==='tail')p.next_cursor++;if(kind==='protocol')p.protocol_version=99
  pages[0]=JSON.stringify(p)
 }
 if(kind==='proof')(f.f.db.inbox.find((r:any)=>r.state==='note_resolution_proven') as any).resolution_proof.outcome_digest='0'.repeat(64)
 if(kind==='source')(f.f.db.localNotes[0] as any).envelope.ciphertext='AA'
 if(kind==='logout')await f.f.auth.logout()
 const before=f.before();await expect(f.transport.replayInboundHistory(scope,pages)).rejects.toThrow();expect(f.before()).toEqual(before)
})
for(const compressed of [false,true])it(`complete-history replay preserves genuine mixed owners and an unresolved prefix ID${compressed?1:0}`,async()=>{
 const f=await fixture('keep_both',3,compressed,true,true),before=f.before()
 expect(await f.transport.replayInboundHistory(scope,fullPages(f))).toMatchObject({new_events:0,replayed_events:before.inbox.length})
 expect(await f.g.calculate(await f.g.prepare(scope))).toMatchObject({first_blocking_sequence:f.children.at(-1),blocker:'unproven_family',ackAuthorized:false})
 expect(f.before()).toEqual(before)
})

it('full-history replay bounds page count and aggregate input before verification',async()=>{
 const f=await fixture(),before=f.before(),calls=f.f.calls.length
 for(const pages of [[],Array(16385).fill(''),['x'.repeat(17*1024*1024),'x'.repeat(17*1024*1024)]]){
  await expect(f.transport.replayInboundHistory(scope,pages)).rejects.toThrow()
 }
 expect(f.f.calls.length).toBe(calls);expect(f.before()).toEqual(before)
})
