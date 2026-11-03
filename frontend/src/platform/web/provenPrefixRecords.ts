import { contentNoteSequences,type VerifiedContentNotePrefix } from './contentNotePrefix'
import {stageOrderSequences,type VerifiedStageOrderPrefix} from './stageOrderApply'
import { stageSequences,type VerifiedStagePrefix } from './stageApply'
import { verifiedMetadataSequences, type VerifiedMetadataPrefix } from './metadataPrefixProof'
import { deriveInitialLegacyCohort } from './initialNoteCapture'
import { BrowserPersistenceError } from './persistence'
import { inspectBrowserAckInventory } from './ackPrerequisites'
import { validateBrowserInboxRecord,immutableWireIdentity,validateBrowserUploadPair,type BrowserSyncScope,type BrowserSyncCheckpoint } from './syncDurability'
import { validateNoteHistorySet,type BrowserNoteCandidate } from './noteHistoryRecords'
import { validateBrowserProjectBinding } from './projectBindings'
export const PREFIX_STORES=['inbox','outbox','receipts','cursors','projectBindings','ownerHeads','legacyNoteHistory']
export interface BrowserPrefixInventory {rows:unknown[];out:unknown[];receipts:unknown[];bindings:unknown[];owners:unknown[];history:unknown[];intents?:unknown[];localNotes?:unknown[];cursor:BrowserSyncCheckpoint;projects?:unknown[]}
export type PrefixBlocker='missing_sequence'|'received_note'|'metadata_proof_gap'|'unsupported_family'|null
export interface BrowserPrefixResult {current_proven_prefix:number;candidate_proven_prefix:number;pull_cursor:number;first_blocking_sequence:number|null;blocker:PrefixBlocker;unsupported_family:string|null;counts:{applied:number;self_echo:number;conflict:number;unsealed_conflict:number}}
const same=(a:unknown,b:unknown)=>immutableWireIdentity(a)===immutableWireIdentity(b)
const bad=():never=>{throw new BrowserPersistenceError('corrupt_record')}
/** Pure structural calculation. Runtime authenticates these exact immutable rows before any commit. */
export function calculateBrowserPrefix(scope:BrowserSyncScope,v:BrowserPrefixInventory,metadata?:VerifiedMetadataPrefix,stage?:VerifiedStagePrefix,order?:VerifiedStageOrderPrefix,content?:VerifiedContentNotePrefix):BrowserPrefixResult{
 const {cursor}=v,rows=v.rows.map(r=>validateBrowserInboxRecord(r,scope)).sort((a,b)=>a.server_sequence-b.server_sequence),bindings=v.bindings.map(b=>validateBrowserProjectBinding(b,scope)),history=validateNoteHistorySet(scope,v.history)
 for(const b of bindings)if(b.mode==='upload_existing'&&b.phase==='completing'&&b.remote_state==='active')deriveInitialLegacyCohort(scope,b,v.out,v.receipts)
 inspectBrowserAckInventory(scope,cursor,v.rows,v.owners,bindings,v.history,metadata?{token:metadata,inventory:v}:undefined,stage?{token:stage,inventory:v}:undefined,order?{token:order,inventory:v}:undefined,content?{token:content,inventory:v}:undefined)
 const qualified=new Set<number>([...verifiedMetadataSequences(metadata,scope,v),...stageSequences(stage,scope,v),...stageOrderSequences(order,scope,v),...contentNoteSequences(content,scope,v)]),counts={applied:0,self_echo:0,conflict:0,unsealed_conflict:0}
 for(const r of rows){if(r.state==='note_proven'||r.state==='note_conflict_preserved'){
   const p=r.state==='note_proven'?r.note_proof:r.note_conflict_proof,c=history.find((h):h is BrowserNoteCandidate=>h.record_kind==='candidate'&&h.event_id===r.event_id),wire=JSON.parse(r.wire_item),{device_id:source,server_sequence:_seq,...event}=wire.event
   if(!c||c.bootstrap_id!==p.bootstrap_id||c.project_id!==p.header.project_id||c.note_id!==p.header.entity_id||c.source_device_id!==source||c.protocol_version!==r.protocol_version||!same(c.header,p.header)||!same(JSON.parse(c.wire_item),{event,object:wire.object}))bad()
   if(r.state==='note_proven'&&r.note_proof.outcome==='self_echo_applied'){const out=v.out.find(o=>(o as {event_id:string}).event_id===r.event_id),receipt=v.receipts.find(p=>(p as {event_id:string}).event_id===r.event_id);if(!out||!receipt)bad();const o=validateBrowserUploadPair(out,receipt,scope);if(o.state!=='accepted'||o.protocol_version!==r.protocol_version||(receipt as {server_sequence:number}).server_sequence!==r.server_sequence||!same(JSON.parse(o.raw_body).items[0],{event,object:wire.object}))bad()}
   if(r.state==='note_proven'&&r.note_proof.outcome==='applied'&&c!.projection_digest!==r.note_proof.local_note_digest)bad()
   qualified.add(r.server_sequence)
 }}
 const result:BrowserPrefixResult={current_proven_prefix:cursor.proven_prefix,candidate_proven_prefix:cursor.proven_prefix,pull_cursor:cursor.pull_cursor,first_blocking_sequence:null,blocker:null,unsupported_family:null,counts}
 for(let n=1;n<=cursor.pull_cursor;n++){const r=rows.find(r=>r.server_sequence===n);if(!r||!qualified.has(n)){if(n<=cursor.proven_prefix)bad();result.first_blocking_sequence=n;const event=r&&JSON.parse(r.wire_item).event;result.blocker=!r?'missing_sequence':event.entity_type==='project_metadata'?'metadata_proof_gap':event.entity_type==='note'&&['upsert','delete'].includes(event.operation)?'received_note':'unsupported_family';if(result.blocker==='unsupported_family')result.unsupported_family=event.entity_type;break}
   if(r.state==='content_note_conflict_preserved'){if(r.content_note_conflict_proof.record_version===2)counts.unsealed_conflict++;else counts.conflict++;}else if(r.state==='content_note_proven')counts.applied++;else if(r.state==='note_proven'){if(r.note_proof.outcome==='applied')counts.applied++;else counts.self_echo++}else if(r.state==='note_conflict_preserved'){if(r.note_conflict_proof.record_version===2)counts.unsealed_conflict++;else counts.conflict++}
   if(n>result.candidate_proven_prefix)result.candidate_proven_prefix=n
 }
 return result
}
