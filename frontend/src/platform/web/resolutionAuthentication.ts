import {requireOwnResolutionPublication} from './resolutionPublication'
import {authenticateTerminalMetadataInventory,authenticateMetadataHandoffInventory,type MetadataHandoffInspection} from './historicalMetadataDependency'
import type {ResolutionInventorySnapshot} from './resolutionPlanner'
import { decryptObjectBytes, type AccountMasterKey } from '@/crypto'
import { decodeBase64Url } from '@/api/base64url'
import { canonical } from '@/cloud/projectMetadataCodec'
import { unframeContentNote, type ContentNoteEvent } from '@/cloud/contentNoteCodec'
import { decodeNoteSyncResolutionV2, type NoteSyncResolutionV2 } from '@/cloud/noteSyncResolutionV2Codec'
import { type NoteSyncRecord, type NoteSyncTombstone } from '@/cloud/noteSyncCodec'
import type { NormalUserAuthRuntime } from '@/auth/userAuth'
import type { AuthoritativeAccountBinding } from '@/auth/accountBinding'
import type { RuntimeKeyContext } from '@/auth/keyContext'
import { BrowserPersistenceError } from './persistence'
import { localExact, localId, MAX_LOCAL_NOTES, validateLocalNote, validateLocalProjectRecord, validateLocalNoteRecord, validateLocalProject, type LocalProjectRecord, type LocalNoteRecord, type LocalEnvelope, type LocalProjectPayload, type LocalNotePayload } from './localAuthorityRecords'
import { openContentIntentSource } from './contentAuthorityAuthentication'
import { openHistoryCandidate } from './noteHistory'
import { openFrozenLocalVersion } from './noteUnsealedVersion'
import { noteProofDigest, noteOutcomeDigest, verifyStoredNoteProof } from './legacyNoteApply'
import { validateBrowserUploadPair, snapshotBrowserSyncScope, type BrowserSyncScope } from './syncDurability'
import { validateBrowserProjectBinding } from './projectBindings'
import { prepareMetadataPrefix, verifiedMetadataSequences } from './metadataPrefixProof'
import { validateBrowserMetadataHead } from './metadataApplyRecords'
import { prepareStagePrefix, stageSequences } from './stageApply'
import { validateStageHead } from './stageRecords'
import { proveStageReferenceCoverage } from './stageOrderRecords'
import type { BrowserPrefixInventory } from './provenPrefixRecords'
import type { BrowserNoteCandidate, BrowserNoteConflictGroup } from './noteHistoryRecords'
import { inspectResolutionDag } from './resolutionDag'
import { rejectLegacy3ResolutionRoots, resolutionBad as bad, resolutionSame as same, resolutionIdentity, readFutureInboxRecord, readFutureHistoryRecord, boundResolutionJson, MAX_RESOLUTION_EVIDENCE_BYTES, type BrowserResolution4 } from './resolutionRecords'

