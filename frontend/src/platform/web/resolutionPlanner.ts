import { authenticateContinuationHistory, verifyContinuationSources, continuationDependencyView } from './resolutionHistorical'
import type {NormalUserAuthRuntime} from '@/auth/userAuth'
import type {AuthoritativeAccountBinding} from '@/auth/accountBinding'
import type {AuthoritativeKeyContextLease,RuntimeKeyContext} from '@/auth/keyContext'
import {decryptObjectBytes,encryptObjectBytes,type AccountMasterKey} from '@/crypto'
import {decodeBase64Url,encodeBase64Url} from '@/api/base64url'
import {unframeContentNote,type ContentNoteEvent} from '@/cloud/contentNoteCodec'
import {decodeNoteSyncResolutionV2,type NoteSyncResolutionV2} from '@/cloud/noteSyncResolutionV2Codec'
import {canonicalizeSyncTimestamp} from '@/cloud/syncTimestamp'
import type {NoteSyncPlaintext,NoteSyncRecord,NoteSyncTombstone} from '@/cloud/noteSyncCodec'
import {BrowserPersistenceError} from './persistence'
import {currentBrowserDevice,type VerifiedBrowserDevice} from './deviceRegistration'
import {BrowserLocalAuthority,type BrowserLocalAuthorityRuntime,type LocalSourceToken,scheduleProtectedResolutionApply,type ProtectedResolutionSourcePlan} from './localAuthority'
import {assertContentNoteRoute,validateContentLocalNote,validateLocalNote,MAX_LOCAL_SOURCE_BYTES,MAX_LOCAL_NOTES,localCanonicalBytes} from './localAuthorityRecords'
import {snapshotBrowserSyncScope,validateBrowserInboxRecord,validateBrowserStoredUploadPair,validateBrowserSyncCursor,immutableWireIdentity,type BrowserSyncScope,type BrowserSyncTransaction,type BrowserInboxRecord} from './syncDurability'
import {validateBrowserProjectBinding} from './projectBindings'
import {validateNoteHistorySet,type BrowserNoteCandidate,type BrowserNoteConflictGroup,type BrowserNoteHead} from './noteHistoryRecords'
import {validateNoteIntentRecord} from './noteIntentRecords'
import {validateBrowserV9State} from './indexedDbV9Migration'
import {inspectResolutionDag} from './resolutionDag'
import {resolutionTransportKind,type ResolutionTransportKind,boundResolutionJson,resolutionIdentity,MAX_RESOLUTION_EVIDENCE_BYTES,readFutureInboxRecord,type BrowserResolution4,type BrowserResolutionHead2,type BrowserGroupTransition2,type BrowserResolutionInbox10,type BrowserResolutionProof1} from './resolutionRecords'
import {authenticateResolutionHistory,resolutionBytesDigest} from './resolutionAuthentication'
import {canonical} from '@/cloud/projectMetadataCodec'
import {noteProofDigest} from './legacyNoteApply'
import type {LocalProjectRecord,LocalNoteRecord} from './localAuthorityRecords'
import {openHistoryCandidate} from './noteHistory'
import {openNoteTransport} from './noteTransport'
import {verifyContentGroup} from './contentNoteConflictProof'
import {verifyStoredContentNoteProof} from './contentNoteProof'
import {openFrozenLocalVersion} from './noteUnsealedVersion'
import {verifyStoredNoteProof} from './legacyNoteApply'
import {authenticateContentAuthorityInventory} from './contentAuthorityInventory'
import {prepareMetadataPrefix,verifiedMetadataSequences} from './metadataPrefixProof'
import {validateBrowserMetadataHead} from './metadataApplyRecords'
import {prepareStagePrefix,stageSequences} from './stageApply'
import {validateStageHead} from './stageRecords'
import {proveStageReferenceCoverage} from './stageOrderRecords'
import type {BrowserPrefixInventory} from './provenPrefixRecords'

export const MAX_PLANNER_ROWS=16384
export const MAX_PLANNER_INVENTORY_BYTES=32*1024*1024
export type ResolutionPlannerCode='missing_parent'|'missing_publication'|'missing_bootstrap'|'dependency_unavailable'|'missing_conflict'|'stale_inventory'|'invalid_resolution'|'source_incompatible'|'resource_limit'|'lifecycle_invalid'|'clone_collision'
export class ResolutionPlannerError extends Error {
 readonly name='ResolutionPlannerError'
 constructor(readonly code:ResolutionPlannerCode){super(`resolution_planner_${code}`)}
}
function fail(code:ResolutionPlannerCode):never{throw new ResolutionPlannerError(code)}
const same=(a:unknown,b:unknown)=>immutableWireIdentity(a)===immutableWireIdentity(b)
export const resolutionInventoryStores=['inbox','outbox','receipts','projectBindings','ownerHeads','legacyNoteHistory','legacyNoteIntents','localProjects','localNotes','cursors']
export interface ResolutionInventorySnapshot extends BrowserPrefixInventory {projects:unknown[];localNotes:unknown[];intents:unknown[];checkpoints:unknown[]}
const stores=resolutionInventoryStores
type Inventory=ResolutionInventorySnapshot
export interface ResolutionPlannerToken {readonly resolutionPlannerToken:unique symbol}
export interface ResolutionPlanStatus {readonly readOnly:true;readonly commitAuthorized:false;readonly ackAuthorized:false;readonly parentCount:number;readonly strategy:NoteSyncResolutionV2['resolution']['strategy']}
interface Plan {successor:boolean;owner:BrowserResolutionPlannerRuntime;scope:BrowserSyncScope;sequence:number;inventory:Inventory;source:LocalSourceToken;current:()=>boolean;resolution:NoteSyncResolutionV2;transportKind:ResolutionTransportKind;group:BrowserNoteConflictGroup;parents:Map<string,NoteSyncPlaintext>;status:ResolutionPlanStatus;lease:AuthoritativeKeyContextLease}
const plans=new WeakMap<object,Plan>()
const wireNote=(n:NoteSyncRecord|NoteSyncTombstone|Record<string,unknown>)=>{const v={...n};delete(v as Record<string,unknown>).revision;return v}
const route=(n:NoteSyncRecord|NoteSyncTombstone)=>[n.project_id,n.stage_id,n.source_type,n.source_map_id,n.source_node_id,n.content_format]

