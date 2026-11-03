import { inspectResolutionDag } from './resolutionDag'
import type {MetadataHandoffInspection} from './historicalMetadataDependency'
import {consumeHHistoryRequest,type HHistoryToken,type BrowserResolutionContinuationRuntime} from './resolutionContinuation'
import {consumeGHistoryRequest,consumeQHistoryRequest,type GHistoryToken,type QHistoryToken,type BrowserResolutionPrefixRuntime} from './resolutionPrefix'
import {consumeOHistoryRequest,type OHistoryToken,type BrowserV10OrdinaryApplyRuntime} from './v10OrdinaryApply'
import type {ResolutionInventorySnapshot} from './resolutionPlanner'
import {decryptObjectBytes,type AccountMasterKey} from '@/crypto'
import {decodeBase64Url} from '@/api/base64url'
import {canonical} from '@/cloud/projectMetadataCodec'
import {decodeNoteSyncResolutionV2,type NoteSyncResolutionV2} from '@/cloud/noteSyncResolutionV2Codec'
import {unframeContentNote} from '@/cloud/contentNoteCodec'
import type {NoteSyncPlaintext} from '@/cloud/noteSyncCodec'
import {BrowserPersistenceError} from './persistence'
import {authenticateResolutionHistory,authenticateTerminalMetadataResolutions,authenticateMetadataHandoffResolutions,type ResolutionInventory} from './resolutionAuthentication'
import {rejectLegacy3ResolutionRoots,boundResolutionJson,readFutureInboxRecord,type BrowserResolution4} from './resolutionRecords'
import {openHistoryCandidate} from './noteHistory'
import {openFrozenLocalVersion,unsealedPlaintext} from './noteUnsealedVersion'
import {noteProofDigest,noteOutcomeDigest,verifyStoredNoteProof} from './legacyNoteApply'
import {contentProofFor} from './contentNoteProof'
import {validateBrowserProjectBinding} from './projectBindings'
import {validateNoteIntentRecord} from './noteIntentRecords'
import {authenticateContentIntent2,authenticateContentReceipt2,authenticateContentSelfEcho2} from './contentAuthorityAuthentication'
import {validateBrowserInboxRecord,validateBrowserStoredUploadPair,immutableWireIdentity,type BrowserSyncScope} from './syncDurability'
import type {BrowserNoteCandidate,BrowserNoteConflictGroup,BrowserNoteHead} from './noteHistoryRecords'
import {validateLocalNoteRecord,validateLocalProjectRecord,validatePersistedLocalNote,localExact,validateLocalProject,MAX_LOCAL_NOTES,type LocalEnvelope,type LocalProjectPayload,type LocalProjectRecord,type LocalNoteRecord} from './localAuthorityRecords'
import {prepareNotePrerequisite} from './noteTransportPrerequisite'
import type {BrowserPrefixInventory} from './provenPrefixRecords'
export const sameResolutionValue=(a:unknown,b:unknown)=>immutableWireIdentity(a)===immutableWireIdentity(b)
export const MAX_CONTINUATION_MATERIAL_BYTES=16*1024*1024
const same=sameResolutionValue
export function continuationBad():never{throw new BrowserPersistenceError('corrupt_record')}
function bad():never{return continuationBad()}
export async function openHistoricalJson(s:BrowserSyncScope,project:string,id:string,type:'note'|'project_metadata',envelope:LocalEnvelope,amk:AccountMasterKey,current:()=>boolean){
 const nonce=decodeBase64Url(envelope.nonce),ciphertext=decodeBase64Url(envelope.ciphertext);let bytes:Uint8Array|undefined
 try{bytes=await decryptObjectBytes(amk,{userId:s.canonicalUserId,projectId:project,entityId:id,entityType:type},{...envelope,nonce,ciphertext});if(!current())bad();return bytes.slice()}finally{nonce.fill(0);ciphertext.fill(0);bytes?.fill(0)}
}
/** Only a Metadata/Stage prerequisite view. FULL future DAG and publication checks
 * run separately below; this view never supplies Note/history or prefix authority. */
