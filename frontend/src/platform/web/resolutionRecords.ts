import { validateDocumentInbox, type DocumentInbox } from './documentRecords'
import type { NoteSyncResolutionV2Header, NoteSyncResolutionV2 } from '@/cloud/noteSyncResolutionV2Codec'
import { canonicalizeSyncTimestamp } from '@/cloud/syncTimestamp'
import { decodeBase64Url } from '@/api/base64url'
import { BrowserPersistenceError } from './persistence'
import { localExact, localId, localSafe, type LocalEnvelope } from './localAuthorityRecords'
import { canonicalNoteUuid, noteHistoryHash, validateNoteHistoryRecord, type BrowserNoteHistoryRecord } from './noteHistoryRecords'
import { validateBrowserInboxRecord, immutableWireIdentity, type BrowserSyncScope, type BrowserInboxRecord } from './syncDurability'

// Isolated future-v10 leaves. Nothing in the active v9 persistence imports this module.
export const MAX_RESOLUTION_EVIDENCE_BYTES = 64 * 1024 * 1024
export const MAX_RESOLUTION_RECORD_BYTES = 128 * 1024 * 1024
export function resolutionBad(): never { throw new BrowserPersistenceError('corrupt_record') }
export const resolutionSame = (a: unknown, b: unknown) => immutableWireIdentity(a) === immutableWireIdentity(b)
export type ResolutionTransportKind = 'legacy_resolution_v2' | 'content_note_resolution_v2'
/** Descriptor selects one permitted codec. A failed decoder never tries another. */
export function resolutionTransportKind(protocol: number, operation: unknown): ResolutionTransportKind {
  if ((protocol === 2 || protocol === 3) && operation === 'resolution') return 'legacy_resolution_v2'
  if (protocol === 3 && operation === 'event') return 'content_note_resolution_v2'
  return resolutionBad()
}
/** T admits the new root only to F/J. Shared higher-level readers stay bounded. */
export function rejectLegacy3ResolutionRoots(history: readonly unknown[]): void {
  if (history.some(value => {
    const r = value as Partial<BrowserResolution4> | null
    return r?.record_kind === 'resolution' && r.protocol_version === 3 && r.transport_kind === 'legacy_resolution_v2'
  })) resolutionBad()
}
type Strategy = NoteSyncResolutionV2['resolution']['strategy']
interface HistoryIdentity { account_id: string; device_id: string; project_id: string; note_id: string; bootstrap_id: string; record_kind: string; record_id: string }
export interface BrowserResolution4 extends HistoryIdentity {
  record_version: 4; record_kind: 'resolution'; event_id: string; source_device_id: string
  protocol_version: 2 | 3; transport_kind: 'legacy_resolution_v2' | 'content_note_resolution_v2'; server_sequence: number
  header: NoteSyncResolutionV2Header; wire_item: string; wire_digest: string; payload_digest: string; dependency_digest: string
  remote_group_id: string; remote_generation: number; local_group_id: string; local_generation: number; group_digest: string
  parent_event_ids: string[]; strategy: Strategy; result_operation: 'upsert' | 'delete'; retained_note_id: string | null
  evidence_envelope: LocalEnvelope; record_digest: string
}
export type ResolutionNodeRef = { kind: 'candidate'; event_id: string } | { kind: 'resolution_result'; resolution_event_id: string; result_role: 'original' | 'retained'; resolution_digest: string }
export interface BrowserResolutionHead2 extends HistoryIdentity { record_version: 2; record_kind: 'head'; head_revision: number; linear_head_event_id: string; tip_event_ids: string[]; node_ref: ResolutionNodeRef }
export interface BrowserGroupTransition2 extends HistoryIdentity {
  record_version: 2; record_kind: 'group_transition'; group_id: string; from_generation: number; from_digest: string
  resolution_event_id: string; resolution_digest: string; resolved_parent_ids: string[]; effective_lifecycle: 'resolved'; record_digest: string
}
export interface ResolutionReference { event_id: string; device_id: string; server_sequence: number; project_id: string; entity_id: string; entity_type: 'note'; operation: 'event' | 'resolution'; revision: number; updated_at: string; deleted_at: null }
export interface BrowserResolutionProof1 {
  record_version: 1; owner_kind: 'note_resolution'; outcome: 'applied'; account_id: string; bootstrap_id: string; protocol_version: 2 | 3
  reference: ResolutionReference; server_sequence: number; header: NoteSyncResolutionV2Header
  wire_digest: string; payload_digest: string; dependency_digest: string; resolution_digest: string; transition_digest: string
  output_digests: { original: string; retained: string | null }; outcome_digest: string
}
export interface BrowserResolutionInbox10 {
  record_version: 10; state: 'note_resolution_proven'; account_id: string; device_id: string; event_id: string
  server_sequence: number; protocol_version: 2 | 3; wire_item: string; resolution_proof: BrowserResolutionProof1
}
export type FutureNoteHistory = BrowserNoteHistoryRecord | BrowserResolution4 | BrowserResolutionHead2 | BrowserGroupTransition2
export type FutureInbox = BrowserInboxRecord | BrowserResolutionInbox10 | DocumentInbox
const H = ['record_version','account_id','device_id','project_id','note_id','bootstrap_id','record_kind','record_id']
export const resolutionIdentity = (r: BrowserResolution4) => { const { evidence_envelope: _e, record_digest: _d, ...identity } = r; return identity }
function exact(value: unknown, fields: string[]): void { if (!localExact(value, fields)) resolutionBad() }
export function resolutionIds(value: unknown, min = 2, max = 64): asserts value is string[] {
  if (!Array.isArray(value) || value.length < min || value.length > max) resolutionBad()
  for (let i = 0; i < value.length; i++) if (!Object.hasOwn(value, i) || !canonicalNoteUuid(value[i]) || i > 0 && value[i - 1]! >= value[i]!) resolutionBad()
}
function time(value: unknown): void { if (typeof value !== 'string') resolutionBad(); try { if (canonicalizeSyncTimestamp(value) !== value) resolutionBad() } catch { resolutionBad() } }
function identity(r: HistoryIdentity, scope: BrowserSyncScope, bootstrap?: string): void {
  if (r.account_id !== scope.accountId || r.device_id !== scope.deviceId || !canonicalNoteUuid(r.bootstrap_id) || bootstrap !== undefined && bootstrap !== r.bootstrap_id) resolutionBad()
  localId(r.project_id); localId(r.note_id)
}
function hashes(...values: unknown[]): void { if (!values.every(noteHistoryHash)) resolutionBad() }
export function validateResolutionHeader(value: unknown): NoteSyncResolutionV2Header {
  exact(value, ['event_id','parent_event_id','additional_parent_event_ids','project_id','entity_id','entity_type','operation','revision','updated_at'])
  const h = value as NoteSyncResolutionV2Header
  if (!canonicalNoteUuid(h.event_id) || h.entity_type !== 'note' || h.operation !== 'resolution' || !localSafe(h.revision, 2)) resolutionBad()
  resolutionIds([h.parent_event_id, ...(Array.isArray(h.additional_parent_event_ids) ? h.additional_parent_event_ids : [])])
  if (!Array.isArray(h.additional_parent_event_ids) || h.additional_parent_event_ids.includes(h.event_id) || h.parent_event_id === h.event_id) resolutionBad()
  localId(h.project_id); localId(h.entity_id); time(h.updated_at); return h
}
/** Check lengths before base64 decoding. The evidence limit is intentionally not a local-source limit. */
export function validateResolutionEnvelope(value: unknown): LocalEnvelope {
  exact(value, ['crypto_version','aad_version','nonce','ciphertext'])
  const e = value as LocalEnvelope
  if (e.crypto_version !== 1 || e.aad_version !== 1 || typeof e.nonce !== 'string' || e.nonce.length !== 32 || typeof e.ciphertext !== 'string' || e.ciphertext.length > Math.ceil((MAX_RESOLUTION_EVIDENCE_BYTES + 16) * 4 / 3)) resolutionBad()
  const encoded=e.ciphertext,mod=encoded.length%4,alphabet='ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_'
  if(!/^[A-Za-z0-9_-]+$/.test(encoded)||mod===1||Math.floor(encoded.length*3/4)<16||Math.floor(encoded.length*3/4)>MAX_RESOLUTION_EVIDENCE_BYTES+16||mod===2&&(alphabet.indexOf(encoded.at(-1)!)&15)!==0||mod===3&&(alphabet.indexOf(encoded.at(-1)!)&3)!==0)resolutionBad()
  const n = decodeBase64Url(e.nonce, { expectedLength: 24 });n.fill(0);return e
}
/** Bound JSON without constructing an oversized serialization or following cycles. */
export function boundResolutionJson(value: unknown, limit = MAX_RESOLUTION_RECORD_BYTES): void {
  const seen = new Set<object>(); let left = limit
  const charge = (n: number) => { left -= n; if (left < 0) resolutionBad() }
  const walk = (v: unknown, depth: number): void => {
    if (depth > 128) resolutionBad()
    if (typeof v === 'string') { if (v.length > left) resolutionBad(); charge(new TextEncoder().encode(JSON.stringify(v)).length) }
    else if (typeof v === 'number') { if (!Number.isFinite(v)) resolutionBad(); charge(String(v).length) }
    else if (typeof v === 'boolean') charge(v ? 4 : 5)
    else if (v === null) charge(4)
    else if (typeof v === 'object') {
      if (seen.has(v)) resolutionBad(); seen.add(v)
      if (Array.isArray(v)) { charge(2 + Math.max(0, v.length - 1)); for (let i = 0; i < v.length; i++) { if (!Object.hasOwn(v, i)) resolutionBad(); walk(v[i], depth + 1) } }
      else { const entries = Object.entries(v); charge(2 + Math.max(0, entries.length - 1)); for (const [key, entry] of entries) { walk(key, depth + 1); charge(1); walk(entry, depth + 1) } }
      seen.delete(v)
    } else resolutionBad()
  }
  walk(value, 0)
}
export function validateResolution4(value: unknown, s: BrowserSyncScope, bootstrap?: string): BrowserResolution4 {
  exact(value, [...H,'event_id','source_device_id','protocol_version','transport_kind','server_sequence','header','wire_item','wire_digest','payload_digest','dependency_digest','remote_group_id','remote_generation','local_group_id','local_generation','group_digest','parent_event_ids','strategy','result_operation','retained_note_id','evidence_envelope','record_digest'])
  const r = value as BrowserResolution4; identity(r, s, bootstrap)
  if (r.record_version !== 4 || r.record_kind !== 'resolution' || r.record_id !== r.event_id || !canonicalNoteUuid(r.event_id) || !canonicalNoteUuid(r.source_device_id) || !localSafe(r.server_sequence, 1) || !canonicalNoteUuid(r.remote_group_id) || !canonicalNoteUuid(r.local_group_id) || !localSafe(r.remote_generation, 1) || !localSafe(r.local_generation, 1)) resolutionBad()
  if (!(r.protocol_version === 2 && r.transport_kind === 'legacy_resolution_v2' || r.protocol_version === 3 && ['legacy_resolution_v2','content_note_resolution_v2'].includes(r.transport_kind))) resolutionBad()
  const h = validateResolutionHeader(r.header); resolutionIds(r.parent_event_ids)
  if (h.event_id !== r.event_id || h.project_id !== r.project_id || h.entity_id !== r.note_id || !resolutionSame(r.parent_event_ids, [h.parent_event_id, ...h.additional_parent_event_ids])) resolutionBad()
  if (!['choose_version','manual_merge','delete','keep_both'].includes(r.strategy) || !['upsert','delete'].includes(r.result_operation) || ['manual_merge','keep_both'].includes(r.strategy) && r.result_operation !== 'upsert' || r.strategy === 'delete' && r.result_operation !== 'delete') resolutionBad()
  if (r.strategy === 'keep_both') { localId(r.retained_note_id); if (r.retained_note_id === r.note_id || r.parent_event_ids.length !== 2) resolutionBad() } else if (r.retained_note_id !== null) resolutionBad()
  hashes(r.wire_digest,r.payload_digest,r.dependency_digest,r.group_digest,r.record_digest)
  validateResolutionEnvelope(r.evidence_envelope); boundResolutionJson(r)
  const row = validateBrowserInboxRecord({record_version:1,state:'received',account_id:s.accountId,device_id:s.deviceId,event_id:r.event_id,server_sequence:r.server_sequence,protocol_version:r.protocol_version,wire_item:r.wire_item},s)
  const e = JSON.parse(row.wire_item).event
  if (e.device_id !== r.source_device_id || e.project_id !== r.project_id || e.entity_id !== r.note_id || e.entity_type !== 'note' || resolutionTransportKind(r.protocol_version,e.operation) !== r.transport_kind || e.revision !== h.revision || e.deleted_at !== null) resolutionBad()
  if (canonicalizeSyncTimestamp(e.updated_at) !== h.updated_at) resolutionBad()
  return r
}
export function validateResolutionHead2(value: unknown, s: BrowserSyncScope, bootstrap?: string): BrowserResolutionHead2 {
  exact(value, [...H,'head_revision','linear_head_event_id','tip_event_ids','node_ref']); const r = value as BrowserResolutionHead2; identity(r,s,bootstrap)
  if (r.record_version !== 2 || r.record_kind !== 'head' || r.record_id !== 'head' || !localSafe(r.head_revision,1) || !canonicalNoteUuid(r.linear_head_event_id)) resolutionBad()
  resolutionIds(r.tip_event_ids,1,Number.MAX_SAFE_INTEGER)
  const n = r.node_ref
  if (n?.kind === 'candidate') { exact(n,['kind','event_id']); if (n.event_id !== r.linear_head_event_id) resolutionBad() }
  else if (n?.kind === 'resolution_result') { exact(n,['kind','resolution_event_id','result_role','resolution_digest']); if (n.resolution_event_id !== r.linear_head_event_id || !['original','retained'].includes(n.result_role)) resolutionBad(); hashes(n.resolution_digest) }
  else resolutionBad()
  return r
}
export function validateGroupTransition2(value: unknown, s: BrowserSyncScope, bootstrap?: string): BrowserGroupTransition2 {
  exact(value,[...H,'group_id','from_generation','from_digest','resolution_event_id','resolution_digest','resolved_parent_ids','effective_lifecycle','record_digest']); const r=value as BrowserGroupTransition2; identity(r,s,bootstrap)
  if (r.record_version !== 2 || r.record_kind !== 'group_transition' || r.record_id !== r.group_id || !canonicalNoteUuid(r.group_id) || !canonicalNoteUuid(r.resolution_event_id) || !localSafe(r.from_generation,1) || r.effective_lifecycle !== 'resolved') resolutionBad()
  hashes(r.from_digest,r.resolution_digest,r.record_digest); resolutionIds(r.resolved_parent_ids); return r
}
export function validateResolutionProof1(value: unknown,s:BrowserSyncScope,bootstrap?:string):BrowserResolutionProof1 {
  exact(value,['record_version','owner_kind','outcome','account_id','bootstrap_id','protocol_version','reference','server_sequence','header','wire_digest','payload_digest','dependency_digest','resolution_digest','transition_digest','output_digests','outcome_digest']); const p=value as BrowserResolutionProof1
  if(p.record_version!==1||p.owner_kind!=='note_resolution'||p.outcome!=='applied'||p.account_id!==s.accountId||!canonicalNoteUuid(p.bootstrap_id)||bootstrap!==undefined&&p.bootstrap_id!==bootstrap||![2,3].includes(p.protocol_version)||!localSafe(p.server_sequence,1))resolutionBad()
  const h=validateResolutionHeader(p.header),r=p.reference
  exact(r,['event_id','device_id','server_sequence','project_id','entity_id','entity_type','operation','revision','updated_at','deleted_at'])
  resolutionTransportKind(p.protocol_version,r.operation)
  if(!canonicalNoteUuid(r.device_id)||r.event_id!==h.event_id||r.server_sequence!==p.server_sequence||r.project_id!==h.project_id||r.entity_id!==h.entity_id||r.entity_type!=='note'||r.revision!==h.revision||r.deleted_at!==null||canonicalizeSyncTimestamp(r.updated_at)!==h.updated_at)resolutionBad()
  hashes(p.wire_digest,p.payload_digest,p.dependency_digest,p.resolution_digest,p.transition_digest,p.outcome_digest)
  exact(p.output_digests,['original','retained']); hashes(p.output_digests.original);if(p.output_digests.retained!==null)hashes(p.output_digests.retained)
  return p
}
export function validateResolutionInbox10(value:unknown,s:BrowserSyncScope,bootstrap?:string):BrowserResolutionInbox10 {
  exact(value,['record_version','state','account_id','device_id','event_id','server_sequence','protocol_version','wire_item','resolution_proof']);const r=value as BrowserResolutionInbox10
  if(r.record_version!==10||r.state!=='note_resolution_proven')resolutionBad()
  const {resolution_proof:_p,...base}=r
  validateBrowserInboxRecord({...base,record_version:1,state:'received'},s)
  const p=validateResolutionProof1(r.resolution_proof,s,bootstrap)
  if(p.header.event_id!==r.event_id||p.protocol_version!==r.protocol_version||p.server_sequence!==r.server_sequence||!resolutionSame(p.reference,JSON.parse(r.wire_item).event))resolutionBad()
  return r
}
export function readFutureHistoryRecord(value:unknown,s:BrowserSyncScope):FutureNoteHistory {
  const r=value as {record_kind?:string;record_version?:number}
  if(r?.record_kind==='resolution')return validateResolution4(value,s)
  if(r?.record_kind==='head'&&r.record_version===2)return validateResolutionHead2(value,s)
  if(r?.record_kind==='group_transition')return validateGroupTransition2(value,s)
  return validateNoteHistoryRecord(value,s)
}
export function readFutureInboxRecord(value:unknown,s:BrowserSyncScope):FutureInbox {
  if ((value as {record_version?:number})?.record_version===11) return validateDocumentInbox(value,s)
  return (value as {record_version?:number})?.record_version===10?validateResolutionInbox10(value,s):validateBrowserInboxRecord(value,s)
}
