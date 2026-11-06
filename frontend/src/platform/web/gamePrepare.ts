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
 * linear project genesis/writing/completion and required account genesis/reward. */
export async function prepareGameEvent(scope:BrowserSyncScope,input:GameInput,snapshot:ReplaySnapshot,amk:AccountMasterKey,current:()=>boolean,cache=new Map<number,PreparedGameEvent>()):Promise<PreparedGameEvent>{
 if(!current())throw new BrowserPersistenceError('closed')
 const cached=cache.get(input.server_sequence)
 if(cached){if(inspectPreparedGameEvent(cached,scope,snapshot).wireItem!==input.wire_item)reject();return cached}
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
 }else if(!['genesis','reward'].includes(a.kind))reject()
 let previous:GamePlan|undefined,source:ProjectGameEvent|undefined
 for(const row of snapshot.rows){if(row.server_sequence>=input.server_sequence)break
  if(row.state!=='game_proven')continue
  const plan=inspectPreparedGameEvent(await prepareGameEvent(scope,row,snapshot,amk,current,cache),scope,snapshot)
  if(gameOwner(plan.event)===key)previous=plan
  if(a.kind==='reward'&&plan.event.header.event_id===a.project_action_id&&plan.event.header.scope==='project')source=plan.event as ProjectGameEvent
 }
 if(a.kind==='genesis'){if(previous||h.revision!==1||h.parents.length)reject()}
 else if(!previous||h.parents.length!==1||h.parents[0]!==previous.event.header.event_id||h.revision!==previous.event.header.revision+1)reject()
 if(a.kind==='reward'&&(!source||h.event_id!==await gameActionId(h.account_id,'reward',a.project_action_id)))reject()
 const projection=transitionGame(event,previous?.projection,source)
 if(!current())throw new BrowserPersistenceError('closed')
 if(identity!==immutableWireIdentity(snapshot))throw new BrowserPersistenceError('stale_source')
 const token=Object.freeze({}) as PreparedGameEvent
 prepared.set(token,{scope:scopeId,snapshot:identity,current,data:{event:structuredClone(event),projection,wireItem:input.wire_item,serverSequence:input.server_sequence}});cache.set(input.server_sequence,token);return token
}
export function inspectPreparedGameEvent(token:PreparedGameEvent,scope:BrowserSyncScope,snapshot:ReplaySnapshot):GamePlan{
 const plan=prepared.get(token)
 if(!plan||!plan.current())throw new BrowserPersistenceError('closed')
 if(plan.scope!==immutableWireIdentity(scope)||plan.snapshot!==immutableWireIdentity(snapshot))throw new BrowserPersistenceError('stale_source')
 return structuredClone(plan.data)
}
