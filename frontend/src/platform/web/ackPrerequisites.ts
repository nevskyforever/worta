import {contentConflictHistoryJoin} from './contentNoteConflictProof'
import {contentNoteSequences,type VerifiedContentNotePrefix} from './contentNotePrefix'
import {contentHistoryJoin} from './contentNoteProof'
import {stageOrderSequences,type VerifiedStageOrderPrefix} from './stageOrderApply'
import {stageSequences,type VerifiedStagePrefix} from './stageApply'
import { verifiedMetadataSequences, type VerifiedMetadataPrefix } from './metadataPrefixProof'
import type { BrowserPrefixInventory } from './provenPrefixRecords'
import { validateNoteHistorySet, type BrowserNoteCandidate, type BrowserNoteConflictGroup } from './noteHistoryRecords'
import { BrowserPersistenceError } from './persistence'
import { validateBrowserInboxRecord, immutableWireIdentity, type BrowserSyncScope, type BrowserSyncCheckpoint } from './syncDurability'
import { validateBrowserOwnerHead } from './stageRecords'
import { validateBrowserProjectBinding, type BrowserProjectBinding } from './projectBindings'

export type BrowserAckPrerequisiteCode = 'registry_reconciliation_required' | 'bootstrap_completion_unavailable' | 'pull_required' | 'protected_apply_required' | 'unsupported_family_proof' | 'ack_proof_unavailable' | 'ack_transport_unavailable'
export interface BrowserAckInventory {
  content_note_applied_verified?:number; content_note_authority_only?:number; stage_order_applied_verified?:number; stage_order_authority_only?:number; stage_applied_verified?:number; stage_authority_only?:number; metadata_applied_verified?:number; note_conflict_preserved:number; note_received_only: number; note_applied: number; note_self_echo: number; received_only: number; metadata_authority_only: number; missing_next_sequence: boolean
  unsupported_families: string[]; first_unproven_sequence: number | null; proven_prefix: number
}
export interface BrowserAckPrerequisites {
  ready: false; ack_eligible: false; code: BrowserAckPrerequisiteCode; required_cursor: number
  pull_cursor: number; ack_cursor: number; proven_prefix: number; inventory: BrowserAckInventory
}
function corrupt(): never { throw new BrowserPersistenceError('corrupt_record') }
/** Exact durable record inventory. Metadata authority is NOT a general native apply/ACK ledger. */
export function inspectBrowserAckInventory(scope: BrowserSyncScope, cursor: BrowserSyncCheckpoint, inbound: unknown[], heads: unknown[], bindings: BrowserProjectBinding[],history:unknown[]=[],metadata?:{token:VerifiedMetadataPrefix;inventory:BrowserPrefixInventory},stage?:{token:VerifiedStagePrefix;inventory:BrowserPrefixInventory},order?:{token:VerifiedStageOrderPrefix;inventory:BrowserPrefixInventory},content?:{token:VerifiedContentNotePrefix;inventory:BrowserPrefixInventory}): BrowserAckInventory {
  if (!Number.isSafeInteger(cursor.ack_cursor)||cursor.ack_cursor<0||!Number.isSafeInteger(cursor.proven_prefix)||cursor.proven_prefix<cursor.ack_cursor||cursor.proven_prefix>cursor.pull_cursor|| !Number.isSafeInteger(cursor.pull_cursor) || cursor.pull_cursor < 0) corrupt()
  const rows = inbound.map(v => validateBrowserInboxRecord(v,scope)), sequences = new Set<number>(), events = new Set<string>()
  const certified=metadata?verifiedMetadataSequences(metadata.token,scope,metadata.inventory):new Set<number>()
  const stageCertified=stage?stageSequences(stage.token,scope,stage.inventory):new Set<number>()
  const orderCertified=order?stageOrderSequences(order.token,scope,order.inventory):new Set<number>()
  const contentCertified=content?contentNoteSequences(content.token,scope,content.inventory):new Set<number>()
  const noteHistory=validateNoteHistorySet(scope,history)
  const owners = heads.map(v => validateBrowserOwnerHead(v,scope))
  if (new Set(owners.map(h=>JSON.stringify([h.owner_kind,h.owner_id]))).size !== owners.length) corrupt()
  const inventory: BrowserAckInventory = {note_conflict_preserved:0,note_received_only:0,note_applied:0,note_self_echo:0,received_only:0,metadata_authority_only:0,missing_next_sequence:cursor.pull_cursor>cursor.proven_prefix&&!rows.some(r=>r.server_sequence===cursor.proven_prefix+1),unsupported_families:[],first_unproven_sequence:cursor.pull_cursor>cursor.proven_prefix?cursor.proven_prefix+1:null,proven_prefix:cursor.proven_prefix}
  const families = new Set<string>()
  for (const r of rows) {
    if (r.server_sequence > cursor.pull_cursor || sequences.has(r.server_sequence) || events.has(r.event_id)) corrupt()
    sequences.add(r.server_sequence);events.add(r.event_id)
    const event = JSON.parse(r.wire_item).event
    if (r.state === 'received') { inventory.received_only++; if(event.entity_type==='note'&&['upsert','delete'].includes(event.operation))inventory.note_received_only++ }
    else if(r.state==='content_note_self_echo_proven'){inventory.received_only++;inventory.note_received_only++}
    else if(r.state==='content_note_conflict_preserved'){const p=r.content_note_conflict_proof,b=bindings.find(b=>b.project_id===p.header.project_id);if(!b||b.mode!=='import_remote'||b.phase!=='captured'||b.remote_state!=='active'||b.bootstrap_id!==p.bootstrap_id)corrupt();contentConflictHistoryJoin(scope,r,noteHistory);if(contentCertified.has(r.server_sequence))inventory.note_conflict_preserved++}
    else if(r.state==='content_note_proven'){const b=bindings.find(b=>b.project_id===r.content_note_proof.header.project_id);if(!b||b.mode!=='import_remote'||b.phase!=='captured'||b.remote_state!=='active'||b.bootstrap_id!==r.content_note_proof.bootstrap_id)corrupt();contentHistoryJoin(scope,r,noteHistory);if(contentCertified.has(r.server_sequence))inventory.content_note_applied_verified=(inventory.content_note_applied_verified??0)+1;else inventory.content_note_authority_only=(inventory.content_note_authority_only??0)+1}
    else if(r.state==='note_proven'){const binding=bindings.find(b=>b.project_id===r.note_proof.header.project_id);if(!binding)corrupt();const lineage=validateBrowserProjectBinding(binding,scope);if(lineage.bootstrap_id!==r.note_proof.bootstrap_id||lineage.remote_state!=='active'||!(lineage.mode==='import_remote'&&lineage.phase==='captured'||lineage.mode==='upload_existing'&&lineage.phase==='completing'))corrupt();if(r.note_proof.outcome==='applied')inventory.note_applied++;else inventory.note_self_echo++}
    else if(r.state==='note_conflict_preserved'){
      const p=r.note_conflict_proof,b=bindings.find(b=>b.project_id===p.header.project_id),g=noteHistory.find((h):h is BrowserNoteConflictGroup=>h.record_kind==='group'&&h.group_id===p.group_id),c=noteHistory.find((h):h is BrowserNoteCandidate=>h.record_kind==='candidate'&&h.event_id===r.event_id),m=g?.members.find(m=>m.event_id===r.event_id)
      if(!b||b.bootstrap_id!==p.bootstrap_id||b.remote_state!=='active'||!g||g.bootstrap_id!==b.bootstrap_id||g.project_id!==p.header.project_id||g.note_id!==p.header.entity_id||!c||c.origin!=='peer'||!m||m.source!=='remote'||m.server_sequence!==r.server_sequence||m.conflict_generation!==p.conflict_generation||immutableWireIdentity(c.header)!==immutableWireIdentity(p.header))corrupt()
      const local=g.members.find(m=>m.source==='local_unsealed');if(local){if(p.record_version!==2||p.local_version_digest!==(noteHistory.find(v=>v.record_kind==='local_version'&&v.version_id===local.version_id) as {record_digest:string}|undefined)?.record_digest||p.local_version_id!==local.version_id||p.local_mutation_generation!==(local as {local_mutation_generation:number}).local_mutation_generation)corrupt()}else if(p.record_version!==1)corrupt()
      for(const member of g.members){if(member.source==='local_unsealed'){const frozen=noteHistory.find(v=>v.record_kind==='local_version'&&v.version_id===member.version_id);if(!frozen)corrupt();continue}const incoming=rows.find(r=>r.event_id===member.event_id);if(!incoming||incoming.server_sequence!==member.server_sequence)corrupt();if(member.source==='remote_applied'){if(incoming.state!=='note_proven'&&incoming.state!=='content_note_proven')corrupt()}else {const proof=incoming.state==='note_conflict_preserved'?incoming.note_conflict_proof:incoming.state==='content_note_conflict_preserved'?incoming.content_note_conflict_proof:null;if(!proof||proof.group_id!==g.group_id||proof.conflict_generation!==member.conflict_generation)corrupt()}}
      inventory.note_conflict_preserved++
    }
    else if(r.state==='stage_order_proven'){const p=r.stage_order_proof,h=owners.find(h=>h.owner_kind==='stage_order'&&h.project_id===p.project_id),b=bindings.find(b=>b.project_id===p.project_id);if(!h||!b||b.mode!=='import_remote'||b.phase!=='captured'||b.remote_state!=='active'||b.bootstrap_id!==p.bootstrap_id||!h.history.some(v=>immutableWireIdentity(v)===immutableWireIdentity(p)))corrupt();if(orderCertified.has(r.server_sequence))inventory.stage_order_applied_verified=(inventory.stage_order_applied_verified??0)+1;else inventory.stage_order_authority_only=(inventory.stage_order_authority_only??0)+1}
    else if(r.state==='stage_proven'){
      const p=r.stage_proof,h=owners.find(h=>h.owner_kind==='stage'&&h.project_id===p.project_id&&h.stage_id===p.stage_id),b=bindings.find(b=>b.project_id===p.project_id)
      if(!h||!b||b.mode!=='import_remote'||b.phase!=='captured'||b.remote_state!=='active'||b.bootstrap_id!==p.bootstrap_id||!h.history.some(v=>immutableWireIdentity(v)===immutableWireIdentity(p)))corrupt()
      if(stageCertified.has(r.server_sequence))inventory.stage_applied_verified=(inventory.stage_applied_verified??0)+1;else inventory.stage_authority_only=(inventory.stage_authority_only??0)+1
    }
    else if(r.state==='document_proven'){corrupt()} // Document apply does not grant ACK authority.
    else {
      const proof = r.applied_proof, head = owners.find(h=>h.owner_kind==='project_metadata'&&h.owner_id===proof.project_id), binding = bindings.find(b=>b.project_id===proof.project_id)
      if (!head || !binding) corrupt()
      const lineage = validateBrowserProjectBinding(binding,scope)
      if (lineage.mode !== 'import_remote' || lineage.phase !== 'captured' || lineage.remote_state !== 'active' || head.bootstrap_id!==lineage.bootstrap_id || !head.history.some(p=>immutableWireIdentity(p)===immutableWireIdentity(proof))) corrupt()
      if(certified.has(r.server_sequence))inventory.metadata_applied_verified=(inventory.metadata_applied_verified??0)+1;else inventory.metadata_authority_only++
    }
    if (event.entity_type !== 'project_metadata' && r.state!=='note_proven'&&r.state!=='note_conflict_preserved'&&!stageCertified.has(r.server_sequence)&&!orderCertified.has(r.server_sequence)&&!contentCertified.has(r.server_sequence)) families.add(event.entity_type)
  }
  for (const h of owners) for (const p of h.history) if (!rows.some(r=>r.event_id===p.event_id && (h.owner_kind==='stage_order'?r.state==='stage_order_proven'&&immutableWireIdentity(r.stage_order_proof)===immutableWireIdentity(p):h.owner_kind==='stage'?r.state==='stage_proven'&&immutableWireIdentity(r.stage_proof)===immutableWireIdentity(p):r.state==='applied'&&immutableWireIdentity(r.applied_proof)===immutableWireIdentity(p)))) corrupt()
  inventory.unsupported_families=[...families].sort()
  // Structural inventory only; explicit authenticated prefix runtime performs aggregation. Metadata
  // and unsupported families still lack native ledgers. Note outcomes alone are not ACK.
  return inventory
}
export function browserAckPrerequisites(binding: BrowserProjectBinding, cursor: BrowserSyncCheckpoint, inventory: BrowserAckInventory, registryCurrent: boolean): BrowserAckPrerequisites {
  const required_cursor=Math.max(binding.remote_high_water??0,binding.initial_max_server_sequence??0)
  let code:BrowserAckPrerequisiteCode='registry_reconciliation_required'
  if(registryCurrent){
    if(binding.remote_state!=='active'||binding.mode==='upload_existing'&&binding.phase!=='completing')code='bootstrap_completion_unavailable'
    else if(cursor.pull_cursor<required_cursor)code='pull_required'
    else if(inventory.missing_next_sequence)code='protected_apply_required'
    else if(inventory.unsupported_families.length)code='unsupported_family_proof'
    else if(inventory.received_only||inventory.metadata_authority_only)code='protected_apply_required'
    else code=cursor.proven_prefix>cursor.ack_cursor?'ack_transport_unavailable':'ack_proof_unavailable'
  }
  return {ready:false,ack_eligible:false,code,required_cursor,pull_cursor:cursor.pull_cursor,ack_cursor:cursor.ack_cursor,proven_prefix:cursor.proven_prefix,inventory}
}