export function scheduleResolutionInventory(tx:IDBTransaction,s:BrowserSyncScope,current:()=>boolean,result:(v:Inventory)=>void,abort:(e:unknown)=>void):void {
   const range=IDBKeyRange.bound([s.accountId],[s.accountId,[]]),arrays:unknown[][]=stores.map(()=>[])
   let completed=0,left=MAX_PLANNER_INVENTORY_BYTES-2*stores.length
   stores.forEach((name,index)=>{
    const request=tx.objectStore(name).openCursor(range)
    request.onsuccess=()=>{try{
     if(!current())fail('lifecycle_invalid')
     const cursor=request.result
     if(cursor){
      if(arrays[index]!.length>=MAX_PLANNER_ROWS)fail('resource_limit')
      const value:unknown=cursor.value
      // Admit one record before retaining it; do not materialize every account
      // store with getAll and discover the aggregate limit only afterwards.
      try{boundResolutionJson(value,left)}catch{fail('resource_limit')}
      const bytes=new TextEncoder().encode(JSON.stringify(value))
      try{left-=bytes.length+1;if(left<0)fail('resource_limit')}finally{bytes.fill(0)}
      arrays[index]!.push(value);cursor.continue();return
     }
     if(++completed!==stores.length)return
     const [rows,out,receipts,bindings,owners,history,intents,projects,localNotes,checkpoints]=arrays as [unknown[],unknown[],unknown[],unknown[],unknown[],unknown[],unknown[],unknown[],unknown[],unknown[]]
     if(checkpoints.length!==1)fail('invalid_resolution')
     result({rows,out,receipts,bindings,owners,history,intents,projects,localNotes,checkpoints,cursor:validateBrowserSyncCursor(checkpoints[0],s)})
    }catch(e){abort(e)}}
   })
}

