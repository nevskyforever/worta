import type {NormalUserAuthRuntime} from '@/auth/userAuth'
import type {AuthoritativeAccountBinding} from '@/auth/accountBinding'
import type {RuntimeKeyContext} from '@/auth/keyContext'
import type {ContentNoteEvent} from '@/cloud/contentNoteCodec'
import type {NoteSyncPlaintext} from '@/cloud/noteSyncCodec'
import {canonicalizeSyncTimestamp} from '@/cloud/syncTimestamp'
import {decodeBase64Url} from '@/api/base64url'
import {BrowserPersistenceError} from './persistence'
import {BrowserLocalAuthority} from './localAuthority'
import {currentBrowserDevice,type VerifiedBrowserDevice} from './deviceRegistration'
import {BrowserSyncDurability,snapshotBrowserSyncScope,type BrowserSyncScope,type BrowserSyncTransaction} from './syncDurability'
import {rejectLegacy3ResolutionRoots,readFutureInboxRecord,resolutionSame as same} from './resolutionRecords'
import {inspectResolutionDag} from './resolutionDag'
import {BrowserResolutionPlanner,ResolutionPlannerError,resolutionInventoryStores,scheduleResolutionInventory,type ResolutionInventorySnapshot} from './resolutionPlanner'
import {BrowserResolutionContinuation} from './resolutionContinuation'
import {BrowserResolutionPrefix,type ResolutionPrefixResult} from './resolutionPrefix'
import {BrowserNoteReaderReadiness,type BrowserNoteReadinessDiagnostic} from './noteReaderReadiness'
import {BrowserV10Transport} from './v10Transport'
import {BrowserLegacyNoteApply} from './legacyNoteApply'
import {BrowserContentNoteApply} from './contentNoteApply'
import {BrowserImportedMetadataApply} from './importedMetadataApply'
import {BrowserStageApply} from './stageApply'
import {BrowserStageOrderApply} from './stageOrderApply'
import {openBrowserCanonicalObject} from './canonicalReaders'
import {openNoteTransport} from './noteTransport'
import {bindMixedV10Recovery,bindPostFMetadataRecovery} from './v10MixedRecovery'

type Route='metadata'|'stage'|'stage_order'|'legacy'|'content'|'resolution'|'continuation'
export interface RecoveryBlocker {
 sequence:number;kind:'missing_sequence'|'dependency_unavailable'|'unsupported_route'|'unsupported_generation'|'unresolved_fork'|'unqualified_family'|'budget_exhausted'
 retryable:boolean
}
export interface V10RecoveryOptions {maxEvents?:number;replay?:{since:number;rawPage:string}}
export interface V10RecoveryResult {
 readonly ready:false;readonly declarationAllowed:false;readonly ackAuthorized:false
 applied:{sequence:number;route:Route;status:'applied'|'conflict_preserved'}[]
 blocker:RecoveryBlocker|null;more:boolean;replayed_events:number
 prefix:ResolutionPrefixResult|null;readiness:BrowserNoteReadinessDiagnostic|null
}
function fail(code:'closed'|'corrupt_record'|'stale_source'|'unsupported_local_source'='corrupt_record'):never{throw new BrowserPersistenceError(code)}
const hasResolution=(v:ResolutionInventorySnapshot)=>v.history.some(r=>(r as {record_kind:string}).record_kind==='resolution')
function gap(v:ResolutionInventorySnapshot,s:BrowserSyncScope):number|null {
 const rows=v.rows.map(r=>readFutureInboxRecord(r,s)).sort((a,b)=>a.server_sequence-b.server_sequence)
 let next=1
 for(const row of rows){if(row.server_sequence!==next)return next;next++}
 return next<=v.cursor.pull_cursor?next:null
}
/** Manual, isolated K factory. No production composition, background timer, pull,
 * ACK or declaration surface. Each mutation belongs to an accepted atomic owner. */
