import type { AccountMasterKey } from '@/crypto'
import type { GameEvent, ProjectGameEvent } from '@/cloud/gameCodec'
import { canonical } from '@/cloud/projectMetadataCodec'
import { progressMicros } from '@/cloud/progressCodec'
import { gameOwner, transitionGame, gameActionId, type GameState } from '@/cloud/gameProjectTransition'
import { decodeBase64Url } from '@/api/base64url'
import { canonicalizeSyncTimestamp } from '@/cloud/syncTimestamp'
import { openBrowserCanonicalObject } from './canonicalReaders'
import { prepareMetadataPrefix, verifiedMetadataSequences } from './metadataPrefixProof'
import { validateBrowserMetadataHead } from './metadataApplyRecords'
import { validateBrowserProjectBinding } from './projectBindings'
import { requireContentStageDependency } from './contentStageDependency'
import { authenticateReplayProgress, progressPreparationInventory } from './progressApply'
import { prepareProgressEvent, inspectPreparedProgressEvent } from './progressPrepare'
import { BrowserPersistenceError } from './persistence'
import { immutableWireIdentity, type BrowserSyncScope } from './syncDurability'
import type { ReplaySnapshot } from './replayInventory'
export interface GameInput { server_sequence:number; wire_item:string }
interface GamePlan { event:GameEvent; projection:GameState; serverSequence:number; wireItem:string }
export interface PreparedGameEvent { readonly gameToken: unique symbol }
const prepared=new WeakMap<object,{scope:string;snapshot:string;current:()=>boolean;data:GamePlan}>()
function reject():never{throw new BrowserPersistenceError('unsupported_local_source')}
/** Native Game owner admission over the exact replay snapshot. Only complete
 * linear project genesis/writing/completion and account genesis/reward/inventory. */
