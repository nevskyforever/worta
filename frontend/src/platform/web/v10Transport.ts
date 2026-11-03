import type {NormalUserAuthRuntime} from '@/auth/userAuth'
import type {AuthoritativeAccountBinding} from '@/auth/accountBinding'
import type {RuntimeKeyContext} from '@/auth/keyContext'
import type {AccountMasterKey} from '@/crypto'
import {BrowserPersistenceError} from './persistence'
import {currentBrowserDevice,type VerifiedBrowserDevice} from './deviceRegistration'
import {snapshotBrowserSyncScope,validateBrowserInboundPage,validateBrowserStoredUploadPair,immutableWireIdentity as identity,type BrowserSyncScope,type BrowserSyncTransaction,type BrowserReceivedInboxRecord} from './syncDurability'
import {readFutureInboxRecord,resolutionSame as same,type ResolutionReference} from './resolutionRecords'
import {resolutionInventoryStores,scheduleResolutionInventory,MAX_PLANNER_ROWS,MAX_PLANNER_INVENTORY_BYTES,type ResolutionInventorySnapshot} from './resolutionPlanner'
import {authenticateResolutionHistory} from './resolutionAuthentication'
import {authenticateContinuationHistory,verifyContinuationSources,MAX_CONTINUATION_MATERIAL_BYTES} from './resolutionHistorical'

export interface V10PageReplayResult {committed_cursor:number;new_events:0;replayed_events:number;has_more:boolean}
export interface RetainedF2JoinResult {
 readonly readOnly:true;readonly matched:true;readonly eventId:string;readonly serverSequence:number
 readonly receivedProtocol:3;readonly retainedProtocol:2;readonly commitAuthorized:false;readonly ackAuthorized:false
}

function fail(code:'closed'|'corrupt_record'|'stale_source'|'stale_cursor'|'conflicting_record'='corrupt_record'):never{throw new BrowserPersistenceError(code)}
const safe=(n:number)=>Number.isSafeInteger(n)&&n>=0

/** Explicit J transport reader, absent from normal persistence composition.
 * New pages still use the existing transport. This surface ONLY replays the
 * current committed interval or discovers received ciphertext; it never writes,
 * applies, produces certificates, declares capabilities or sends HTTP. */
export class BrowserV10Transport {
 constructor(private readonly transaction:BrowserSyncTransaction,private readonly profileCurrent:()=>boolean){}
 bind(auth:NormalUserAuthRuntime,bindings:AuthoritativeAccountBinding,keys:RuntimeKeyContext,device:VerifiedBrowserDevice){return new BrowserV10TransportRuntime(this.transaction,this.profileCurrent,auth,bindings,keys,device)}
}
export class BrowserV10TransportRuntime {
 constructor(private readonly transaction:BrowserSyncTransaction,private readonly profileCurrent:()=>boolean,private readonly auth:NormalUserAuthRuntime,private readonly bindings:AuthoritativeAccountBinding,private readonly keys:RuntimeKeyContext,private readonly device:VerifiedBrowserDevice){}