export interface ResolutionInventory extends BrowserPrefixInventory { history: unknown[] }
interface ParentWitness { event_id:string; revision:number; operation:'upsert'|'delete'; history_digest:string; inbox_digest:string; publication_source:'remote'|'remote_applied'|'local_accepted'; server_sequence:number; local_mutation_generation:number|null; frozen_version_digest:string|null; snapshot_digest:string }
interface SourceWitness { project:LocalProjectRecord; notes:LocalNoteRecord[] }
interface Evidence { resolution_format:1; kind:'resolution_apply'; identity:ReturnType<typeof resolutionIdentity>; authenticated_payload:{wire_frame_digest:string;wrapper_payload_digest:string|null;resolution_payload_digest:string}; parents:ParentWitness[]; group_snapshot:BrowserNoteConflictGroup; head_before:unknown; source_before:SourceWitness; source_after:SourceWitness }
export async function resolutionBytesDigest(bytes:Uint8Array){const copy=Uint8Array.from(bytes);let digest:Uint8Array|undefined;try{digest=new Uint8Array(await crypto.subtle.digest('SHA-256',copy));return Array.from(digest,b=>b.toString(16).padStart(2,'0')).join('')}finally{copy.fill(0);digest?.fill(0)}}
const legacyEligible=(n:NoteSyncRecord|NoteSyncTombstone)=>n.stage_id===null&&n.source_type==='project'&&n.source_map_id===null&&n.source_node_id===null&&n.content_format==='html'
const check=(current:()=>boolean)=>{if(!current())throw new BrowserPersistenceError('closed')}
const exact=(v:unknown,fields:string[])=>{if(!localExact(v,fields))bad()}
async function openBytes(s:BrowserSyncScope,project:string,id:string,type:'note'|'project_metadata',e:LocalEnvelope,amk:AccountMasterKey,current:()=>boolean){
  check(current);const nonce=decodeBase64Url(e.nonce),ciphertext=decodeBase64Url(e.ciphertext);let bytes:Uint8Array|undefined
  try{bytes=await decryptObjectBytes(amk,{userId:s.canonicalUserId,projectId:project,entityId:id,entityType:type},{...e,nonce,ciphertext});check(current);return bytes.slice()}finally{nonce.fill(0);ciphertext.fill(0);bytes?.fill(0)}
}
function parseCanonical(bytes:Uint8Array):unknown {const raw=new TextDecoder('utf-8',{fatal:true}).decode(bytes),v:unknown=JSON.parse(raw);boundResolutionJson(v,MAX_RESOLUTION_EVIDENCE_BYTES);if(canonical(v)!==raw)bad();return v}
async function digestRecord(r:unknown,field='record_digest'){const body={...r as Record<string,unknown>};delete body[field];return noteProofDigest(body)}
function wireNote(note:NoteSyncRecord|NoteSyncTombstone|Record<string,unknown>){const value={...note};delete (value as Record<string,unknown>).revision;return value}
function route(note:NoteSyncRecord|NoteSyncTombstone){return [note.project_id,note.stage_id,note.source_type,note.source_map_id,note.source_node_id,note.content_format]}
async function openSource(s:BrowserSyncScope,row:LocalNoteRecord,content:boolean,amk:AccountMasterKey,current:()=>boolean){
  if(content)return openContentIntentSource(s,row,amk,current)
  validateLocalNoteRecord(row,s,row.project_id)
  const bytes=await openBytes(s,row.project_id,row.note_id,'note',row.envelope,amk,current)
  try{const value=parseCanonical(bytes);exact(value,['local_format','kind','account_id','device_id','project_id','note_id','source_revision','source']);const p=value as LocalNotePayload
    if(p.local_format!==1||p.kind!=='note'||p.account_id!==s.accountId||p.device_id!==s.deviceId||p.project_id!==row.project_id||p.note_id!==row.note_id||p.source_revision!==row.source_revision)bad()
    const source=validateLocalNote(p.source,row.project_id)
    if(source.note.id!==row.note_id||(source.state==='tombstone')!==row.deleted||(source.state==='live'?source.note.revision:source.revision)!==row.note_revision)bad()
    return source
  }finally{bytes.fill(0)}
}
async function openProject(s:BrowserSyncScope,raw:unknown,amk:AccountMasterKey,current:()=>boolean){
  const row=validateLocalProjectRecord(raw,s),bytes=await openBytes(s,row.project_id,row.project_id,'project_metadata',row.envelope,amk,current)
  try{const value=parseCanonical(bytes);exact(value,['local_format','kind','account_id','device_id','project_id','source_revision','deleted_at','project','member_ids']);const p=value as LocalProjectPayload
    if(row.deleted||p.local_format!==1||p.kind!=='project'||p.account_id!==s.accountId||p.device_id!==s.deviceId||p.project_id!==row.project_id||p.source_revision!==row.source_revision||p.deleted_at!==null||!Array.isArray(p.member_ids)||p.member_ids.length>MAX_LOCAL_NOTES||p.member_ids.some((id,i)=>typeof id!=='string'||!id||i>0&&id<=p.member_ids[i-1]!))bad()
    for(const id of p.member_ids)localId(id)
    if(validateLocalProject(p.project).id!==row.project_id)bad();return p
  }finally{bytes.fill(0)}
}