/** Dormant internal entry point. Never composed into BrowserPersistence/editor/scheduler. */
export class BrowserResolutionPlanner {
 constructor(private readonly transaction:BrowserSyncTransaction,private readonly local:BrowserLocalAuthority,private readonly profileCurrent:()=>boolean){}
 bind(auth:NormalUserAuthRuntime,bindings:AuthoritativeAccountBinding,keys:RuntimeKeyContext,device:VerifiedBrowserDevice){return new BrowserResolutionPlannerRuntime(this.transaction,this.local.bind(auth,bindings,keys),this.profileCurrent,auth,bindings,keys,device)}
}
export class BrowserResolutionPlannerRuntime {
 constructor(private readonly transaction:BrowserSyncTransaction,private readonly local:BrowserLocalAuthorityRuntime,private readonly profileCurrent:()=>boolean,private readonly auth:NormalUserAuthRuntime,private readonly bindings:AuthoritativeAccountBinding,private readonly keys:RuntimeKeyContext,private readonly device:VerifiedBrowserDevice){}
 private async guard(s:BrowserSyncScope){
  const initial=this.auth.requireContext()
  if(!this.profileCurrent()||!currentBrowserDevice(this.device,s,this.auth))fail('lifecycle_invalid')
  const {context}=await this.bindings.ensureForCurrentUser(s.accountId),lease=this.keys.leaseForAccount(s.accountId)
  const current=()=>this.profileCurrent()&&this.auth.isCurrent(initial)&&this.auth.isCurrent(context)&&!!lease?.isCurrent()&&currentBrowserDevice(this.device,s,this.auth)
  if(!lease||context.userId!==s.canonicalUserId||lease.localAccountId!==s.accountId||lease.canonicalUserId!==s.canonicalUserId||lease.authEpoch!==context.authEpoch||!current())fail('lifecycle_invalid')
  return{lease,current}
 }
 private load(s:BrowserSyncScope,current:()=>boolean):Promise<Inventory>{
  return this.transaction(s,stores,'readonly',(tx,result,abort)=>scheduleResolutionInventory(tx,s,current,result,abort))
 }
 private async original(s:BrowserSyncScope,row:BrowserInboxRecord,amk:AccountMasterKey,check:()=>void){
  if(row.state!=='received')fail('invalid_resolution')
  const wire=JSON.parse(row.wire_item),e=wire.event
  if(e.entity_type!=='note'||e.deleted_at!==null||e.device_id===s.deviceId||!((row.protocol_version===2||row.protocol_version===3)&&e.operation==='resolution'||row.protocol_version===3&&e.operation==='event')||wire.object.crypto_version!==1||wire.object.aad_version!==1)fail('invalid_resolution')
  const nonce=decodeBase64Url(wire.object.nonce),ciphertext=decodeBase64Url(wire.object.ciphertext);let bytes:Uint8Array|undefined
  try{
   bytes=await decryptObjectBytes(amk,{userId:s.canonicalUserId,projectId:e.project_id,entityId:e.entity_id,entityType:'note'},{...wire.object,nonce,ciphertext});check()
   const transportKind=resolutionTransportKind(row.protocol_version,e.operation)
   let resolution:NoteSyncResolutionV2,content:ContentNoteEvent|undefined
   if(transportKind==='content_note_resolution_v2'){content=unframeContentNote(bytes);if(content.event.version!==2||content.account_id!==s.canonicalUserId||content.device_id!==e.device_id)fail('invalid_resolution');resolution=content.event}
   else resolution=decodeNoteSyncResolutionV2(bytes)
   const h=resolution.header
   if(h.event_id!==row.event_id||h.event_id!==e.event_id||h.project_id!==e.project_id||h.entity_id!==e.entity_id||h.revision!==e.revision||h.updated_at!==canonicalizeSyncTimestamp(e.updated_at))fail('invalid_resolution')
   const outputs=[resolution.result.note,...(resolution.resolution.strategy==='keep_both'?[resolution.resolution.retained_note]:[])]
   for(const n of outputs){
    assertContentNoteRoute(n)
    if('content'in n){let title=0,body=0;for(const _ of n.title)if(++title>500)fail('resource_limit');for(const _ of n.content)if(++body>300000)fail('resource_limit')}
    if(transportKind==='legacy_resolution_v2'&&(n.stage_id!==null||n.content_format!=='html'))fail('source_incompatible')
    const source='deleted_at'in n?{state:'tombstone' as const,note:n,revision:0}:{state:'live' as const,note:{...n,revision:0}}
    try{(transportKind==='content_note_resolution_v2'?validateContentLocalNote:validateLocalNote)(source,h.project_id);const b=localCanonicalBytes({local_format:1,kind:'note',account_id:s.accountId,device_id:s.deviceId,project_id:h.project_id,note_id:n.id,source_revision:1,source});try{if(b.length>MAX_LOCAL_SOURCE_BYTES)fail('resource_limit')}finally{b.fill(0)}}catch(e){if(e instanceof ResolutionPlannerError)throw e;fail('source_incompatible')}
   }
   return{resolution,content,transportKind}
  }finally{nonce.fill(0);ciphertext.fill(0);bytes?.fill(0)}
 }
 private async dependencies(s:BrowserSyncScope,row:BrowserInboxRecord,content:ContentNoteEvent|undefined,v:Inventory,amk:AccountMasterKey,step:<T>(p:Promise<T>)=>Promise<T>,current:()=>boolean){
  if(!content)return // Raw Legacy does not acquire invented Content dependencies.
  const d=content.dependencies,h=content.event.header,b=v.bindings.map(b=>validateBrowserProjectBinding(b,s)).find(b=>b.project_id===h.project_id)
  if(!b||b.mode!=='import_remote'||b.phase!=='captured'||b.remote_state!=='active'||d.bootstrap_id!==b.bootstrap_id)fail('dependency_unavailable')
  const rawHead=v.owners.find((x:any)=>x.owner_kind==='project_metadata'&&x.owner_id===h.project_id)
  if(!rawHead)fail('dependency_unavailable')
  const cert=await step(prepareMetadataPrefix(s,v,amk,current,true)),safe=verifiedMetadataSequences(cert,s,v)
  const head=validateBrowserMetadataHead(rawHead,s)
  const dependency=head.history.find(p=>p.event_id===d.metadata_event_id),tip=head.history.find(p=>p.event_id===head.tip_event_ids[0])
  if(head.bootstrap_id!==b.bootstrap_id||head.tip_event_ids.length!==1||!dependency||dependency.operation==='delete'||!safe.has(dependency.server_sequence)||!tip||tip.operation==='delete'||!safe.has(tip.server_sequence))fail('dependency_unavailable')
  if(content.event.version!==2)fail('invalid_resolution')
  const outputs=[content.event.result.note,...(content.event.resolution.strategy==='keep_both'?[content.event.resolution.retained_note]:[])]
  for(const n of outputs){
   if(n.stage_id===null){if(d.stage_event_ids.length)fail('dependency_unavailable');continue}
   const raw=v.owners.find((x:any)=>x.owner_kind==='stage'&&x.project_id===h.project_id&&x.stage_id===n.stage_id)
   if(!raw)fail('dependency_unavailable')
   const stage=validateStageHead(raw,s),st=await step(prepareStagePrefix(s,v,amk,current,cert,true)),covered=stageSequences(st,s,v)
   if(stage.bootstrap_id!==b.bootstrap_id||stage.history.some(p=>!covered.has(p.server_sequence))||!proveStageReferenceCoverage(stage.tip_event_ids,d.stage_event_ids,stage.history))fail('dependency_unavailable')
   for(const raw of v.rows){const r=validateBrowserInboxRecord(raw,s),e=JSON.parse(r.wire_item).event;if(r.server_sequence<row.server_sequence&&e.entity_type==='stage'&&e.project_id===h.project_id&&e.entity_id===n.stage_id&&(r.state!=='stage_proven'||!covered.has(r.server_sequence)))fail('dependency_unavailable')}
  }
 }
 private collision(s:BrowserSyncScope,resolution:NoteSyncResolutionV2,v:Inventory,futureChildren:WeakSet<object>,reservedClones:Map<string,string>){
  const decision=resolution.resolution;if(decision.strategy!=='keep_both')return
  const id=decision.retained_note.id
  // A retained *reference* from a future child is not an allocation. Received
  // unknown payloads cannot prove safe child identity and remain explicit blockers.
  if(v.localNotes.some((r:any)=>r.note_id===id)||v.intents.some((r:any)=>r.note_id===id)||v.history.some((r:any)=>r.note_id===id||r.retained_note_id===id))fail('clone_collision')
  for(const raw of v.out){const r=validateBrowserStoredUploadPair(raw,v.receipts.find((p:any)=>p.event_id===(raw as any).event_id),s),e=JSON.parse(r.raw_body).items[0].event;if(e.entity_type==='note'&&e.entity_id===id)fail('clone_collision')}
  for(const raw of v.rows){const r=validateBrowserInboxRecord(raw,s),w=JSON.parse(r.wire_item),e=w.event;if(e.event_id===resolution.header.event_id)continue
   if(e.entity_type==='note'&&e.entity_id===id){
    // Existing proven or genesis identities reserve the name. A future child is
    // checked cryptographically in prepare before this structural reservation pass.
    if(r.state!=='received'||!futureChildren.has(raw as object))fail('clone_collision')
   }
   if(e.entity_type==='note'&&reservedClones.get(e.event_id)===id)fail('clone_collision')
  }
 }
 async prepare(input:BrowserSyncScope,sequence:number){return this.prepareInternal(input,sequence,false)}
 /** Explicit peer-only next generation. The frozen initial planner stays strict. */
 async prepareSuccessor(input:BrowserSyncScope,sequence:number){return this.prepareInternal(input,sequence,true)}
 private async prepareInternal(input:BrowserSyncScope,sequence:number,successor:boolean):Promise<{token:ResolutionPlannerToken;status:ResolutionPlanStatus}>{
  try{
   const s=snapshotBrowserSyncScope(input),{lease,current}=await this.guard(s)
   const check=()=>{if(!current())fail('lifecycle_invalid')},step=async<T>(p:Promise<T>)=>{const value=await p;check();return value}
   if(!Number.isSafeInteger(sequence)||sequence<1)fail('invalid_resolution')
   const v=await step(this.load(s,current)),rows=v.rows.map(r=>successor?readFutureInboxRecord(r,s):validateBrowserInboxRecord(r,s)),row=rows.find(r=>r.server_sequence===sequence)
   if(!row||row.state!=='received'||row.server_sequence>v.cursor.pull_cursor)fail('invalid_resolution')
   if(new Set(rows.map(r=>r.event_id)).size!==rows.length||new Set(rows.map(r=>r.server_sequence)).size!==rows.length)fail('invalid_resolution')
   return await step(lease.use(async amk=>{
    const {resolution,content,transportKind}=await step(this.original(s,row,amk,check)),h=resolution.header,ids=resolution.resolution.resolved_event_ids
    const binding=v.bindings.map(b=>validateBrowserProjectBinding(b,s)).find(b=>b.project_id===h.project_id)
    if(!binding)fail('missing_bootstrap')
    if(binding.remote_state!=='active'||!(binding.mode==='import_remote'&&binding.phase==='captured'||binding.mode==='upload_existing'&&binding.phase==='completing'))fail('missing_bootstrap')
    const relevant=v.history.filter((r:any)=>r.project_id===h.project_id&&r.note_id===h.entity_id)
    for(const id of ids)if(!relevant.some((r:any)=>r.record_kind==='candidate'&&r.event_id===id)){
     if(v.intents.some((r:any)=>r.event_id===id)||relevant.some((r:any)=>r.record_kind==='local_version'&&r.header.event_id===id))fail('missing_publication')
     fail('missing_parent')
    }
    const prior=successor?await step(authenticateContinuationHistory(s,v,amk,current)):undefined
    if(successor){
     const roots=prior!.resolutions
     if(sequence!==v.cursor.pull_cursor||rows.length!==v.cursor.pull_cursor||rows.some((r,i)=>r.server_sequence!==i+1||r.server_sequence<sequence&&r.state==='received')
       ||ids.length!==2||roots.size!==1||v.out.length||v.receipts.length||v.intents.length||v.localNotes.length!==1||v.projects.length!==1
       ||resolution.resolution.strategy!=='choose_version'||transportKind!=='content_note_resolution_v2'
       ||[...roots.values()].some(r=>r.record.strategy!=='choose_version'||r.record.retained_note_id!==null||r.record.project_id!==h.project_id||r.record.note_id!==h.entity_id))fail('invalid_resolution')
     await step(verifyContinuationSources(s,v,prior!,amk,current))
    }
    const history=successor?inspectResolutionDag(s,relevant).history:validateNoteHistorySet(s,relevant),dag=inspectResolutionDag(s,history),group=history.find((r):r is BrowserNoteConflictGroup=>r.record_kind==='group'&&(!successor||prior!.groups.some(g=>g.group.group_id===r.group_id&&g.effective==='open'))),head=history.find(r=>r.record_kind==='head')
    if(!group||!head||head.record_kind!=='head')fail('missing_conflict')
    if(group.bootstrap_id!==binding.bootstrap_id||!same(group.tip_event_ids,ids)||!same(head.tip_event_ids,ids)||!same(dag.frontier(h.project_id,h.entity_id,binding.bootstrap_id),ids))fail('stale_inventory')
    const candidates=history.filter((r):r is BrowserNoteCandidate=>r.record_kind==='candidate'),parents=candidates.filter(c=>ids.includes(c.event_id))
    if(parents.length!==ids.length||h.revision!==Math.max(...parents.map(c=>c.header.revision))+1||parents.some(c=>c.bootstrap_id!==binding.bootstrap_id||c.header.parent_event_id!==group.common_parent_event_id))fail('invalid_resolution')
    if(v.intents.map(i=>validateNoteIntentRecord(i,s)).some(i=>i.project_id===h.project_id&&i.note_id===h.entity_id))fail('missing_publication')
    if(!successor)await step(authenticateContentAuthorityInventory(s,{...v,history:v.history},amk,current))
    for(const c of candidates){
     const inbound=rows.find(r=>r.event_id===c.event_id)
     if(c.source_device_id===s.deviceId){
      const o=v.out.find((r:any)=>r.event_id===c.event_id),receipt=v.receipts.find((r:any)=>r.event_id===c.event_id)
      if(!o||!receipt)fail('missing_publication')
      const out=validateBrowserStoredUploadPair(o,receipt,s),w=JSON.parse(out.raw_body).items[0]
      if(out.state!=='accepted'||out.protocol_version!==c.protocol_version||!same(w,JSON.parse(c.wire_item)))fail('invalid_resolution')
      if(inbound&&(inbound.server_sequence!==(receipt as any).server_sequence||!same(JSON.parse(inbound.wire_item).object,w.object)))fail('invalid_resolution')
     }else{
      if(!inbound||!['note_proven','note_conflict_preserved','content_note_proven','content_note_conflict_preserved'].includes(inbound.state))fail('missing_publication')
      const w=JSON.parse(inbound.wire_item),{device_id,server_sequence:_seq,...event}=w.event
      if(device_id!==c.source_device_id||c.protocol_version!==inbound.protocol_version||!same(c.record_version===2?w:{event,object:w.object},JSON.parse(c.wire_item)))fail('invalid_resolution')
      if(inbound.state==='note_proven')await step(verifyStoredNoteProof(s,amk,inbound))
      if(inbound.state==='content_note_proven'&&!successor)await step(verifyStoredContentNoteProof(s,amk,inbound,history as import('./noteHistoryRecords').BrowserNoteHistoryRecord[]))
     }
    }
    if(!successor)await step(verifyContentGroup(s,group,history as import('./noteHistoryRecords').BrowserNoteHistoryRecord[],rows as BrowserInboxRecord[],amk,{out:v.out,receipts:v.receipts}))
    const opened=new Map<string,NoteSyncPlaintext>();let materialBytes=0
    for(const c of candidates){const p=await step(openHistoryCandidate(s,c,amk));const bytes=new TextEncoder().encode(immutableWireIdentity(p));try{materialBytes+=bytes.length;if(materialBytes>16*1024*1024)fail('resource_limit')}finally{bytes.fill(0)}opened.set(c.event_id,p)}
    for(const m of group.members)if(m.source==='local_unsealed'){
     const frozen=history.find(r=>r.record_kind==='local_version'&&r.version_id===m.version_id),sealed=parents.find(c=>c.event_id===m.event_id)
     if(!frozen||frozen.record_kind!=='local_version'||!sealed||sealed.record_version!==1||sealed.origin!=='local_sealed'||(sealed.mutation_generation??0)<frozen.intent.mutation_generation)fail('missing_publication')
     await step(openFrozenLocalVersion(s,frozen,amk))
    }
    const result=resolution.result,decision=resolution.resolution,selected='selected_event_id'in decision?opened.get(decision.selected_event_id):undefined
    if(parents.some(c=>!same(route(opened.get(c.event_id)!.note),route(result.note))))fail('source_incompatible')
    if(decision.strategy==='choose_version'||decision.strategy==='keep_both')if(!selected||selected.header.operation!==result.operation||!same(selected.note,result.note))fail('invalid_resolution')
    if(decision.strategy==='keep_both'){
     const other=opened.get(decision.retained_event_id)
     if(parents.length!==2||parents.some(c=>c.header.operation!=='upsert')||!other||!same({...other.note,id:decision.retained_note.id,created_at:decision.retained_note.created_at,updated_at:decision.retained_note.updated_at},decision.retained_note)||!same(route(result.note),route(decision.retained_note)))fail('invalid_resolution')
    }
    try{await step(this.dependencies(s,row,content,successor?{...v,...continuationDependencyView(v,row)}:v,amk,step,current))}catch(e){check();if(e instanceof ResolutionPlannerError)throw e;fail('dependency_unavailable')}
    // Current-format inventory also retains the accepted shared ordinal/pull/ACK
    // invariants. Structural validation never grants AEAD or prefix authority.
    if(!successor)validateBrowserV9State([s],[v.out,v.rows,v.checkpoints,v.receipts,v.bindings,v.owners,v.projects,v.localNotes,v.history,v.intents])
    // Authenticate account-wide incoming Resolution reservations and distinguish a
    // valid future child reference from an already allocated proposed clone.
    const futureChildren=new WeakSet<object>(),reservedClones=new Map<string,string>()
    if(decision.strategy==='keep_both')for(const raw of v.rows){const r=validateBrowserInboxRecord(raw,s),w=JSON.parse(r.wire_item),e=w.event
     if(r.event_id===row.event_id||e.entity_type!=='note'||r.state!=='received')continue
     if(e.entity_id===decision.retained_note.id){
      const p=await step(openNoteTransport(s,amk,r.protocol_version,r.wire_item,['upsert','delete'].includes(e.operation)?'legacy':'content_note_v1'))
      if(p.plaintext.header.parent_event_id===h.event_id&&p.plaintext.header.revision===h.revision+1&&same(route(p.plaintext.note),route(decision.retained_note))&&e.project_id===h.project_id)futureChildren.add(raw as object)
     }else if(((r.protocol_version===2||r.protocol_version===3)&&e.operation==='resolution')||(r.protocol_version===3&&e.operation==='event')){
      // Only Resolution wrappers reserve a proposed second output. Ordinary rows
      // cannot be treated as failed Resolution decryption/fallback.
      if(e.operation==='event'){const nonce=decodeBase64Url(w.object.nonce),ciphertext=decodeBase64Url(w.object.ciphertext);let bytes:Uint8Array|undefined
       try{bytes=await step(decryptObjectBytes(amk,{userId:s.canonicalUserId,projectId:e.project_id,entityId:e.entity_id,entityType:'note'},{...w.object,nonce,ciphertext}));const c=unframeContentNote(bytes);if(c.account_id!==s.canonicalUserId||c.device_id!==e.device_id||c.event.header.event_id!==e.event_id)fail('invalid_resolution');if(c.event.version===2&&c.event.resolution.strategy==='keep_both')reservedClones.set(e.event_id,c.event.resolution.retained_note.id)}finally{nonce.fill(0);ciphertext.fill(0);bytes?.fill(0)}
      }else{const r2=await step(this.original(s,r,amk,check));if(r2.resolution.resolution.strategy==='keep_both')reservedClones.set(e.event_id,r2.resolution.resolution.retained_note.id)}
     }
    }
    this.collision(s,resolution,v,futureChildren,reservedClones)
    const snapshot=await step(this.local.readSnapshot(s,h.project_id))
    if(!same(snapshot.project,v.projects.find((p:any)=>p.project_id===h.project_id))||!same(snapshot.notes,v.localNotes.filter((n:any)=>n.project_id===h.project_id)))fail('stale_inventory')
    if(!Number.isSafeInteger(snapshot.source_revision+1)||decision.strategy==='keep_both'&&snapshot.notes.length>=MAX_LOCAL_NOTES)fail('resource_limit')
    const working=parents.find(c=>c.event_id===head.linear_head_event_id)
    if(!working||!working.projection)fail('source_incompatible')
    const actual=snapshot.notes.find(n=>n.note_id===h.entity_id)
    if(!actual||!same(actual,working.projection))fail('source_incompatible')
    await step(this.local.withDecryptedSnapshot(s,snapshot.token,async source=>{
     const note=source.notes.find(n=>n.note.id===h.entity_id)
     if(!note||!same(wireNote(note.note),opened.get(working.event_id)!.note))fail('source_incompatible')
     const projectPayload={local_format:1,kind:'project',account_id:s.accountId,device_id:s.deviceId,project_id:h.project_id,source_revision:snapshot.source_revision+1,deleted_at:null,project:source.project,member_ids:[...snapshot.notes.map(n=>n.note_id),...(decision.strategy==='keep_both'?[decision.retained_note.id]:[])].sort()}
     try{const bytes=localCanonicalBytes(projectPayload);bytes.fill(0)}catch{fail('resource_limit')}
    }))
    await step(this.local.assertSnapshot(s,snapshot.token))
    if(!same(await step(this.load(s,current)),v))fail('stale_inventory')
    const status=Object.freeze({readOnly:true as const,commitAuthorized:false as const,ackAuthorized:false as const,parentCount:parents.length,strategy:decision.strategy}),token=Object.freeze({}) as ResolutionPlannerToken
    plans.set(token,{successor,owner:this,scope:s,sequence,inventory:v,source:snapshot.token,current,resolution,transportKind,group,parents:opened,status,lease})
    return{token,status}
   }))
  }catch(e){if(e instanceof ResolutionPlannerError)throw e;if(e instanceof BrowserPersistenceError&&['closed','account_mismatch'].includes(e.code))fail('lifecycle_invalid');fail('invalid_resolution')}
 }
 /** Internal dormant handoff; no normal composition or raw plaintext input. */
 atomicApply():BrowserResolutionApplyRuntime {
  return new BrowserResolutionApplyRuntime(this,this.transaction,this.local,this.auth,this.keys,s=>this.guard(s), (s,current)=>this.load(s,current))
 }
 /** Bounded diagnostics only. A future apply transaction must independently CAS all witnesses. */
 async inspect(token:ResolutionPlannerToken):Promise<ResolutionPlanStatus>{
  const p=plans.get(token);if(!p||p.owner!==this||!p.current())fail('lifecycle_invalid')
  try{await this.local.assertSnapshot(p.scope,p.source);if(!p.current())fail('lifecycle_invalid');const v=await this.load(p.scope,p.current);if(!p.current())fail('lifecycle_invalid');if(!same(v,p.inventory)){plans.delete(token);fail('stale_inventory')}return p.status}catch(e){plans.delete(token);if(e instanceof ResolutionPlannerError)throw e;fail('stale_inventory')}
 }
}