 private async inspect<T>(s:BrowserSyncScope,schedule:(tx:IDBTransaction,v:ResolutionInventorySnapshot,current:()=>boolean,result:(v:T)=>void,abort:(e:unknown)=>void)=>void,verify?:(v:ResolutionInventorySnapshot,amk:AccountMasterKey,current:()=>boolean)=>Promise<void>):Promise<T>{
  const initial=this.auth.requireContext(),{context}=await this.bindings.ensureForCurrentUser(s.accountId),lease=this.keys.leaseForAccount(s.accountId)
  const current=()=>this.profileCurrent()&&this.auth.isCurrent(initial)&&this.auth.isCurrent(context)&&!!lease?.isCurrent()&&currentBrowserDevice(this.device,s,this.auth)
  if(!lease||lease.authEpoch!==context.authEpoch||lease.localAccountId!==s.accountId||lease.canonicalUserId!==s.canonicalUserId||context.userId!==s.canonicalUserId||!current())fail('closed')
  let activeAbort:((e:unknown)=>void)|undefined
  const stop=()=>activeAbort?.(new BrowserPersistenceError('closed')),unAuth=this.auth.onInvalidated(stop),unKey=this.keys.onInvalidated(stop)
  const transaction:BrowserSyncTransaction=(scope,stores,mode,callback)=>this.transaction(scope,stores,mode,(tx,result,abort)=>{
   activeAbort=abort;if(!current()){abort(new BrowserPersistenceError('closed'));return}callback(tx,result,abort)
  })
  try{
   // Accepted F inventory limits: 16384 rows/store, 32 MiB total. A large
   // account fails closed rather than silently truncating history or the scan.
   const v=await transaction<ResolutionInventorySnapshot>(s,resolutionInventoryStores,'readonly',(tx,result,abort)=>scheduleResolutionInventory(tx,s,current,result,abort))
   activeAbort=undefined
   const rows=v.rows.map(raw=>readFutureInboxRecord(raw,s)),events=new Set<string>(),sequences=new Set<number>()
   for(const row of rows){if(events.has(row.event_id)||sequences.has(row.server_sequence)||row.server_sequence>v.cursor.pull_cursor)fail();events.add(row.event_id);sequences.add(row.server_sequence)}
   for(const receipt of v.receipts){const r=receipt as {event_id:string};validateBrowserStoredUploadPair(v.out.find(raw=>(raw as {event_id:string}).event_id===r.event_id),receipt,s)}
   // Structural inbox10 alone never authenticates its proof. Reuse the accepted
   // historical reader outside IDB, including original AEAD, closure and proof
   // digests. This returns NO G certificate, current-source or prefix grant.
   if(rows.some(row=>row.record_version===10)||v.history.some(raw=>(raw as {record_kind:string}).record_kind==='resolution'))await lease.use(amk=>authenticateResolutionHistory(s,v,amk,current,MAX_CONTINUATION_MATERIAL_BYTES))
   if(verify)await lease.use(amk=>verify(v,amk,current))
   if(!current())fail('closed')
   const output=await transaction<T>(s,resolutionInventoryStores,'readonly',(tx,result,abort)=>scheduleResolutionInventory(tx,s,current,now=>{
    try{if(!current())fail('closed');if(!same(now,v))fail('stale_source');schedule(tx,v,current,result,abort)}catch(e){abort(e)}
   },abort))
   if(!current())fail('closed')
   return output
  }finally{activeAbort=undefined;unAuth();unKey()}
 }

 /** Separate exact existing-device join. It preserves F2's original transport
  * identity and is neither interval replay nor a new applied/prefix grant. */
 async joinRetainedF2(input:BrowserSyncScope,since:number,rawPage:string,eventId:string):Promise<RetainedF2JoinResult>{
  const s=snapshotBrowserSyncScope(input)
  if(!safe(since))fail()
  const page=validateBrowserInboundPage(rawPage,since,s),item=page.items.find(i=>i.event.event_id===eventId)
  const reference=item?.event as Partial<ResolutionReference>|undefined
  if(page.protocol_version!==3||!item||reference?.entity_type!=='note'||reference.operation!=='resolution')fail('conflicting_record')
  return this.inspect<RetainedF2JoinResult>(s,(_tx,_v,_current,result)=>result(Object.freeze({readOnly:true,matched:true,eventId,serverSequence:item.event.server_sequence!,receivedProtocol:3,retainedProtocol:2,commitAuthorized:false,ackAuthorized:false})),async(v,amk,current)=>{
   const rows=v.rows.map(r=>readFutureInboxRecord(r,s)).sort((a,b)=>a.server_sequence-b.server_sequence)
   if(rows.length!==v.cursor.pull_cursor||rows.some((r,i)=>r.server_sequence!==i+1))fail()
   const row=rows.find(r=>r.event_id===eventId)
   if(!row||row.state!=='note_resolution_proven'||row.protocol_version!==2||row.server_sequence!==item.event.server_sequence||!same(JSON.parse(row.wire_item),item))fail('conflicting_record')
   const history=await authenticateContinuationHistory(s,v,amk,current),root=history.resolutions.get(eventId)
   if(!root||root.record.protocol_version!==2||root.record.transport_kind!=='legacy_resolution_v2'||root.record.wire_item!==row.wire_item)fail('conflicting_record')
   await verifyContinuationSources(s,v,history,amk,current)
  })
 }