/** Fresh dependency crypto only. The legacy validators see solely their own Metadata/Stage rows. */
async function dependencies(s:BrowserSyncScope,r:BrowserResolution4,content:ContentNoteEvent|undefined,v:ResolutionInventory,amk:AccountMasterKey,current:()=>boolean){
  const bindings=v.bindings.map(b=>validateBrowserProjectBinding(b,s)),binding=bindings.find(b=>b.project_id===r.project_id)
  if(!binding||binding.bootstrap_id!==r.bootstrap_id||binding.remote_state!=='active'||!(binding.mode==='import_remote'&&binding.phase==='captured'||binding.mode==='upload_existing'&&binding.phase==='completing'))bad()
  const d=content?.dependencies??null
  if(content){
    if(!d||d.bootstrap_id!==r.bootstrap_id)bad()
    const relevant:BrowserPrefixInventory={...v,rows:v.rows.filter(row=>['project_metadata','stage','stage_order'].includes(JSON.parse((row as {wire_item:string}).wire_item).event.entity_type))}
    const metadata=await prepareMetadataPrefix(s,relevant,amk,current,true),safe=verifiedMetadataSequences(metadata,s,relevant)
    const head=validateBrowserMetadataHead(v.owners.find(h=>(h as {owner_kind:string;owner_id:string}).owner_kind==='project_metadata'&&(h as {owner_id:string}).owner_id===r.project_id),s)
    const dependency=head.history.find(p=>p.event_id===d.metadata_event_id),tip=head.history.find(p=>p.event_id===head.tip_event_ids[0])
    if(head.bootstrap_id!==r.bootstrap_id||!dependency||dependency.operation==='delete'||!safe.has(dependency.server_sequence)||head.tip_event_ids.length!==1||!tip||tip.operation==='delete'||!safe.has(tip.server_sequence))bad()
    const n=content.event.version===1?content.event.note:content.event.result.note
    if(n.stage_id!==null){
      const stage=validateStageHead(v.owners.find(h=>(h as {owner_kind:string;project_id:string;stage_id:string}).owner_kind==='stage'&&(h as {project_id:string}).project_id===r.project_id&&(h as {stage_id:string}).stage_id===n.stage_id),s)
      const cert=await prepareStagePrefix(s,relevant,amk,current,metadata,true),sequences=stageSequences(cert,s,relevant)
      if(stage.bootstrap_id!==r.bootstrap_id||stage.history.some(p=>!sequences.has(p.server_sequence))||!proveStageReferenceCoverage(stage.tip_event_ids,d.stage_event_ids,stage.history))bad()
      for(const raw of relevant.rows){const row=readFutureInboxRecord(raw,s),e=JSON.parse(row.wire_item).event;if(row.server_sequence<r.server_sequence&&e.entity_type==='stage'&&e.entity_id===n.stage_id&&e.project_id===r.project_id&&(!sequences.has(row.server_sequence)||row.state!=='stage_proven'))bad()}
    }else if(d.stage_event_ids.length)bad()
  }
  const row=v.rows.find(row=>(row as {event_id:string}).event_id===r.event_id)
  // Same anchor formula as noteTransportPrerequisite, with the actual Resolution route.
  const transport={...(row as Record<string,unknown>)};delete transport.resolution_proof;transport.record_version=1;transport.state='received'
  // The original dependency commitment is authenticated inside the evidence AEAD.
  // Current owner heads may legitimately have advanced: verify live coverage above,
  // never pretend hashing today's owner inventory reproduces a historical snapshot.
  if(!row||transport.event_id!==r.event_id)bad()
  check(current)
}