export class BrowserV10Recovery {
 constructor(private readonly transaction:BrowserSyncTransaction,private readonly local:BrowserLocalAuthority,private readonly profileCurrent:()=>boolean,
  private readonly ownerTransaction?:(sequence:number)=>Promise<BrowserSyncTransaction>){}
 /** Separate dormant manual slice; the historical bind below is unchanged. */
 bindMixedSources(auth:NormalUserAuthRuntime,bindings:AuthoritativeAccountBinding,keys:RuntimeKeyContext,device:VerifiedBrowserDevice){return bindMixedV10Recovery(this.transaction,this.local,this.profileCurrent,auth,bindings,keys,device)}
 /** Explicit bounded .18 route; historical K/G/Q bindings keep their contracts. */
 bindPostFMetadata(auth:NormalUserAuthRuntime,bindings:AuthoritativeAccountBinding,keys:RuntimeKeyContext,device:VerifiedBrowserDevice){return bindPostFMetadataRecovery(this.transaction,this.local,this.profileCurrent,auth,bindings,keys,device)}
 bind(auth:NormalUserAuthRuntime,bindings:AuthoritativeAccountBinding,keys:RuntimeKeyContext,device:VerifiedBrowserDevice){
  let operation:BrowserSyncTransaction|undefined
  const tx:BrowserSyncTransaction=(scope,stores,mode,schedule)=>(operation??this.transaction)(scope,stores,mode,schedule)
  const current=this.profileCurrent,local=this.ownerTransaction?new BrowserLocalAuthority(tx):this.local
  const transport=new BrowserV10Transport(tx,current).bind(auth,bindings,keys,device)
  const planner=new BrowserResolutionPlanner(tx,local,current).bind(auth,bindings,keys,device),resolution=planner.atomicApply()
  const continuation=new BrowserResolutionContinuation(tx,local,current).bind(auth,bindings,keys,device)
  const prefix=new BrowserResolutionPrefix(tx,current).bind(auth,bindings,keys,device)
  const readiness=new BrowserNoteReaderReadiness(tx,current).bind(auth,bindings,keys,device)
  const notes=new BrowserLegacyNoteApply(tx,local,current),legacy=notes.bind(auth,bindings,keys),content=new BrowserContentNoteApply(notes).bind(auth,bindings,keys)
  const metadata=new BrowserImportedMetadataApply(tx,new BrowserSyncDurability(tx)),stage=new BrowserStageApply(tx,current).bind(auth,bindings,keys),order=new BrowserStageOrderApply(tx,current).bind(auth,bindings,keys)
  let busy=false
  // A closure keeps the mutable pass guard and accepted component handles out of
  // public diagnostics. Results cannot be consumed as apply/prefix/ACK tokens.
  return Object.freeze({run:async(input:BrowserSyncScope,options:V10RecoveryOptions={}):Promise<V10RecoveryResult>=>{
   if(busy)fail('closed')
   const s=snapshotBrowserSyncScope(input),max=options.maxEvents??8,replay=options.replay?{...options.replay}:undefined
   if(!Number.isSafeInteger(max)||max<1||max>32)fail()
   busy=true
   try{
    const initial=auth.requireContext(),{context}=await bindings.ensureForCurrentUser(s.accountId),lease=keys.leaseForAccount(s.accountId)
    const live=()=>current()&&auth.isCurrent(initial)&&auth.isCurrent(context)&&!!lease?.isCurrent()&&currentBrowserDevice(device,s,auth)
    const check=()=>{if(!live())fail('closed')}
    if(!lease||lease.localAccountId!==s.accountId||lease.canonicalUserId!==s.canonicalUserId||lease.authEpoch!==context.authEpoch||context.userId!==s.canonicalUserId)fail('closed')
    check()
    const step=async<T>(p:Promise<T>)=>{const result=await p;check();return result}
    const load=()=>step(tx<ResolutionInventorySnapshot>(s,resolutionInventoryStores,'readonly',(tx,result,abort)=>scheduleResolutionInventory(tx,s,live,result,abort)))
    const result:V10RecoveryResult={ready:false,declarationAllowed:false,ackAuthorized:false,applied:[],blocker:null,more:false,replayed_events:0,prefix:null,readiness:null}
    if(replay)result.replayed_events=(await step(transport.replayInboundPage(s,replay.since,replay.rawPage))).replayed_events
    let v:ResolutionInventorySnapshot
    for(;;){
     // J authenticates the complete bounded future inventory and scans actual
     // inbox keys/index ownership. Always restart at zero; no caller offset can
     // hide an earlier pending event or persisted gap after a crash.
     const pending=await step(transport.listReceived(s,200)),loaded=await load();v=loaded
     rejectLegacy3ResolutionRoots(v.history)
     inspectResolutionDag(s,v.history) // Unknown/dangling future history never becomes an idle success.
     const rows=v.rows.map(r=>readFutureInboxRecord(r,s)),missing=gap(v,s),row=pending[0]
     if(missing!==null&&missing<=v.cursor.proven_prefix)fail()
     if(missing!==null&&(!row||missing<row.server_sequence)){result.blocker={sequence:missing,kind:'missing_sequence',retryable:false};break}
     if(row&&!same(rows.find(r=>r.server_sequence===row.server_sequence),row))fail('stale_source')
     if(hasResolution(v)){
      const certified=await step(prefix.prepare(s)),calculated=await step(prefix.calculate(certified)),n=calculated.first_blocking_sequence
      const barrier=n===null?undefined:rows.find(r=>r.server_sequence===n)
      if(barrier&&barrier.state!=='received'){
       result.blocker={sequence:n!,kind:['note_conflict_preserved','content_note_conflict_preserved'].includes(barrier.state)?'unresolved_fork':'unqualified_family',retryable:false};break
      }
     }
     if(!row)break
     if(result.applied.length===max){result.blocker={sequence:row.server_sequence,kind:'budget_exhausted',retryable:true};break}
     const block=(kind:RecoveryBlocker['kind'],retryable=false)=>{result.blocker={sequence:row.server_sequence,kind,retryable}}
     const wire=JSON.parse(row.wire_item),e=wire.event
     if(e.device_id===s.deviceId){block('unsupported_route');break} // No own author/self-echo activation.
     let route:Route
     if(e.entity_type==='note'){
      const classified=await step(lease.use(async amk=>{
       if(e.operation==='resolution'){if(row.protocol_version!==2)return null;return'resolution' as const}
       if(e.operation!=='event')return(await openNoteTransport(s,amk,row.protocol_version,row.wire_item,'legacy')).plaintext
       if(row.protocol_version!==3)fail()
       const nonce=decodeBase64Url(wire.object.nonce),ciphertext=decodeBase64Url(wire.object.ciphertext)
       try{
        const c=await openBrowserCanonicalObject(amk,{accountId:s.canonicalUserId,projectId:e.project_id,entityId:e.entity_id,entityType:'note',eventId:e.event_id},{...wire.object,nonce,ciphertext}) as ContentNoteEvent
        const h=c.event.header
        if(c.device_id!==e.device_id||h.revision!==e.revision||canonicalizeSyncTimestamp(h.updated_at)!==canonicalizeSyncTimestamp(e.updated_at))fail()
        return c.event.version===2?'resolution' as const:c.event
       }finally{nonce.fill(0);ciphertext.fill(0)}
      }))
      if(classified===null){block('unsupported_route');break}
      if(classified==='resolution'){
       if(hasResolution(v)){block('unsupported_generation');break} // Frozen F is pre-v10 only.
       route='resolution'
      }else{
       const h=(classified as NoteSyncPlaintext).header
       if(hasResolution(v)){
        const head=v.history.find(raw=>{const r=raw as {record_kind:string;record_version:number;project_id:string;note_id:string};return r.record_kind==='head'&&r.record_version===2&&r.project_id===h.project_id&&r.note_id===h.entity_id})
        if(!head||h.parent_event_id===null){block('unsupported_route');break}
        route='continuation'
       }else route=e.operation==='event'?'content':'legacy'
      }
     }else{
      if(hasResolution(v)){block('unsupported_route');break}
      if(e.entity_type==='project_metadata')route='metadata'
      else if(e.entity_type==='stage')route='stage'
      else if(e.entity_type==='stage_order')route='stage_order'
      else{block('unsupported_route');break}
     }
     // Prepare under the owner's real auth/AEAD/source rules, then pin K's exact
     // routing inventory before its atomic commit. No filtered v9 views or DTO
     // authority. A pass is resumable, NOT one giant multi-event transaction.
     const commit=async<T,R>(prepare:Promise<T>,apply:(token:T)=>Promise<R>)=>{
      const token=await step(prepare);if(!same(await load(),v))fail('stale_source');return step(apply(token))
     }
     try{
      if(this.ownerTransaction)operation=await step(this.ownerTransaction(row.server_sequence))
      let status:'applied'|'conflict_preserved'='applied'
      if(route==='resolution'){
       const p=await step(planner.prepare(s,row.server_sequence));await commit(resolution.prepare(p.token),t=>resolution.commit(t))
      }else if(route==='continuation')status=await commit(continuation.prepare(s,row.server_sequence),t=>continuation.commit(t))
      else if(route==='legacy'||route==='content'){
       const reader=route==='legacy'?legacy:content,value=await commit(reader.prepare(s,row.server_sequence),t=>reader.commit(t))
       if(value==='conflict_preserved')status=value
      }else if(route==='metadata')await commit(metadata.prepareVerified(s,e,auth,bindings,keys),t=>metadata.commitVerifiedImportedMetadataApply(s,t))
      else if(route==='stage')await commit(stage.prepare(s,row.server_sequence),t=>stage.commit(t))
      else await commit(order.prepare(s,row.server_sequence),t=>order.commit(t))
      result.applied.push({sequence:row.server_sequence,route,status})
      if(route==='continuation'&&status==='conflict_preserved'){block('unresolved_fork');break}
     }catch(error){
      check()
      if(error instanceof ResolutionPlannerError&&['missing_parent','missing_publication','missing_bootstrap','dependency_unavailable','missing_conflict'].includes(error.code)||error instanceof BrowserPersistenceError&&['note_orphan','missing_history','ineligible_lineage','stale_authority','stage_dependency_missing'].includes(error.code)){
       block('dependency_unavailable',true);break
      }
      if(error instanceof BrowserPersistenceError&&['unsupported_local_source','note_conflict_unproven','local_incompatible'].includes(error.code)||error instanceof ResolutionPlannerError&&['source_incompatible','clone_collision'].includes(error.code)){
       block('unsupported_route');break
      }
      throw error // Corruption, crypto failure, native failure and revocation never become retry-success.
     }finally{operation=undefined}
    }
    v=await load()
    if(hasResolution(v)){
     const token=await step(prefix.prepare(s));result.prefix=await step(prefix.calculate(token))
     if(result.prefix.candidate_proven_prefix>result.prefix.current_proven_prefix)await step(prefix.commit(await step(prefix.prepareCommit(token))))
     result.readiness=await step(readiness.inspect(s)) // I remains negative-only.
     if(!result.blocker&&result.prefix.first_blocking_sequence!==null)result.blocker={sequence:result.prefix.first_blocking_sequence,kind:'unqualified_family',retryable:false}
    }else if(!result.blocker){
     const conflict=v.rows.map(r=>readFutureInboxRecord(r,s)).sort((a,b)=>a.server_sequence-b.server_sequence).find(r=>['note_conflict_preserved','content_note_conflict_preserved'].includes(r.state))
     if(conflict)result.blocker={sequence:conflict.server_sequence,kind:'unresolved_fork',retryable:false}
    }
    result.more=(await step(transport.listReceived(s,1))).length>0
    if(replay)await step(transport.replayInboundPage(s,replay.since,replay.rawPage))
    return result
   }finally{busy=false}
  }})
 }
}