 /** Full zero-origin reconciliation. Older intervals are admitted only as part
  * of one complete ordered account history, never as independent replay pages.
  * This returns no owner, prefix, ACK or reader-readiness certificate. */
 async replayInboundHistory(input:BrowserSyncScope,rawPages:readonly string[]):Promise<V10PageReplayResult>{
  const s=snapshotBrowserSyncScope(input)
  if(!rawPages.length||rawPages.length>MAX_PLANNER_ROWS)fail('conflicting_record')
  let bytes=0
  for(const raw of rawPages){
   if(typeof raw!=='string'||raw.length>MAX_PLANNER_INVENTORY_BYTES)fail('conflicting_record')
   bytes+=new TextEncoder().encode(raw).length
   if(bytes>MAX_PLANNER_INVENTORY_BYTES)fail('conflicting_record')
  }
  let since=0
  const items:ReturnType<typeof validateBrowserInboundPage>['items']=[]
  const protocols:number[]=[]
  for(let i=0;i<rawPages.length;i++){
   const page=validateBrowserInboundPage(rawPages[i]!,since,s)
   if(!page.items.length||page.next_cursor<=since||page.has_more!==(i<rawPages.length-1))fail('conflicting_record')
   items.push(...page.items);protocols.push(...page.items.map(()=>page.protocol_version));since=page.next_cursor
   if(items.length>MAX_PLANNER_ROWS)fail('conflicting_record')
  }
  const cursor=since
  return this.inspect(s,(tx,v,current,result,abort)=>{
   if(!v.history.some(r=>(r as {record_kind:string}).record_kind==='resolution'))fail('conflicting_record')
   if(v.cursor.pull_cursor!==cursor||v.rows.length!==items.length)fail('stale_cursor')
   const store=tx.objectStore('inbox')
   const next=(index:number)=>{
    if(index===items.length){result({committed_cursor:cursor,new_events:0,replayed_events:items.length,has_more:false});return}
    const item=items[index]!,sequence=item.event.server_sequence!
    if(sequence!==index+1)fail('conflicting_record')
    const bySequence=store.get([s.accountId,sequence]),byEvent=store.index('event').get([s.accountId,item.event.event_id]),out=tx.objectStore('outbox').get([s.accountId,item.event.event_id]),receipt=tx.objectStore('receipts').get([s.accountId,item.event.event_id])
    let done=0
    const finish=()=>{if(++done!==4)return;try{
     if(!current())fail('closed')
     const row=readFutureInboxRecord(bySequence.result,s),indexed=readFutureInboxRecord(byEvent.result,s)
     if(row.server_sequence!==sequence||row.event_id!==item.event.event_id||row.protocol_version!==protocols[index]
       ||!same(row,indexed)||!same(JSON.parse(row.wire_item),item))fail('conflicting_record')
     if(out.result!==undefined){
      const outbound=validateBrowserStoredUploadPair(out.result,receipt.result,s),{device_id,server_sequence:_sequence,...descriptor}=item.event
      if(outbound.protocol_version!==protocols[index]||device_id!==outbound.device_id||!same(JSON.parse(outbound.raw_body).items[0],{event:descriptor,object:item.object})
        ||receipt.result!==undefined&&receipt.result.server_sequence!==sequence)fail('conflicting_record')
     }else if(receipt.result!==undefined)fail()
     next(index+1)
    }catch(e){abort(e)}}
    bySequence.onsuccess=finish;byEvent.onsuccess=finish;out.onsuccess=finish;receipt.onsuccess=finish
   }
   try{next(0)}catch(e){abort(e)}
  },async(v,amk,current)=>{
   const history=await authenticateContinuationHistory(s,v,amk,current)
   await verifyContinuationSources(s,v,history,amk,current)
  })
 }