/** Read-only authentication; no durable plans, writes, registration or prefix certificates. */
export async function authenticateResolutionHistory(scope:BrowserSyncScope,input:ResolutionInventory,amk:AccountMasterKey,current:()=>boolean,materialLimit=Number.POSITIVE_INFINITY){
  return authenticateResolutionHistoryCore(scope,input,amk,current,materialLimit,dependencies)
}

/** Isolated terminal-delete reader only: full original F checks with the fresh
 * complete .13 internal dependency semantics; no live prerequisite is minted. */
export async function authenticateTerminalMetadataResolutions(scope:BrowserSyncScope,input:ResolutionInventorySnapshot,amk:AccountMasterKey,current:()=>boolean){
  rejectLegacy3ResolutionRoots(input.history)
  return authenticateResolutionHistoryCore(scope,input,amk,current,16*1024*1024,async(s,_r,_content,v,key,live)=>{
    await authenticateTerminalMetadataInventory(s,v as ResolutionInventorySnapshot,key,live)
  })
}

/** Private Metadata prerequisite only; does not mint a live Note prerequisite. */
export async function authenticateMetadataHandoffResolutions(s:BrowserSyncScope,v:ResolutionInventorySnapshot,amk:AccountMasterKey,current:()=>boolean,mode:MetadataHandoffInspection){
 rejectLegacy3ResolutionRoots(v.history)
 return authenticateResolutionHistoryCore(s,v,amk,current,16*1024*1024,async()=>authenticateMetadataHandoffInventory(s,v,amk,current,mode))
}