export function continuationDependencyView(v:ResolutionInventory,row:unknown):BrowserPrefixInventory{
 return{...v,rows:v.rows.filter(r=>same(r,row)||['project_metadata','stage','stage_order'].includes(JSON.parse((r as {wire_item:string}).wire_item).event.entity_type))}
}
export interface HistoricalResolutionOutput {
 record:BrowserResolution4;payload:NoteSyncResolutionV2;headBefore:BrowserNoteHead;
 project:LocalProjectPayload;notes:LocalNoteRecord[]
}
/** Isolated future-v10 historical adapters, never an apply/prefix certificate. */
export async function authenticateContinuationHistory(s:BrowserSyncScope,v:ResolutionInventory,amk:AccountMasterKey,current:()=>boolean){
 rejectLegacy3ResolutionRoots(v.history)
 const base=await authenticateResolutionHistory(s,v,amk,current,MAX_CONTINUATION_MATERIAL_BYTES)
 return authenticateContinuationCore(s,v,amk,current,base,async(row,binding)=>prepareNotePrerequisite(s,validateBrowserInboxRecord(row,s),binding,amk,current,undefined,continuationDependencyView(v,row)))
}
/** H-only private owner composition. Generic readers retain T's guard. */
export async function authenticateHContinuationHistory(token:HHistoryToken,owner:BrowserResolutionContinuationRuntime){
 return authenticateOwnedContinuationHistory(consumeHHistoryRequest(token,owner))
}
/** G-only composition. H requests, Q/default readers and diagnostics cannot enter. */
export async function authenticateGContinuationHistory(token:GHistoryToken,owner:BrowserResolutionPrefixRuntime){
 return authenticateOwnedContinuationHistory(consumeGHistoryRequest(token,owner))
}
/** O-only composition. It authenticates history but grants no source write. */
export async function authenticateOContinuationHistory(token:OHistoryToken,owner:BrowserV10OrdinaryApplyRuntime){
 const request=consumeOHistoryRequest(token,owner)
 return{...request,history:await authenticateOwnedContinuationHistory(request)}
}
/** Q-only composition. It authenticates history but grants no cursor write. */
export async function authenticateQContinuationHistory(token:QHistoryToken,owner:BrowserResolutionPrefixRuntime){
 const request=consumeQHistoryRequest(token,owner)
 return{...request,history:await authenticateOwnedContinuationHistory(request)}
}
async function authenticateOwnedContinuationHistory({scope:s,inventory:v,amk,current}:{scope:BrowserSyncScope;inventory:ResolutionInventorySnapshot;amk:AccountMasterKey;current:()=>boolean}){
 const base=await authenticateResolutionHistory(s,v,amk,current,MAX_CONTINUATION_MATERIAL_BYTES)
 if(base.resolutions!==1)bad() // This slice accepts one genuine first-generation F.
 return authenticateContinuationCore(s,v,amk,current,base,async(row,binding)=>prepareNotePrerequisite(s,validateBrowserInboxRecord(row,s),binding,amk,current,undefined,continuationDependencyView(v,row)))
}
/** Explicit retained-outcome composition only. Complete .13 Metadata authentication
 * runs inside the F pass; this path never calls a live certificate factory. */
export async function authenticateTerminalMetadataContinuations(s:BrowserSyncScope,v:ResolutionInventorySnapshot,amk:AccountMasterKey,current:()=>boolean){
 const base=await authenticateTerminalMetadataResolutions(s,v,amk,current)
 return authenticateContinuationCore(s,v,amk,current,base,async row=>{
  // Complete-inventory interval/proof authentication above covered ALL candidates.
  // Still reject any first apply or incompatible publication here, independently.
  const r=readFutureInboxRecord(row,s)
  if(r.protocol_version!==3||!['content_note_proven','content_note_conflict_preserved'].includes(r.state))bad()
 })
}
/** Dormant private Metadata verifier: retained outcomes only, never first apply. */
export async function authenticateMetadataHandoffContinuations(s:BrowserSyncScope,v:ResolutionInventorySnapshot,amk:AccountMasterKey,current:()=>boolean,mode:MetadataHandoffInspection){
 const base=await authenticateMetadataHandoffResolutions(s,v,amk,current,mode)
 return authenticateContinuationCore(s,v,amk,current,base,async row=>{
  const r=readFutureInboxRecord(row,s)
  if(r.protocol_version!==3||!['content_note_proven','content_note_conflict_preserved'].includes(r.state))bad()
 })
}