 /** No arbitrary older-page replay after later pull advancement. No cursor put,
  * even on success: native readonly completion is the transport barrier. */
 async replayInboundPage(input:BrowserSyncScope,since:number,rawPage:string):Promise<V10PageReplayResult>{
  const s=snapshotBrowserSyncScope(input);if(!safe(since))fail()
  const page=validateBrowserInboundPage(rawPage,since,s)
  return this.inspect(s,(tx,v,current,result,abort)=>{
   if(v.cursor.pull_cursor!==page.next_cursor||v.cursor.pull_cursor<=since||!page.items.length)fail('stale_cursor')
   const store=tx.objectStore('inbox'),count=store.count(IDBKeyRange.bound([s.accountId,since],[s.accountId,page.next_cursor],true,false))
   count.onsuccess=()=>{try{
    if(!current())fail('closed');if(count.result!==page.items.length)fail('conflicting_record')
    const next=(index:number)=>{
     if(index===page.items.length){result({committed_cursor:page.next_cursor,new_events:0,replayed_events:page.items.length,has_more:page.has_more});return}
     const item=page.items[index]!,sequence=item.event.server_sequence!,event=item.event.event_id
     const bySequence=store.get([s.accountId,sequence]),byEvent=store.index('event').get([s.accountId,event]),out=tx.objectStore('outbox').get([s.accountId,event]),receipt=tx.objectStore('receipts').get([s.accountId,event])
     let done=0
     const finish=()=>{if(++done!==4)return;try{
      if(!current())fail('closed')
      const row=readFutureInboxRecord(bySequence.result,s),indexed=readFutureInboxRecord(byEvent.result,s)
      if(row.server_sequence!==sequence||row.event_id!==event||!same(row,indexed)||row.protocol_version!==page.protocol_version||identity(JSON.parse(row.wire_item))!==identity(item))fail('conflicting_record')
      if(out.result!==undefined){
       const outbound=validateBrowserStoredUploadPair(out.result,receipt.result,s),{device_id,server_sequence:_sequence,...descriptor}=item.event
       // Same immutable own-echo join as the old transport; accepted receipts
       // remain required when the stored outbound state requires one.
       if(outbound.protocol_version!==page.protocol_version||device_id!==outbound.device_id||identity(JSON.parse(outbound.raw_body).items[0])!==identity({event:descriptor,object:item.object})||receipt.result!==undefined&&receipt.result.server_sequence!==sequence)fail('conflicting_record')
      }else if(receipt.result!==undefined)fail()
      next(index+1)
     }catch(e){abort(e)}}
     bySequence.onsuccess=finish;byEvent.onsuccess=finish;out.onsuccess=finish;receipt.onsuccess=finish
    }
    next(0)
   }catch(e){abort(e)}}
  })
 }

 async listReceived(input:BrowserSyncScope,limit=200,afterServerSequence=0):Promise<BrowserReceivedInboxRecord[]>{
  const s=snapshotBrowserSyncScope(input)
  if(!Number.isSafeInteger(limit)||limit<1||limit>200||!safe(afterServerSequence))fail()
  return this.inspect(s,(tx,v,current,result,abort)=>{
   if(afterServerSequence===Number.MAX_SAFE_INTEGER){result([]);return}
   const store=tx.objectStore('inbox'),request=store.openCursor(IDBKeyRange.bound([s.accountId,afterServerSequence],[s.accountId,Number.MAX_SAFE_INTEGER],true,false)),records:BrowserReceivedInboxRecord[]=[]
   let previous=afterServerSequence
   request.onsuccess=()=>{try{
    if(!current())fail('closed')
    const cursor=request.result;if(!cursor){result(records);return}
    const row=readFutureInboxRecord(cursor.value,s)
    if(row.server_sequence<=previous||row.server_sequence>v.cursor.pull_cursor||!same(cursor.key,[s.accountId,row.server_sequence])||!same(cursor.primaryKey,cursor.key))fail()
    previous=row.server_sequence
    const indexed=store.index('event').get([s.accountId,row.event_id])
    indexed.onsuccess=()=>{try{
     if(!current())fail('closed')
     if(!same(readFutureInboxRecord(indexed.result,s),row))fail('conflicting_record')
     if(row.state==='received')records.push(row)
     if(records.length===limit)result(records);else cursor.continue()
    }catch(e){abort(e)}}
   }catch(e){abort(e)}}
  })
 }
}