export async function prepareGameEvent(scope:BrowserSyncScope,input:GameInput,snapshot:ReplaySnapshot,amk:AccountMasterKey,current:()=>boolean,cache=new Map<number,PreparedGameEvent>(),active=new Set<number>(),atomic:ReadonlySet<number>=new Set()):Promise<PreparedGameEvent>{
 if(!current())throw new BrowserPersistenceError('closed')
 const cached=cache.get(input.server_sequence)
 if(cached){if(inspectPreparedGameEvent(cached,scope,snapshot).wireItem!==input.wire_item)reject();return cached}
 if(active.has(input.server_sequence))reject()
 active.add(input.server_sequence)
 try {
 const identity=immutableWireIdentity(snapshot),scopeId=immutableWireIdentity(scope),wire=JSON.parse(input.wire_item),d=wire.event
 const account=d.entity_type==='account_game'
 if(!Number.isSafeInteger(input.server_sequence)||input.server_sequence<1||d.server_sequence!==input.server_sequence||d.device_id===scope.deviceId
   ||d.deleted_at!==null||d.operation!==(account?'upsert':'event')||!['account_game','project_game'].includes(d.entity_type))reject()
 const nonce=decodeBase64Url(wire.object.nonce),ciphertext=decodeBase64Url(wire.object.ciphertext)
 let event:GameEvent
 try{event=await openBrowserCanonicalObject(amk,{accountId:scope.canonicalUserId,projectId:account?null:d.project_id,entityId:d.entity_id,entityType:d.entity_type,eventId:d.event_id},{...wire.object,nonce,ciphertext}) as GameEvent}
 finally{nonce.fill(0);ciphertext.fill(0)}
 const h=event.header,a=event.action,key=gameOwner(event)
 if(h.account_id!==scope.canonicalUserId||h.device_id!==d.device_id||h.event_id!==d.event_id||h.entity_id!==d.entity_id||h.revision!==d.revision
   ||h.updated_at!==canonicalizeSyncTimestamp(d.updated_at)||(h.scope==='account')!==account||h.scope==='project'&&h.project_id!==d.project_id)reject()
 if(h.scope==='project'){
  if(!['genesis','writing','completion'].includes(a.kind))reject()
  const inventory=progressPreparationInventory(snapshot),bindings=inventory.bindings.map(b=>validateBrowserProjectBinding(b,scope)),binding=bindings.find(b=>b.project_id===h.project_id)
  if(!binding||binding.mode!=='import_remote'||binding.phase!=='captured'||binding.remote_state!=='active'||binding.bootstrap_id!==h.bootstrap_id)reject()
  const structural={...inventory,rows:inventory.rows.filter(r=>['project_metadata','stage','stage_order'].includes(JSON.parse((r as {wire_item:string}).wire_item).event.entity_type))}
  const certificate=await prepareMetadataPrefix(scope,structural,amk,current,true),safe=verifiedMetadataSequences(certificate,scope,structural)
  const raw=inventory.owners.find(o=>(o as {owner_kind:string;owner_id:string}).owner_kind==='project_metadata'&&(o as {owner_id:string}).owner_id===h.project_id)
  if(!raw)reject()
  const head=validateBrowserMetadataHead(raw,scope),reference=head.history.find(p=>p.event_id===h.metadata_event_id)
  if(!reference||reference.server_sequence>=input.server_sequence||!safe.has(reference.server_sequence)||reference.operation==='delete'||head.bootstrap_id!==h.bootstrap_id
    ||head.tip_event_ids.length!==1||head.history.find(p=>p.event_id===head.tip_event_ids[0])?.operation==='delete')reject()
  await requireContentStageDependency(scope,input,event as ProjectGameEvent,structural,amk,current,certificate)
  if(a.kind==='writing'||a.kind==='completion'){
   await authenticateReplayProgress(scope,snapshot,amk,current)
   const source=snapshot.rows.find(r=>r.event_id===a.progress_event_id&&r.state==='progress_proven'&&r.server_sequence<input.server_sequence)
   if(!source)reject()
   const plan=inspectPreparedProgressEvent(await prepareProgressEvent(scope,source,inventory,amk,current),scope,inventory)
   if(plan.event.header.project_id!==h.project_id||plan.event.header.stage_id!==h.stage_id||plan.event.header.entity_id!==a.progress_entity_id)reject()
   if(a.kind==='writing'){
    if(plan.event.header.operation!=='append'||canonical(plan.event.entries)!==canonical([a.fact])||!plan.projection.entries.some(f=>canonical(f)===canonical(a.fact))
      ||h.event_id!==await gameActionId(h.account_id,'writing',[h.project_id,h.stage_id,a.progress_event_id,a.fact.entry_id]))reject()
   }else{
    if(progressMicros(plan.projection.entries.at(-1)?.new_total??plan.projection.base_total)!==progressMicros(a.total_symbols)
      ||h.event_id!==await gameActionId(h.account_id,'completion',[h.project_id,h.stage_id,a.completion_id]))reject()
    const referenceId=h.stage_id===null?h.metadata_event_id:h.stage_event_ids.length===1?h.stage_event_ids[0]:undefined
    const row=snapshot.rows.find(r=>r.event_id===referenceId)
    if(!row)reject()
    const w=JSON.parse(row.wire_item),n=decodeBase64Url(w.object.nonce),c=decodeBase64Url(w.object.ciphertext)
    try{const p=await openBrowserCanonicalObject(amk,{accountId:scope.canonicalUserId,projectId:h.project_id,entityId:w.event.entity_id,entityType:w.event.entity_type,eventId:w.event.event_id},{...w.object,nonce:n,ciphertext:c}) as {metadata?:{status:string};stage?:{status:string}}
      if((h.stage_id===null?p.metadata?.status:p.stage?.status)!=='завершен')reject()
    }finally{n.fill(0);c.fill(0)}
   }
  }
 }else if(!['genesis','reward','inventory','global_streak','freeze'].includes(a.kind))reject()
 let previous:GamePlan|undefined,source:ProjectGameEvent|undefined
 // Filter by durable owner key only to select candidates. Each candidate is
 // independently decrypted and its actual owner identity checked below.
 for(const row of snapshot.rows){if(row.server_sequence>=input.server_sequence)break
  if(active.has(row.server_sequence))continue
  if(row.state!=='game_proven'||!snapshot.games?.ledger.some(value=>{const l=value as {event_id:string;entity_id:string};return l.event_id===row.event_id&&l.entity_id===key}))continue
  const plan=inspectPreparedGameEvent(await prepareGameEvent(scope,row,snapshot,amk,current,cache,active,atomic),scope,snapshot)
  if(gameOwner(plan.event)!==key)reject()
  previous=plan
 }
 if(a.kind==='genesis'){if(previous||h.revision!==1||h.parents.length)reject()}
 else {
  if(h.parents.length!==1)reject()
  // Native account actions can be delivered before their causal parent. A
  // parent is valid when already proven or part of the same atomic Game window.
  const parent=snapshot.rows.find(row=>row.event_id===h.parents[0]&&(row.server_sequence<input.server_sequence||row.state==='game_proven'||atomic.has(row.server_sequence)))
  if(!parent||!(parent.state==='game_proven'||parent.state==='received'&&atomic.has(parent.server_sequence)))reject()
  const plan=inspectPreparedGameEvent(await prepareGameEvent(scope,parent,snapshot,amk,current,cache,active,atomic),scope,snapshot)
  if(gameOwner(plan.event)!==key||h.revision!==plan.event.header.revision+1)reject()
  previous=plan
 }
 if(a.kind==='reward'){
  // Native permits account reward to precede project source on the wire. A
  // received source is usable ONLY as a prepared member of this same atomic
  // Game commit. Its genuine ledger/proof is written before the reward.
  const row=snapshot.rows.find(row=>row.event_id===a.project_action_id&&(row.state==='game_proven'||row.state==='received'&&atomic.has(row.server_sequence)))
  if(!row||JSON.parse(row.wire_item).event.entity_type!=='project_game')reject()
  const plan=inspectPreparedGameEvent(await prepareGameEvent(scope,row,snapshot,amk,current,cache,active,atomic),scope,snapshot)
  if(plan.event.header.scope!=='project'||h.event_id!==await gameActionId(h.account_id,'reward',a.project_action_id))reject()
  source=plan.event as ProjectGameEvent
 }
 if(a.kind==='global_streak'){
  if(event.version===2){
   if(h.event_id!==await gameActionId(h.account_id,'global_streak_loss',a.before))reject()
  }else if(event.version===1){
   const refs=a.project_action_ids
   if(refs.length!==1)reject()
   const row=snapshot.rows.find(row=>row.event_id===refs[0]&&(row.state==='game_proven'||row.state==='received'&&atomic.has(row.server_sequence)))
   if(!row||JSON.parse(row.wire_item).event.entity_type!=='project_game')reject()
   const plan=inspectPreparedGameEvent(await prepareGameEvent(scope,row,snapshot,amk,current,cache,active,atomic),scope,snapshot)
   if(plan.event.header.scope!=='project'||plan.event.action.kind!=='writing'||plan.event.action.fact.writing_day!==a.writing_day
     ||h.event_id!==await gameActionId(h.account_id,'global_streak',[a.writing_day,refs]))reject()
   source=plan.event as ProjectGameEvent
  }else reject()
 }
 if (a.kind === 'freeze' && 'project_id' in a) {
  if(a.project_id!==null||a.project_action_id!==null||a.before===null||a.after===null
    ||h.event_id!==await gameActionId(h.account_id,'global_freeze',[a.writing_day,a.before,a.before_count]))reject()
 }
 const projection=transitionGame(event,previous?.projection,source)
 if(!current())throw new BrowserPersistenceError('closed')
 if(identity!==immutableWireIdentity(snapshot))throw new BrowserPersistenceError('stale_source')
 const token=Object.freeze({}) as PreparedGameEvent
 prepared.set(token,{scope:scopeId,snapshot:identity,current,data:{event:structuredClone(event),projection,wireItem:input.wire_item,serverSequence:input.server_sequence}});cache.set(input.server_sequence,token);return token
 } finally {active.delete(input.server_sequence)}
}
export function inspectPreparedGameEvent(token:PreparedGameEvent,scope:BrowserSyncScope,snapshot:ReplaySnapshot):GamePlan{
 const plan=prepared.get(token)
 if(!plan||!plan.current())throw new BrowserPersistenceError('closed')
 if(plan.scope!==immutableWireIdentity(scope)||plan.snapshot!==immutableWireIdentity(snapshot))throw new BrowserPersistenceError('stale_source')
 return structuredClone(plan.data)
}

/** Prepare one contiguous received Game window in actual dependency order,
 * using the same owner plans, not provisional proofs or cursor assignments. */
export async function prepareGameBatch(scope:BrowserSyncScope,sequence:number,snapshot:ReplaySnapshot,amk:AccountMasterKey,current:()=>boolean){
 if(!snapshot.manifest||sequence!==snapshot.manifest.applied_cursor!+1)reject()
 const atomic=new Set<number>(),cache=new Map<number,PreparedGameEvent>(),active=new Set<number>()
 for(const row of snapshot.rows.filter(row=>row.server_sequence>=sequence)){
  if(row.state!=='received'||!['project_game','account_game'].includes(JSON.parse(row.wire_item).event.entity_type))break
  atomic.add(row.server_sequence)
 }
 if(!atomic.size)reject()
 for(const row of snapshot.rows)if(atomic.has(row.server_sequence))await prepareGameEvent(scope,row,snapshot,amk,current,cache,active,atomic)
 return [...cache.entries()].filter(([n])=>atomic.has(n)).map(([,token])=>token)
}