async function authenticateResolutionHistoryCore(scope:BrowserSyncScope,input:ResolutionInventory,amk:AccountMasterKey,current:()=>boolean,materialLimit:number,verifyDependencies:typeof dependencies){
  const s=snapshotBrowserSyncScope(scope);check(current)
  // Snapshot caller material before the first asynchronous operation.
  const v=structuredClone(input),dag=inspectResolutionDag(s,v.history),rows=v.rows.map(r=>readFutureInboxRecord(r,s)),seq=new Set<number>(),ids=new Set<string>()
  for(const row of rows){if(seq.has(row.server_sequence)||ids.has(row.event_id))bad();seq.add(row.server_sequence);ids.add(row.event_id)}
  const receiptIds=new Set<string>(),receiptSeq=new Map<number,string>()
  for(const receipt of v.receipts){const r=receipt as {event_id:string;server_sequence:number};if(receiptIds.has(r.event_id)||receiptSeq.has(r.server_sequence))bad();receiptIds.add(r.event_id);receiptSeq.set(r.server_sequence,r.event_id);const row=rows.find(row=>row.server_sequence===r.server_sequence);if(row&&row.event_id!==r.event_id)bad()}
  const parsed=new Map<string,Awaited<ReturnType<typeof openHistoryCandidate>>>()
  let materialBytes=0
  for(const c of dag.events.values())if(c.record_kind==='candidate'){
    const value=await openHistoryCandidate(s,c,amk);check(current)
    boundResolutionJson(value,materialLimit-materialBytes)
    const encoded=new TextEncoder().encode(JSON.stringify(value));try{materialBytes+=encoded.length;if(materialBytes>materialLimit)bad()}finally{encoded.fill(0)}
    parsed.set(c.event_id,value)
  }
  const verified=new Map<string,NoteSyncResolutionV2>(),visiting=new Set<string>(),closures=new Set<string>()
  const authenticate=async(r:BrowserResolution4):Promise<NoteSyncResolutionV2>=>{
    const cached=verified.get(r.event_id);if(cached)return cached
    if(visiting.has(r.event_id))bad();visiting.add(r.event_id)
    if(r.source_device_id===s.deviceId)requireOwnResolutionPublication(s,r,v)
    if(await digestRecord(r)!==r.record_digest)bad()
    const row=rows.find(row=>row.event_id===r.event_id)
    if(!row||row.state!=='note_resolution_proven'||row.wire_item!==r.wire_item||row.server_sequence!==r.server_sequence||row.protocol_version!==r.protocol_version)bad()
    const wire=JSON.parse(r.wire_item)
    if(await noteProofDigest({protocol_version:r.protocol_version,item:wire})!==r.wire_digest)bad()
    const bytes=await openBytes(s,r.project_id,r.note_id,'note',wire.object,amk,current)
    let resolution:NoteSyncResolutionV2,content:ContentNoteEvent|undefined,frameDigest:string
    try{frameDigest=await resolutionBytesDigest(bytes);check(current)
      if(r.transport_kind==='content_note_resolution_v2'){content=unframeContentNote(bytes);if(content.event.version!==2||content.account_id!==s.canonicalUserId||content.device_id!==r.source_device_id)bad();resolution=content.event}
      else {resolution=decodeNoteSyncResolutionV2(bytes);if(!legacyEligible(resolution.result.note)||resolution.resolution.strategy==='keep_both'&&!legacyEligible(resolution.resolution.retained_note))bad()}
    }finally{bytes.fill(0)}
    if(!same(resolution.header,r.header)||resolution.resolution.conflict_group_id!==r.remote_group_id||resolution.resolution.conflict_generation!==r.remote_generation||resolution.resolution.strategy!==r.strategy||resolution.result.operation!==r.result_operation||(resolution.resolution.strategy==='keep_both'?resolution.resolution.retained_note.id:null)!==r.retained_note_id||await noteProofDigest(resolution)!==r.payload_digest)bad()
    const evidenceBytes=await openBytes(s,r.project_id,r.note_id,'note',r.evidence_envelope,amk,current)
    let evidence:Evidence
    try{if(evidenceBytes.length>MAX_RESOLUTION_EVIDENCE_BYTES)bad();const e=parseCanonical(evidenceBytes);exact(e,['resolution_format','kind','identity','authenticated_payload','parents','group_snapshot','head_before','source_before','source_after']);evidence=e as Evidence}finally{evidenceBytes.fill(0)}
    const e=evidence
    if(e.resolution_format!==1||e.kind!=='resolution_apply'||!same(e.identity,resolutionIdentity(r)))bad()
    exact(e.authenticated_payload,['wire_frame_digest','wrapper_payload_digest','resolution_payload_digest'])
    if(!same(e.authenticated_payload,{wire_frame_digest:frameDigest,wrapper_payload_digest:content?await noteProofDigest(content):null,resolution_payload_digest:r.payload_digest}))bad()
    await verifyDependencies(s,r,content,v,amk,current)
    const group=readFutureHistoryRecord(e.group_snapshot,s),priorHead=readFutureHistoryRecord(e.head_before,s)
    if(group.record_kind!=='group'||group.group_id!==r.local_group_id||group.conflict_generation!==r.local_generation||group.bootstrap_id!==r.bootstrap_id||group.project_id!==r.project_id||group.note_id!==r.note_id||!same(group.tip_event_ids,r.parent_event_ids)||await noteProofDigest(group)!==r.group_digest||!dag.history.some(h=>same(h,group)))bad()
    if(priorHead.record_kind!=='head'||priorHead.project_id!==r.project_id||priorHead.note_id!==r.note_id||priorHead.bootstrap_id!==r.bootstrap_id||!same(priorHead.tip_event_ids,r.parent_event_ids))bad()
    if(!Array.isArray(e.parents)||e.parents.length!==r.parent_event_ids.length)bad()
    const parents:Array<{id:string;operation:'upsert'|'delete';note:NoteSyncRecord|NoteSyncTombstone;candidate:BrowserNoteCandidate}>=[]
    for(let i=0;i<e.parents.length;i++){
      const p=e.parents[i]!;exact(p,['event_id','revision','operation','history_digest','inbox_digest','publication_source','server_sequence','local_mutation_generation','frozen_version_digest','snapshot_digest'])
      if(p.server_sequence>=r.server_sequence)bad()
      const c=dag.events.get(p.event_id),member=group.members.find(m=>m.event_id===p.event_id)
      if(p.event_id!==r.parent_event_ids[i]||!c||!member||c.record_kind!=='candidate'||c.project_id!==r.project_id||c.note_id!==r.note_id||c.bootstrap_id!==r.bootstrap_id||c.header.parent_event_id!==group.common_parent_event_id||c.header.revision!==group.tip_revision||p.revision!==c.header.revision||p.operation!==c.header.operation||p.history_digest!==c.record_digest)bad()
      // Common ancestors may themselves be Resolution outputs. Authenticate them first,
      // never through this new event's inbox proof or new current-head reachability.
      const ancestor=dag.events.get(c.header.parent_event_id!);if(ancestor?.record_kind==='resolution')await authenticate(ancestor)
      const value=parsed.get(c.event_id)!;if(await noteProofDigest(value.note)!==p.snapshot_digest||!same(route(value.note),route(resolution.result.note)))bad()
      const incoming=rows.find(row=>row.event_id===c.event_id)
      if(member.source==='local_unsealed'){
        const frozen=dag.history.find(h=>h.record_kind==='local_version'&&h.version_id===member.version_id)
        if(!frozen||frozen.record_kind!=='local_version'||p.publication_source!=='local_accepted'||p.local_mutation_generation!==c.mutation_generation||p.frozen_version_digest!==frozen.record_digest||c.record_version!==1||c.origin!=='local_sealed'||c.mutation_generation===null||c.mutation_generation<frozen.intent.mutation_generation)bad()
        await openFrozenLocalVersion(s,frozen,amk);check(current)
        const out=v.out.find(o=>(o as {event_id:string}).event_id===c.event_id),receipt=v.receipts.find(o=>(o as {event_id:string}).event_id===c.event_id)
        if(!out||!receipt)bad();const accepted=validateBrowserUploadPair(out,receipt,s)
        if(accepted.state!=='accepted'||!same(JSON.parse(accepted.raw_body).items[0],JSON.parse(c.wire_item))||p.server_sequence!==(receipt as {server_sequence:number}).server_sequence)bad()
        // Bind the optional retained inbox view explicitly. Receipt/publication,
        // rather than a fabricated self-echo, authenticates a local accepted parent.
      }else{
        if(p.publication_source!==member.source||p.local_mutation_generation!==null||p.frozen_version_digest!==null||!incoming||incoming.server_sequence!==member.server_sequence||p.server_sequence!==incoming.server_sequence)bad()
      }
      if(incoming){const wire=JSON.parse(incoming.wire_item),{device_id,server_sequence:_seq,...event}=wire.event
        if(device_id!==c.source_device_id||!same(c.record_version===2?wire:{event,object:wire.object},JSON.parse(c.wire_item)))bad()}
      if(await noteProofDigest(incoming??null)!==p.inbox_digest)bad()
      if(!incoming){if(member.source!=='local_unsealed')bad()}
      else if(member.source==='local_unsealed'){
        const w=JSON.parse(incoming.wire_item),{device_id:_device,server_sequence:_seq,...event}=w.event
        if(!same({event,object:w.object},JSON.parse(c.wire_item))||incoming.server_sequence!==p.server_sequence)bad()
      }
      else if(incoming.state==='note_proven')await verifyStoredNoteProof(s,amk,incoming)
      else if(incoming.state==='note_conflict_preserved'){
        const proof=incoming.note_conflict_proof
        if(proof.group_id!==group.group_id||proof.conflict_generation!==member.conflict_generation||!same(proof.header,c.header)||await noteOutcomeDigest(proof)!==proof.outcome_digest)bad()
      }else if(incoming.state==='content_note_proven'||incoming.state==='content_note_conflict_preserved'){
        if(c.record_version!==2)bad();const proof=incoming.state==='content_note_proven'?incoming.content_note_proof:incoming.content_note_conflict_proof
        if(proof.candidate_digest!==c.record_digest||proof.wire_digest!==c.wire_digest||proof.logical_digest!==await noteProofDigest(value)||proof.dependency_digest!==c.transport_proof.dependency_digest||!same(proof.header,c.header)||proof.outcome_digest!==await noteOutcomeDigest(proof))bad()
        if(incoming.state==='content_note_conflict_preserved'&&(incoming.content_note_conflict_proof.group_id!==group.group_id||incoming.content_note_conflict_proof.conflict_generation!==member.conflict_generation))bad()
      }else bad()
      if(member.source==='remote_applied'&&incoming?.state!=='note_proven'&&incoming?.state!=='content_note_proven'||member.source==='remote'&&incoming?.state!=='note_conflict_preserved'&&incoming?.state!=='content_note_conflict_preserved')bad()
      parents.push({id:p.event_id,operation:p.operation,note:value.note,candidate:c});check(current)
    }
    const decision=resolution.resolution,selected='selected_event_id'in decision?parents.find(p=>p.id===decision.selected_event_id):undefined
    if(decision.strategy==='choose_version'||decision.strategy==='keep_both'){if(!selected||selected.operation!==resolution.result.operation||!same(selected.note,resolution.result.note))bad()}
    if(decision.strategy==='keep_both'){
      const retained=parents.find(p=>p.id===decision.retained_event_id)
      if(parents.some(p=>p.operation!=='upsert')||!retained||!same({...retained.note,id:decision.retained_note.id,created_at:decision.retained_note.created_at,updated_at:decision.retained_note.updated_at},decision.retained_note))bad()
    }
    for(const source of [e.source_before,e.source_after]){exact(source,['project','notes']);if(!Array.isArray(source.notes)||source.notes.some((n,i)=>i>0&&n.note_id<=source.notes[i-1]!.note_id))bad()}
    if(e.source_before.notes.length!==1||e.source_before.notes[0]?.note_id!==r.note_id||e.source_after.notes.length!==(r.retained_note_id?2:1))bad()
    const before=await openProject(s,e.source_before.project,amk,current),after=await openProject(s,e.source_after.project,amk,current)
    if(before.project_id!==r.project_id||after.project_id!==r.project_id||after.source_revision!==before.source_revision+1||!same(before.project,after.project)||!same(after.member_ids,[...before.member_ids,...(r.retained_note_id?[r.retained_note_id]:[])].sort())||r.retained_note_id!==null&&before.member_ids.includes(r.retained_note_id))bad()
    const old=e.source_before.notes[0]!,working=parents.find(p=>p.candidate.projection&&same(p.candidate.projection,old))
    if(!working)bad()
    const oldSource=await openSource(s,old,working.candidate.record_version===2,amk,current)
    if(old.source_revision>before.source_revision||!working||!before.member_ids.includes(old.note_id)||!same(wireNote(oldSource.note),working.note))bad()
    const original=group.members.find(m=>m.source!=='remote')!
    if(priorHead.linear_head_event_id!==original.event_id)bad()
    for(const raw of e.source_after.notes){const note=validateLocalNoteRecord(raw,s,r.project_id),source=await openSource(s,note,r.transport_kind==='content_note_resolution_v2',amk,current),expected=note.note_id===r.note_id?resolution.result.note:decision.strategy==='keep_both'&&note.note_id===r.retained_note_id?decision.retained_note:undefined
      if(!expected||note.source_revision!==after.source_revision||note.note_revision!==0||!same(wireNote(source.note),expected))bad()
    }
    const transition=dag.history.find(h=>h.record_kind==='group_transition'&&h.group_id===r.local_group_id&&h.project_id===r.project_id&&h.note_id===r.note_id)
    if(!transition||transition.record_kind!=='group_transition'||transition.bootstrap_id!==r.bootstrap_id||transition.from_generation!==r.local_generation||transition.from_digest!==r.group_digest||transition.resolution_event_id!==r.event_id||transition.resolution_digest!==r.record_digest||!same(transition.resolved_parent_ids,r.parent_event_ids)||await digestRecord(transition)!==transition.record_digest)bad()
    const proof=row.resolution_proof,output={original:await noteProofDigest(e.source_after.notes.find(n=>n.note_id===r.note_id)),retained:r.retained_note_id?await noteProofDigest(e.source_after.notes.find(n=>n.note_id===r.retained_note_id)):null}
    if(proof.resolution_digest!==r.record_digest||proof.transition_digest!==transition.record_digest||proof.wire_digest!==r.wire_digest||proof.payload_digest!==r.payload_digest||proof.dependency_digest!==r.dependency_digest||proof.bootstrap_id!==r.bootstrap_id||!same(proof.header,r.header)||!same(proof.output_digests,output)||await digestRecord(proof,'outcome_digest')!==proof.outcome_digest)bad()
    check(current);visiting.delete(r.event_id);verified.set(r.event_id,resolution);closures.add(JSON.stringify([r.project_id,r.note_id,r.local_group_id]));return resolution
  }
  for(const r of dag.events.values())if(r.record_kind==='resolution')await authenticate(r)
  for(const t of dag.history)if(t.record_kind==='group_transition'){
    const r=dag.events.get(t.resolution_event_id)
    if(!r||r.record_kind!=='resolution'||!verified.has(r.event_id)||t.group_id!==r.local_group_id||t.project_id!==r.project_id||t.note_id!==r.note_id||t.bootstrap_id!==r.bootstrap_id||t.from_generation!==r.local_generation||t.from_digest!==r.group_digest||t.resolution_digest!==r.record_digest||!same(t.resolved_parent_ids,r.parent_event_ids)||await digestRecord(t)!==t.record_digest)bad()
  }
  for(const row of rows)if(row.state==='note_resolution_proven'&&!verified.has(row.event_id))bad()
  check(current)
  const open=new Set<string>(),groups=dag.history.filter((r):r is BrowserNoteConflictGroup=>r.record_kind==='group').map(group=>{
    const effective=closures.has(JSON.stringify([group.project_id,group.note_id,group.group_id]))?'resolved' as const:'open' as const
    if(effective==='open'){const key=JSON.stringify([group.project_id,group.note_id,group.bootstrap_id]);if(open.has(key))bad();open.add(key)}
    return {group,effective}
  })
  return {readOnly:true as const,resolutions:verified.size,groups,dag}
}

/** Explicit current-scope wrapper; callers cannot replace real auth/key/profile currentness. */
export class BrowserResolutionHistoryReader {
  constructor(private readonly auth:NormalUserAuthRuntime,private readonly bindings:AuthoritativeAccountBinding,private readonly keys:RuntimeKeyContext,private readonly profileCurrent:()=>boolean){}
  async read(scope:BrowserSyncScope,inventory:ResolutionInventory){
    const s=snapshotBrowserSyncScope(scope);if(!this.profileCurrent())throw new BrowserPersistenceError('closed')
    const {context}=await this.bindings.ensureForCurrentUser(s.accountId),lease=this.keys.leaseForAccount(s.accountId)
    const current=()=>this.profileCurrent()&&this.auth.isCurrent(context)&&!!lease?.isCurrent()
    if(!lease||context.userId!==s.canonicalUserId||lease.localAccountId!==s.accountId||lease.canonicalUserId!==s.canonicalUserId||lease.authEpoch!==context.authEpoch||!current())throw new BrowserPersistenceError('account_mismatch')
    return lease.use(key=>authenticateResolutionHistory(s,inventory,key,current))
  }
}