export interface ResolutionApplyToken {readonly resolutionApplyToken:unique symbol}
interface ApplyPreparation {
 owner:BrowserResolutionApplyRuntime;plan:Plan;local:BrowserLocalAuthorityRuntime;
 source?:ProtectedResolutionSourcePlan;row?:BrowserResolutionInbox10;
 records?:[BrowserResolution4,BrowserGroupTransition2,BrowserResolutionHead2,BrowserResolutionHead2?]
}
const applications=new WeakMap<object,ApplyPreparation>()
/** Only the matching protected local hook can consume a fresh private preparation. */
export function verifiedResolutionForLocal(token:ResolutionApplyToken,local:BrowserLocalAuthorityRuntime,scope:BrowserSyncScope){
 const p=applications.get(token)
 if(!p||p.local!==local||!same(p.plan.scope,scope)||!p.plan.current())fail('lifecycle_invalid')
 return {resolution:p.plan.resolution,transportKind:p.plan.transportKind,current:p.plan.current}
}
const recordDigest=async(value:unknown,field='record_digest')=>{const copy={...value as Record<string,unknown>};delete copy[field];return noteProofDigest(copy)}

/** Dormant protected writer, obtained solely from an authenticated planner runtime. */
export class BrowserResolutionApplyRuntime {
 constructor(private readonly planner:BrowserResolutionPlannerRuntime,private readonly transaction:BrowserSyncTransaction,private readonly local:BrowserLocalAuthorityRuntime,private readonly auth:NormalUserAuthRuntime,private readonly keys:RuntimeKeyContext,private readonly guard:(s:BrowserSyncScope)=>Promise<{lease:AuthoritativeKeyContextLease;current:()=>boolean}>,private readonly load:(s:BrowserSyncScope,current:()=>boolean)=>Promise<Inventory>){}
 async prepare(token:ResolutionPlannerToken):Promise<ResolutionApplyToken>{
  const original=plans.get(token)
  if(!original||original.owner!==this.planner||!original.current())fail('lifecycle_invalid')
  await this.planner.inspect(token)
  if(!original.current())fail('lifecycle_invalid')
  // Reopen the original wire and every published parent with the real current
  // planner. A stale plan is never silently replaced by a new conflict decision.
  const renewed=await (original.successor?this.planner.prepareSuccessor(original.scope,original.sequence):this.planner.prepare(original.scope,original.sequence)),p=plans.get(renewed.token)!
  if(!original.current()||!same(p.inventory,original.inventory))fail('stale_inventory')
  const apply=Object.freeze({}) as ResolutionApplyToken,prepared:ApplyPreparation={owner:this,plan:p,local:this.local}
  applications.set(apply,prepared)
  const check=()=>{if(!p.current()||!original.current())fail('lifecycle_invalid')}
  const digest=async(value:unknown)=>{const hash=await noteProofDigest(value);check();return hash}
  try{
   const source=await this.local.prepareProtectedResolutionApply(p.scope,p.source,apply);check()
   prepared.source=source.token
   await p.lease.use(async amk=>{
    const s=p.scope,v=p.inventory,resolution=p.resolution,h=resolution.header,d=resolution.resolution
    const row=validateBrowserInboxRecord(v.rows.find((r:any)=>r.server_sequence===p.sequence),s),wire=JSON.parse(row.wire_item)
    const nonce=decodeBase64Url(wire.object.nonce),ciphertext=decodeBase64Url(wire.object.ciphertext);let frame:Uint8Array|undefined
    const group=p.group,head=v.history.find((r:any)=>r.record_kind==='head'&&r.project_id===h.project_id&&r.note_id===h.entity_id) as BrowserNoteHead
    try{
     frame=await decryptObjectBytes(amk,{userId:s.canonicalUserId,projectId:h.project_id,entityId:h.entity_id,entityType:'note'},{...wire.object,nonce,ciphertext});check()
     const content=p.transportKind==='content_note_resolution_v2'?unframeContentNote(frame):undefined
     const r:BrowserResolution4={record_version:4,record_kind:'resolution',record_id:h.event_id,account_id:s.accountId,device_id:s.deviceId,project_id:h.project_id,note_id:h.entity_id,bootstrap_id:group.bootstrap_id,event_id:h.event_id,source_device_id:wire.event.device_id,protocol_version:row.protocol_version,transport_kind:p.transportKind,server_sequence:p.sequence,header:h,wire_item:row.wire_item,wire_digest:await digest({protocol_version:row.protocol_version,item:wire}),payload_digest:await digest(resolution),dependency_digest:await digest({scope:s,bootstrap_id:group.bootstrap_id,project_id:h.project_id,note_id:h.entity_id,event_id:h.event_id,transport:row,dependencies:content?.dependencies??null,owners:v.owners,bindings:v.bindings}),remote_group_id:d.conflict_group_id,remote_generation:d.conflict_generation,local_group_id:group.group_id,local_generation:group.conflict_generation,group_digest:await digest(group),parent_event_ids:d.resolved_event_ids,strategy:d.strategy,result_operation:resolution.result.operation,retained_note_id:d.strategy==='keep_both'?d.retained_note.id:null,evidence_envelope:source.project.envelope,record_digest:''}
     const parents=[]
     for(const id of r.parent_event_ids){
      const c=v.history.find((r:any)=>r.record_kind==='candidate'&&r.event_id===id) as BrowserNoteCandidate,m=group.members.find(m=>m.event_id===id)!,local=m.source==='local_unsealed'
      const frozen=local?v.history.find((r:any)=>r.record_kind==='local_version'&&r.version_id===m.version_id) as {record_digest:string}:undefined
      const receipt=local?v.receipts.find((r:any)=>r.event_id===id) as {server_sequence:number}:undefined
      parents.push({event_id:id,revision:c.header.revision,operation:c.header.operation,history_digest:c.record_digest,inbox_digest:await digest(v.rows.find((r:any)=>r.event_id===id)??null),publication_source:local?'local_accepted':m.source,server_sequence:local?receipt!.server_sequence:m.server_sequence,local_mutation_generation:local?c.mutation_generation:null,frozen_version_digest:frozen?.record_digest??null,snapshot_digest:await digest(p.parents.get(id)!.note)})
      check()
     }
     const before={project:v.projects.find((r:any)=>r.project_id===h.project_id),notes:v.localNotes.filter((n:any)=>n.project_id===h.project_id&&n.note_id===h.entity_id)}
     const evidence={resolution_format:1,kind:'resolution_apply',identity:resolutionIdentity(r),authenticated_payload:{wire_frame_digest:await resolutionBytesDigest(frame),wrapper_payload_digest:content?await digest(content):null,resolution_payload_digest:r.payload_digest},parents,group_snapshot:group,head_before:head,source_before:before,source_after:{project:source.project,notes:source.notes}}
     boundResolutionJson(evidence,MAX_RESOLUTION_EVIDENCE_BYTES)
     const bytes=new TextEncoder().encode(canonical(evidence));let encrypted:Awaited<ReturnType<typeof encryptObjectBytes>>|undefined
     try{encrypted=await encryptObjectBytes(amk,{userId:s.canonicalUserId,projectId:h.project_id,entityId:h.entity_id,entityType:'note'},bytes);check();r.evidence_envelope={crypto_version:1,aad_version:1,nonce:encodeBase64Url(encrypted.nonce),ciphertext:encodeBase64Url(encrypted.ciphertext)}}finally{bytes.fill(0);encrypted?.nonce.fill(0);encrypted?.ciphertext.fill(0)}
     r.record_digest=await recordDigest(r);check()
     const transition:BrowserGroupTransition2={record_version:2,record_kind:'group_transition',record_id:group.group_id,account_id:s.accountId,device_id:s.deviceId,project_id:h.project_id,note_id:h.entity_id,bootstrap_id:group.bootstrap_id,group_id:group.group_id,from_generation:group.conflict_generation,from_digest:r.group_digest,resolution_event_id:h.event_id,resolution_digest:r.record_digest,resolved_parent_ids:r.parent_event_ids,effective_lifecycle:'resolved',record_digest:''}
     transition.record_digest=await recordDigest(transition);check()
     const nextHead:BrowserResolutionHead2={...head,record_version:2,head_revision:head.head_revision+1,linear_head_event_id:h.event_id,tip_event_ids:[h.event_id],node_ref:{kind:'resolution_result',resolution_event_id:h.event_id,result_role:'original',resolution_digest:r.record_digest}}
     const cloneHead:BrowserResolutionHead2|undefined=r.retained_note_id?{...nextHead,note_id:r.retained_note_id,head_revision:1,node_ref:{kind:'resolution_result',resolution_event_id:h.event_id,result_role:'retained',resolution_digest:r.record_digest}}:undefined
     const proof:BrowserResolutionProof1={record_version:1,owner_kind:'note_resolution',outcome:'applied',account_id:s.accountId,bootstrap_id:group.bootstrap_id,protocol_version:row.protocol_version,reference:wire.event,server_sequence:p.sequence,header:h,wire_digest:r.wire_digest,payload_digest:r.payload_digest,dependency_digest:r.dependency_digest,resolution_digest:r.record_digest,transition_digest:transition.record_digest,output_digests:{original:await digest(source.notes.find(n=>n.note_id===r.note_id)),retained:r.retained_note_id?await digest(source.notes.find(n=>n.note_id===r.retained_note_id)):null},outcome_digest:''}
     proof.outcome_digest=await recordDigest(proof,'outcome_digest');check()
     const inbox:BrowserResolutionInbox10={...row,protocol_version:row.protocol_version,record_version:10,state:'note_resolution_proven',resolution_proof:proof}
     const records:[BrowserResolution4,BrowserGroupTransition2,BrowserResolutionHead2,BrowserResolutionHead2?]=[r,transition,nextHead,cloneHead]
     for(const record of records)if(record)boundResolutionJson(record)
     boundResolutionJson(inbox)
     const predicted={...v,projects:v.projects.map((p:any)=>p.project_id===h.project_id?source.project:p),history:[...v.history.filter(raw=>!same(raw,head)),...records.filter(Boolean)],rows:v.rows.map(raw=>same(raw,row)?inbox:raw),localNotes:[...v.localNotes.filter((n:any)=>!source.notes.some(a=>a.note_id===n.note_id)),...source.notes]}
     // A successful commit must remain within the same bounded reopen/replay
     // inventory budget. Reject before writing rather than strand a large result.
     let left=MAX_PLANNER_INVENTORY_BYTES-2*stores.length
     for(const array of [predicted.rows,predicted.out,predicted.receipts,predicted.bindings,predicted.owners,predicted.history,predicted.intents,predicted.projects,predicted.localNotes,predicted.checkpoints]){
      if(array.length>MAX_PLANNER_ROWS)fail('resource_limit')
      for(const value of array){try{boundResolutionJson(value,left)}catch{fail('resource_limit')}
       const encoded=new TextEncoder().encode(JSON.stringify(value));try{left-=encoded.length+1;if(left<0)fail('resource_limit')}finally{encoded.fill(0)}
      }
     }
     // Future v10 authentication is a prerequisite, not an assertion made after
     // a write has already committed. The old production readers remain strict.
     await authenticateResolutionHistory(s,predicted,amk,p.current);check()
     prepared.records=records;prepared.row=inbox
    }finally{frame?.fill(0);nonce.fill(0);ciphertext.fill(0)}
   })
   check();return apply
  }catch(e){applications.delete(apply);throw e}
 }
 async commit(token:ResolutionApplyToken):Promise<{status:'applied'}>{
  const a=applications.get(token)
  if(!a||a.owner!==this||!a.source||!a.row||!a.records||!a.plan.current())fail('lifecycle_invalid')
  applications.delete(token) // Single-use even on abort; retry needs fresh crypto/auth.
  const p=a.plan,s=p.scope,records=a.records,row=a.row,source=a.source
  let abortActive:((e:unknown)=>void)|undefined
  const stop=()=>abortActive?.(new ResolutionPlannerError('lifecycle_invalid'))
  const unAuth=this.auth.onInvalidated(stop),unKey=this.keys.onInvalidated(stop)
  try{
   return await this.transaction(s,stores,'readwrite',(tx,result,abort)=>{
    abortActive=abort
    const check=()=>{if(!p.current())fail('lifecycle_invalid')}
    try{check();scheduleResolutionInventory(tx,s,p.current,v=>{
     try{
      check();if(!same(v,p.inventory))fail('stale_inventory')
      scheduleProtectedResolutionApply(tx,s,source,()=>{
       check();const history=tx.objectStore('legacyNoteHistory')
       const requests=[history.add(records[0]),history.add(records[1]),history.put(records[2])]
       if(records[3])requests.push(history.add(records[3]))
       requests.forEach(r=>r.onsuccess=()=>{try{check()}catch(e){abort(e)}})
      },()=>{
       check();const request=tx.objectStore('inbox').put(row)
       request.onsuccess=()=>{try{check();result({status:'applied'})}catch(e){abort(e)}}
      },abort)
     }catch(e){abort(e)}
    },abort)}catch(e){abort(e)}
   })
  }finally{abortActive=undefined;unAuth();unKey()}
 }
 /** Exact replay after restart. Never allocates a token, nonce, clone or counter. */
 async reconcile(input:BrowserSyncScope,sequence:number):Promise<{status:'already_applied'}>{
  const s=snapshotBrowserSyncScope(input),{lease,current}=await this.guard(s)
  const check=()=>{if(!current())fail('lifecycle_invalid')}
  const v=await this.load(s,current);check()
  const row=readFutureInboxRecord(v.rows.find((r:any)=>r.server_sequence===sequence),s)
  if(row.state!=='note_resolution_proven')fail('invalid_resolution')
  await lease.use(async amk=>{
   const authenticated=await authenticateResolutionHistory(s,v,amk,current);check()
   const r=authenticated.dag.events.get(row.event_id)
   if(!r||r.record_kind!=='resolution')fail('invalid_resolution')
   const nonce=decodeBase64Url(r.evidence_envelope.nonce),ciphertext=decodeBase64Url(r.evidence_envelope.ciphertext);let bytes:Uint8Array|undefined
   try{
    bytes=await decryptObjectBytes(amk,{userId:s.canonicalUserId,projectId:r.project_id,entityId:r.note_id,entityType:'note'},{...r.evidence_envelope,nonce,ciphertext});check()
    const evidence=JSON.parse(new TextDecoder('utf-8',{fatal:true}).decode(bytes)) as {head_before:BrowserNoteHead;source_after:{project:LocalProjectRecord;notes:LocalNoteRecord[]}}
    const originalHead:BrowserResolutionHead2={...evidence.head_before,record_version:2,head_revision:evidence.head_before.head_revision+1,linear_head_event_id:r.event_id,tip_event_ids:[r.event_id],node_ref:{kind:'resolution_result',resolution_event_id:r.event_id,result_role:'original',resolution_digest:r.record_digest}}
    const heads=[originalHead,...(r.retained_note_id?[{...originalHead,note_id:r.retained_note_id,head_revision:1,node_ref:{kind:'resolution_result',resolution_event_id:r.event_id,result_role:'retained',resolution_digest:r.record_digest}}]:[])]
    for(const head of heads)if(!v.history.some(raw=>same(raw,head)))fail('invalid_resolution')
    // F accepts only the exact still-current output. General historical source
    // continuation and contiguous-prefix qualification belong to the next slice.
    if(!same(v.projects.find((p:any)=>p.project_id===r.project_id),evidence.source_after.project)||evidence.source_after.notes.some(n=>!same(v.localNotes.find((x:any)=>x.note_id===n.note_id),n)))fail('stale_inventory')
   }finally{nonce.fill(0);ciphertext.fill(0);bytes?.fill(0)}
  })
  if(!same(await this.load(s,current),v))fail('stale_inventory');check()
  return{status:'already_applied'}
 }
}