async function authenticateContinuationCore(s:BrowserSyncScope,v:ResolutionInventory,amk:AccountMasterKey,current:()=>boolean,base:Awaited<ReturnType<typeof authenticateResolutionHistory>>,verifyPeerDependencies:(row:unknown,binding:ReturnType<typeof validateBrowserProjectBinding>)=>Promise<unknown>){
 const check=()=>{if(!current())throw new BrowserPersistenceError('closed')}
 check()
 const dag=base.dag,rows=v.rows.map(r=>readFutureInboxRecord(r,s)),bindings=v.bindings.map(b=>validateBrowserProjectBinding(b,s)),opened=new Map<string,NoteSyncPlaintext>(),resolutions=new Map<string,HistoricalResolutionOutput>()
 if(rows.some(r=>r.server_sequence>v.cursor.pull_cursor))bad()
 for(const r of dag.events.values())if(r.record_kind==='resolution'){
  let bytes=await openHistoricalJson(s,r.project_id,r.note_id,'note',r.evidence_envelope,amk,current)
  let e:{head_before:BrowserNoteHead;source_after:{project:LocalProjectRecord;notes:LocalNoteRecord[]}}
  try{const text=new TextDecoder('utf-8',{fatal:true}).decode(bytes);e=JSON.parse(text);if(canonical(e)!==text)bad()}finally{bytes.fill(0)}
  const wire=JSON.parse(r.wire_item);bytes=await openHistoricalJson(s,r.project_id,r.note_id,'note',wire.object,amk,current)
  let payload:NoteSyncResolutionV2
  try{if(r.transport_kind==='legacy_resolution_v2')payload=decodeNoteSyncResolutionV2(bytes);else{const wrapper=unframeContentNote(bytes);if(wrapper.event.version!==2)bad();payload=wrapper.event}}finally{bytes.fill(0)}
  bytes=await openHistoricalJson(s,r.project_id,r.project_id,'project_metadata',e.source_after.project.envelope,amk,current)
  let project:LocalProjectPayload
  try{project=JSON.parse(new TextDecoder('utf-8',{fatal:true}).decode(bytes))}finally{bytes.fill(0)}
  resolutions.set(r.event_id,{record:r,payload,headBefore:e.head_before,project,notes:e.source_after.notes});check()
 }
 const outgoing=v.out.map(o=>validateBrowserStoredUploadPair(o,v.receipts.find((r:any)=>r.event_id===(o as any).event_id),s))
 const intents=(v.intents??[]).map(i=>validateNoteIntentRecord(i,s))
 const notes=(v.localNotes??[]).map((n:any)=>validateLocalNoteRecord(n,s,n.project_id)),projects=((v as ResolutionInventory&{projects?:unknown[]}).projects??[]).map(p=>validateLocalProjectRecord(p,s))
 if(new Set(notes.map(n=>n.note_id)).size!==notes.length||new Set(projects.map(p=>p.project_id)).size!==projects.length||new Set(intents.map(i=>i.note_id)).size!==intents.length)bad()
 const allocations=[...outgoing,...intents].sort((a,b)=>a.local_ordinal-b.local_ordinal)
 if(new Set(allocations.map(r=>r.event_id)).size!==allocations.length||allocations.some((r,i)=>r.local_ordinal!==i+1)||v.cursor.next_local_ordinal!==allocations.length+1)bad()
 for(const receipt of v.receipts)if(!outgoing.some(o=>o.event_id===(receipt as any).event_id))bad()
 for(const i of intents){
  if(i.record_version===2)await authenticateContentIntent2(s,i,amk,current);else await unsealedPlaintext(s,i,amk)
  if(!notes.some(n=>same(n,i.snapshot)))bad()
  const head=dag.history.find(h=>h.record_kind==='head'&&h.project_id===i.project_id&&h.note_id===i.note_id)
  if(i.parent_event_id!==null&&(!head||head.record_kind!=='head'||head.record_version!==1&&i.record_version!==2||head.linear_head_event_id!==i.parent_event_id||head.tip_event_ids.length!==1))throw new BrowserPersistenceError('unsupported_local_source')
  check()
 }
 let material=0
 for(const c of dag.events.values())if(c.record_kind==='candidate'){
  const p=await openHistoryCandidate(s,c,amk);check();boundResolutionJson(p,MAX_CONTINUATION_MATERIAL_BYTES-material)
  const bytes=new TextEncoder().encode(JSON.stringify(p));try{material+=bytes.length;if(material>MAX_CONTINUATION_MATERIAL_BYTES)bad()}finally{bytes.fill(0)}
  opened.set(c.event_id,p)
  const binding=bindings.find(b=>b.project_id===c.project_id)
  if(!binding||binding.bootstrap_id!==c.bootstrap_id||binding.remote_state!=='active')bad()
  const row=rows.find(r=>r.event_id===c.event_id)
  if(c.source_device_id===s.deviceId){
   const out=outgoing.find(o=>o.event_id===c.event_id),receipt=v.receipts.find((r:any)=>r.event_id===c.event_id)
   if(!out||!same(JSON.parse(out.raw_body).items[0],JSON.parse(c.wire_item))||out.state==='accepted'&&!receipt)bad()
   if(c.record_version===3){if(await noteProofDigest(out.raw_body)!==c.raw_body_digest||out.local_ordinal!==c.intent.local_ordinal)bad();if(receipt)await authenticateContentReceipt2(s,receipt,c,out,amk,current);if(row?.state==='content_note_self_echo_proven'){if(!receipt)bad();await authenticateContentSelfEcho2(s,row,c,receipt as never,out,amk,current)}}
   else if(row){if(row.state!=='note_proven'||row.note_proof.outcome!=='self_echo_applied'||!receipt||row.server_sequence!==(receipt as any).server_sequence)bad();await verifyStoredNoteProof(s,amk,row)}
   check();continue
  }
  if(!row||!['note_proven','note_conflict_preserved','content_note_proven','content_note_conflict_preserved'].includes(row.state))bad()
  const wire=JSON.parse(row.wire_item),{device_id,server_sequence:_sequence,...event}=wire.event
  if(device_id!==c.source_device_id||row.protocol_version!==c.protocol_version||!same(JSON.parse(c.wire_item),c.record_version===2?wire:{event,object:wire.object}))bad()
  if(row.state==='note_proven'){
   await verifyStoredNoteProof(s,amk,row)
   if(row.note_proof.bootstrap_id!==c.bootstrap_id||row.note_proof.local_note_digest!==c.projection_digest||c.projection&&row.note_proof.source_revision!==c.projection.source_revision)bad()
  }else if(row.state==='content_note_proven'){
   if(c.record_version!==2||!same(await contentProofFor(s,row,c,p.mutation),row.content_note_proof))bad()
  }else if(row.state==='note_conflict_preserved'||row.state==='content_note_conflict_preserved'){
   const proof=row.state==='note_conflict_preserved'?row.note_conflict_proof:row.content_note_conflict_proof
   const group=dag.history.find((g):g is BrowserNoteConflictGroup=>g.record_kind==='group'&&g.group_id===proof.group_id&&g.project_id===c.project_id&&g.note_id===c.note_id),member=group?.members.find(m=>m.event_id===c.event_id)
   if(!group||group.bootstrap_id!==c.bootstrap_id||!member||member.source!=='remote'||member.server_sequence!==row.server_sequence||member.conflict_generation!==proof.conflict_generation||!same(proof.header,p.header)||proof.mutation!==p.mutation||proof.bootstrap_id!==c.bootstrap_id||await noteOutcomeDigest(proof)!==proof.outcome_digest||c.projection!==null)bad()
   if(row.state==='content_note_conflict_preserved'){
    if(c.record_version!==2)bad();const t=c.transport_proof,cp=row.content_note_conflict_proof
    if(cp.candidate_digest!==c.record_digest||cp.wire_digest!==c.wire_digest||cp.logical_digest!==await noteProofDigest(p)||cp.dependency_digest!==t.dependency_digest||cp.transport_proof_digest!==t.proof_digest||cp.common_parent_event_id!==group.common_parent_event_id)bad()
   }else if(proof.wire_digest!==await noteProofDigest({protocol_version:row.protocol_version,event:wire.event,object:wire.object}))bad()
   const local=group.members.find(m=>m.source==='local_unsealed')
   if(local){const frozen=dag.history.find(h=>h.record_kind==='local_version'&&h.version_id===local.version_id);if(!frozen||frozen.record_kind!=='local_version'||proof.record_version!==2||proof.local_version_id!==frozen.version_id||proof.local_version_digest!==frozen.record_digest||proof.local_mutation_generation!==frozen.intent.mutation_generation)bad();await openFrozenLocalVersion(s,frozen,amk)}else if(proof.record_version!==1)bad()
  }
  if(c.record_version===2)await verifyPeerDependencies(row,binding)
  check()
 }
 for(const row of rows)if(['note_proven','note_conflict_preserved','content_note_proven','content_note_conflict_preserved','content_note_self_echo_proven'].includes(row.state)&&!dag.events.has(row.event_id))bad()
 for(const {group} of base.groups){
  const parent=dag.events.get(group.common_parent_event_id)
  const resolution=parent?.record_kind==='resolution'?resolutions.get(parent.event_id):undefined
  const baseNote=parent?.record_kind==='candidate'?opened.get(parent.event_id)?.note:parent?.record_kind==='resolution'?parent.note_id===group.note_id?resolution?.payload.result.note:resolution?.payload.resolution.strategy==='keep_both'?resolution.payload.resolution.retained_note:undefined:undefined
  if(!baseNote)bad()
  const route=(n:NoteSyncPlaintext['note'])=>[n.id,n.project_id,n.stage_id,n.source_type,n.source_map_id,n.source_node_id,n.content_format]
  for(const m of group.members){
   let value=opened.get(m.event_id)
   if(m.source==='local_unsealed'){const frozen=dag.history.find(h=>h.record_kind==='local_version'&&h.version_id===m.version_id);if(!frozen||frozen.record_kind!=='local_version')bad();value=await openFrozenLocalVersion(s,frozen,amk)}
   if(!value||!same(route(baseNote),route(value.note)))bad()
  }
 }
 for(const g of base.groups)if(g.effective==='open'){
  const group=g.group,head=dag.history.find(h=>h.record_kind==='head'&&h.project_id===group.project_id&&h.note_id===group.note_id),working=group.members.find(m=>m.source!=='remote')
  if(!head||head.record_kind!=='head'||!working)bad()
  if(working.source==='local_unsealed'){
   // Existing frozen admission may precede sealing. It remains historical;
   // current intent/publication crypto above decides ownership, never a new grant.
   if(head.linear_head_event_id!==group.common_parent_event_id&&head.linear_head_event_id!==working.event_id)bad()
  }else if(head.linear_head_event_id!==working.event_id||!same(head.tip_event_ids,group.tip_event_ids))bad()
  for(const m of group.members)if(m.source!=='local_unsealed'){
   const row=rows.find(r=>r.event_id===m.event_id)
   if(!row||row.server_sequence!==m.server_sequence||m.source==='remote_applied'&&!['note_proven','content_note_proven'].includes(row.state)||m.source==='remote'&&!['note_conflict_preserved','content_note_conflict_preserved'].includes(row.state))bad()
  }
 }
 check();return{readOnly:true as const,dag,resolutions,opened,groups:base.groups,rows}
}

/** Canonical encrypted current-source checks, distinct from historical outcomes. */
export async function verifyContinuationSources(s:BrowserSyncScope,v:ResolutionInventory&{projects:unknown[]},history:Awaited<ReturnType<typeof authenticateContinuationHistory>>,amk:AccountMasterKey,current:()=>boolean){
 const superseded=new Set<string>()
 const roots=[...history.resolutions.values()]
 for(const projectId of new Set(roots.map(r=>r.record.project_id))){
  const chain=roots.filter(r=>r.record.project_id===projectId).sort((a,b)=>a.record.server_sequence-b.record.server_sequence)
  if(chain.length<2)continue
  if(chain.some(r=>r.record.strategy!=='choose_version'||r.record.retained_note_id!==null||r.record.transport_kind!=='content_note_resolution_v2'||r.record.note_id!==chain[0]!.record.note_id))throw new BrowserPersistenceError('unsupported_local_source')
  const latest=chain.at(-1)!.record
  const bytes=await openHistoricalJson(s,latest.project_id,latest.note_id,'note',latest.evidence_envelope,amk,current)
  let evidence:{head_before:unknown;source_before:{project:LocalProjectRecord;notes:LocalNoteRecord[]}}
  try{evidence=JSON.parse(new TextDecoder('utf-8',{fatal:true}).decode(bytes))}finally{bytes.fill(0)}
  // Reconstruct the exact pre-commit snapshot from authenticated owner evidence.
  // Never mint a head/source/proof to make the earlier generation validate.
  const prior={...v,
   history:[...v.history.filter((raw:any)=>{
    if(raw.project_id!==latest.project_id||raw.note_id!==latest.note_id)return true
    if(raw.record_kind==='resolution'&&raw.event_id===latest.event_id||raw.record_kind==='group_transition'&&raw.resolution_event_id===latest.event_id||raw.record_kind==='head')return false
    if(raw.record_kind==='candidate'&&history.rows.some(r=>r.event_id===raw.event_id&&r.server_sequence>=latest.server_sequence))return false
    if(raw.record_kind==='group'&&raw.members.some((m:any)=>m.server_sequence>=latest.server_sequence))return false
    return true
   }),evidence.head_before],
   rows:v.rows.map((raw:any)=>{
    const e=JSON.parse(raw.wire_item).event
    if(raw.server_sequence<latest.server_sequence||e.project_id!==latest.project_id||e.entity_id!==latest.note_id)return raw
    return{record_version:1,state:'received',account_id:raw.account_id,device_id:raw.device_id,event_id:raw.event_id,server_sequence:raw.server_sequence,protocol_version:raw.protocol_version,wire_item:raw.wire_item}
   }),
   projects:v.projects.map((raw:any)=>raw.project_id===latest.project_id?evidence.source_before.project:raw),
   localNotes:[...(v.localNotes??[]).filter((raw:any)=>raw.project_id!==latest.project_id),...evidence.source_before.notes]}
  // The full history already authenticated these immutable leaves. Reuse them
  // in the historical view instead of decrypting every older generation again.
  const dag=inspectResolutionDag(s,prior.history)
  const priorView={...history,dag,rows:prior.rows.map(r=>readFutureInboxRecord(r,s)),
   resolutions:new Map([...history.resolutions].filter(([id])=>dag.events.get(id)?.record_kind==='resolution')),
   opened:new Map([...history.opened].filter(([id])=>dag.events.get(id)?.record_kind==='candidate')),
   groups:dag.history.filter((g):g is BrowserNoteConflictGroup=>g.record_kind==='group').map(group=>({group,
    effective:dag.history.some(t=>t.record_kind==='group_transition'&&t.group_id===group.group_id&&t.project_id===group.project_id&&t.note_id===group.note_id&&t.bootstrap_id===group.bootstrap_id)?'resolved' as const:'open' as const}))}
  await verifyContinuationSources(s,prior,priorView,amk,current)
  for(const earlier of chain.slice(0,-1))superseded.add(earlier.record.event_id)
 }
 const projects=new Set<string>()
 for(const root of history.resolutions.values()){
  if(superseded.has(root.record.event_id))continue
  const r=root.record
  // Historical generations were verified against their authenticated pre-commit
  // snapshots above. Only the latest root supplies current-source authority.
  if(projects.has(r.project_id))throw new BrowserPersistenceError('unsupported_local_source')
  projects.add(r.project_id)
  const project=validateLocalProjectRecord(v.projects.find((p:any)=>p.project_id===r.project_id),s)
  if(project.deleted)bad()
  let bytes=await openHistoricalJson(s,r.project_id,r.project_id,'project_metadata',project.envelope,amk,current)
  let p:LocalProjectPayload
  try{const raw=new TextDecoder('utf-8',{fatal:true}).decode(bytes);p=JSON.parse(raw)
   if(canonical(p)!==raw||!localExact(p,['local_format','kind','account_id','device_id','project_id','source_revision','deleted_at','project','member_ids'])||p.local_format!==1||p.kind!=='project'||p.account_id!==s.accountId||p.device_id!==s.deviceId||p.project_id!==r.project_id||p.source_revision!==project.source_revision||p.deleted_at!==null||!same(validateLocalProject(p.project),root.project.project)||!same(p.member_ids,root.project.member_ids))bad()
  }finally{bytes.fill(0)}
  const notes=(v.localNotes??[]).filter((n:any)=>n.project_id===r.project_id).map(n=>validateLocalNoteRecord(n,s,r.project_id)).sort((a,b)=>a.note_id<b.note_id?-1:1)
  if(notes.length>MAX_LOCAL_NOTES||!same(notes.map(n=>n.note_id),p.member_ids))bad()
  for(const n of notes){
   bytes=await openHistoricalJson(s,r.project_id,n.note_id,'note',n.envelope,amk,current)
   try{const raw=new TextDecoder('utf-8',{fatal:true}).decode(bytes),a=JSON.parse(raw)
    if(canonical(a)!==raw||!localExact(a,['local_format','kind','account_id','device_id','project_id','note_id','source_revision','source'])||a.local_format!==1||a.kind!=='note'||a.account_id!==s.accountId||a.device_id!==s.deviceId||a.project_id!==r.project_id||a.note_id!==n.note_id||a.source_revision!==n.source_revision||n.source_revision>p.source_revision)bad()
    const source=validatePersistedLocalNote(a.source,r.project_id)
    if(source.note.id!==n.note_id||(source.state==='tombstone')!==n.deleted||(source.state==='live'?source.note.revision:source.revision)!==n.note_revision)bad()
   }finally{bytes.fill(0)}
  }
  const sourceRevisions:number[]=[]
  for(const noteId of [r.note_id,...(r.retained_note_id?[r.retained_note_id]:[])]){
   const head=history.dag.history.find(h=>h.record_kind==='head'&&h.project_id===r.project_id&&h.note_id===noteId&&h.bootstrap_id===r.bootstrap_id)
   if(!head||head.record_kind!=='head'||head.record_version!==2)bad()
   const reaches=(event:string):boolean=>{
    const a=history.dag.events.get(event)
    if(a?.record_kind==='resolution')return a.event_id===r.event_id&&(noteId===a.note_id||noteId===a.retained_note_id)
    return !!a&&a.project_id===r.project_id&&a.note_id===noteId&&a.bootstrap_id===r.bootstrap_id&&!!a.header.parent_event_id&&reaches(a.header.parent_event_id)
   }
   if(!reaches(head.linear_head_event_id))bad()
   const following=[...history.dag.events.values()].filter((a):a is BrowserNoteCandidate=>a.record_kind==='candidate'&&a.project_id===r.project_id&&a.note_id===noteId&&reaches(a.event_id))
   const expectedRevision=(noteId===r.note_id?root.headBefore.head_revision+1:1)+following.length
   if(head.head_revision!==expectedRevision)bad()
   for(const c of following){
    if(c.origin!=='peer'&&c.record_version!==3)throw new BrowserPersistenceError('unsupported_local_source')
    const row=history.rows.find(row=>row.event_id===c.event_id),parent=history.rows.find(row=>row.event_id===c.header.parent_event_id)
    if(!parent||row&&row.server_sequence<=parent.server_sequence||!row&&c.record_version!==3)bad()
    const parentCandidate=history.dag.events.get(c.header.parent_event_id!)
    const parentDeleted=parentCandidate?.record_kind==='resolution'?parentCandidate.result_operation==='delete':parentCandidate?.header.operation==='delete'
    if(parentDeleted&&c.header.operation!=='delete'&&!(c.record_version===2&&parentCandidate?.record_kind==='resolution'&&parentCandidate.result_operation==='delete'))throw new BrowserPersistenceError('unsupported_local_source')
    if(c.record_version===3){if(c.intent.mutation_generation!==1||c.intent.snapshot.source_revision<=root.project.source_revision)bad();sourceRevisions.push(c.intent.snapshot.source_revision)}
    if(c.projection){if(c.projection.note_revision!==0||c.projection.source_revision<=root.project.source_revision)bad();sourceRevisions.push(c.projection.source_revision)}
   }
   const last=history.dag.events.get(head.linear_head_event_id),expected=last?.record_kind==='candidate'?(last.record_version===3?last.intent.snapshot:last.projection):root.notes.find(n=>n.note_id===noteId)
   const intent=(v.intents??[]).map(i=>validateNoteIntentRecord(i,s)).find(i=>i.record_version===2&&i.project_id===r.project_id&&i.note_id===noteId)
   if(intent){if(intent.mutation_generation!==1)bad();sourceRevisions.push(intent.snapshot.source_revision)}
   if(!expected||!same(notes.find(n=>n.note_id===noteId),intent?.snapshot??expected))bad()
  }
  sourceRevisions.sort((a,b)=>a-b)
  if(sourceRevisions.some((n,i)=>n!==root.project.source_revision+i+1)||p.source_revision!==root.project.source_revision+sourceRevisions.length)bad()
 }
 if(!current())throw new BrowserPersistenceError('closed')
}
