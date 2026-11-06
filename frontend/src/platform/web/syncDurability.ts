import { validateMapInbox, type MapInbox } from './mapRecords'
import { validateDocumentInbox, type DocumentInbox } from './documentRecords'
import {validateContentReceipt2,contentBaseReceipt,validateContentSelfEchoProof2,type BrowserContentSelfEchoProof2,type BrowserAnyUploadReceipt} from './contentAuthorityRecords'
import {validateContentNoteConflictProof,type BrowserContentNoteConflictProof} from './contentNoteRecords'
import { validateContentNoteProof,type BrowserContentNoteProof } from './contentNoteRecords'
import {validateStageOrderProof,type BrowserStageOrderProof} from './stageOrderRecords'
import { validateStageProof, type BrowserStageProof } from './stageRecords'
import { validateBrowserNoteProof, validateBrowserNoteConflictProof, type BrowserNoteConflictProof, type BrowserNoteProof } from './noteApplyRecords'
import { validateBrowserMetadataProof, type BrowserMetadataProof } from './metadataApplyRecords'
import type { AccountDescriptor, V3Descriptor } from '@/api/encryptedSyncV3'
import { MAX_ENCRYPTED_SYNC_BATCH_CIPHERTEXT_BYTES, MAX_ENCRYPTED_SYNC_WIRE_BODY_BYTES } from '@/api/encryptedSync'
import { parseV2PullResponse, parseV2PushResponse, validateV2PushItem } from '@/api/encryptedSyncV2'
import { parseV3Pull, parseV3PushResponse } from '@/api/encryptedSyncV3'
import type { CommitInboundPageResult, NoteSyncPullState } from '@/infrastructure/sqlite/noteSyncInboxRepository'
import { BrowserPersistenceError, type runBrowserTransaction } from './persistence'

export interface BrowserSyncScope { canonicalUserId: string; accountId: string; deviceId: string }
export interface BrowserSyncCheckpoint extends NoteSyncPullState { proven_prefix: number; next_local_ordinal: number }
export interface BrowserOutboxRecord {
  record_version: 1; account_id: string; device_id: string; event_id: string
  local_ordinal: number; protocol_version: 2 | 3; state: 'sealed' | 'accepted'; raw_body: string
}
export type BrowserSealedOutboxRecord = BrowserOutboxRecord & { state: 'sealed' }
export interface BrowserReceivedInboxRecord {
  record_version: 1; account_id: string; device_id: string; event_id: string
  server_sequence: number; protocol_version: 2 | 3; state: 'received'; wire_item: string
}
export interface BrowserAppliedInboxRecord extends Omit<BrowserReceivedInboxRecord, 'record_version' | 'state'> { record_version: 2; state: 'applied'; applied_proof: BrowserMetadataProof }
export interface BrowserNoteInboxRecord extends Omit<BrowserReceivedInboxRecord, 'record_version' | 'state'> { record_version: 3; state: 'note_proven'; note_proof: BrowserNoteProof }
export interface BrowserNoteConflictInboxRecord extends Omit<BrowserReceivedInboxRecord,'record_version'|'state'> {record_version:4;state:'note_conflict_preserved';note_conflict_proof:BrowserNoteConflictProof}
export interface BrowserStageOrderInboxRecord extends Omit<BrowserReceivedInboxRecord,'record_version'|'state'> {record_version:6;state:'stage_order_proven';stage_order_proof:BrowserStageOrderProof}
export interface BrowserStageInboxRecord extends Omit<BrowserReceivedInboxRecord,'record_version'|'state'> {record_version:5;state:'stage_proven';stage_proof:BrowserStageProof}
export interface BrowserContentNoteConflictInboxRecord extends Omit<BrowserReceivedInboxRecord,'record_version'|'state'> {record_version:8;state:'content_note_conflict_preserved';content_note_conflict_proof:BrowserContentNoteConflictProof}
export interface BrowserContentNoteInboxRecord extends Omit<BrowserReceivedInboxRecord,'record_version'|'state'> {record_version:7;state:'content_note_proven';content_note_proof:BrowserContentNoteProof}
export interface BrowserContentSelfEchoInboxRecord extends Omit<BrowserReceivedInboxRecord,'record_version'|'state'> {record_version:9;state:'content_note_self_echo_proven';content_note_self_echo_proof:BrowserContentSelfEchoProof2}
export type BrowserInboxRecord = MapInbox | DocumentInbox | BrowserContentSelfEchoInboxRecord | BrowserContentNoteConflictInboxRecord | BrowserContentNoteInboxRecord | BrowserStageOrderInboxRecord | BrowserStageInboxRecord | BrowserReceivedInboxRecord | BrowserAppliedInboxRecord | BrowserNoteInboxRecord | BrowserNoteConflictInboxRecord
export interface BrowserUploadReceipt {
  record_version: 1; account_id: string; device_id: string; event_id: string
  protocol_version: 2 | 3; server_sequence: number; duplicate: boolean
}
/** Existing transport identity only; never a complete-head or authenticated apply proof. */
export type BrowserEncryptedHistoryReference = V3Descriptor | AccountDescriptor
export interface BrowserRetainedEncryptedHistory {
  reference: BrowserEncryptedHistoryReference
  outbox: BrowserOutboxRecord | null
  receipt: BrowserAnyUploadReceipt | null
  inbox: BrowserInboxRecord | null
}

interface CursorRecord extends BrowserSyncCheckpoint { record_version: 1; account_id: string; device_id: string }
type Schedule<T> = Parameters<typeof runBrowserTransaction<T>>[3]
export type BrowserSyncTransaction = <T>(scope: BrowserSyncScope, stores: string[], mode: 'readonly' | 'readwrite', schedule: Schedule<T>) => Promise<T>
function failure(code: 'corrupt_record' | 'conflicting_record' | 'stale_cursor' = 'corrupt_record'): never { throw new BrowserPersistenceError(code) }
const exact = (value: unknown, keys: readonly string[]): boolean => typeof value === 'object' && value !== null && !Array.isArray(value)
  && Object.keys(value).length === keys.length && keys.every(key => Object.hasOwn(value, key))
const safe = (n: unknown, minimum = 0): n is number => Number.isSafeInteger(n) && (n as number) >= minimum
const limitCheck = (n: number) => { if (!safe(n, 1) || n > 200) failure() }
const accountRange = (account: string) => IDBKeyRange.bound([account], [account, []])
function snapshotScope(scope: BrowserSyncScope): BrowserSyncScope {
  if (!exact(scope, ['canonicalUserId', 'accountId', 'deviceId'])) failure()
  if (!Object.values(scope).every(value => typeof value === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(value))) throw new BrowserPersistenceError('account_mismatch')
  return Object.freeze({ ...scope })
}

export { snapshotScope as snapshotBrowserSyncScope }

/** Comparison follows native metadata/encrypted-byte equality, independent of JSON key order. */
export function immutableWireIdentity(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(immutableWireIdentity).join(',')}]`
  if (value !== null && typeof value === 'object') return `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${immutableWireIdentity((value as Record<string, unknown>)[key])}`).join(',')}}`
  return JSON.stringify(value)
}
function compactJSON(raw: string): string {
  let quoted = false, escaped = false, start = 0
  const parts: string[] = []
  for (let index = 0; index < raw.length; index++) {
    const character = raw[index]
    if (quoted) {
      if (escaped) escaped = false
      else if (character === '\\') escaped = true
      else if (character === '"') quoted = false
    } else if (character === '"') quoted = true
    else if (character === ' ' || character === '\t' || character === '\r' || character === '\n') {
      parts.push(raw.slice(start, index)); start = index + 1
    }
  }
  parts.push(raw.slice(start))
  return parts.join('')
}
function wireJSON(raw: string, exactSerialization = false): unknown {
  try {
    if (typeof raw !== 'string' || raw.length > MAX_ENCRYPTED_SYNC_WIRE_BODY_BYTES || new TextEncoder().encode(raw).length > MAX_ENCRYPTED_SYNC_WIRE_BODY_BYTES) failure()
    const value: unknown = JSON.parse(raw)
    // Persisted raw bodies must be a serialization from the existing encoders.
    // Duplicate/overwritten keys could otherwise smuggle plaintext into saved raw bytes.
    if (exactSerialization && compactJSON(raw) !== JSON.stringify(value)) failure()
    return value
  } catch { return failure() }
}
interface WireItem { event: { event_id: string; device_id?: string; server_sequence?: number; scope?: string; canonical_user_id?: string }; object: unknown }
interface WirePage { protocol_version: 2 | 3; encrypted_sync_version: 2 | 3; items: WireItem[]; next_cursor: number; has_more: boolean }
function validatePage(page: WirePage, since: number, user: string): number {
  try {
    const parsed = page.protocol_version === 2 ? parseV2PullResponse(page, since, 200) : page.protocol_version === 3 ? parseV3Pull(page, since, 200) : failure()
    if (page.items.some(item => item.event.scope === 'account' && item.event.canonical_user_id !== user)) throw new BrowserPersistenceError('account_mismatch')
    return parsed.items.reduce((total, item) => total + item.object.ciphertext.length, 0)
  } catch (error) { if (error instanceof BrowserPersistenceError) throw error; return failure() }
}

/** Only a singleton body from the existing v2/v3 encoders, never arbitrary JSON. */
export function validateBrowserSealedBody(raw: string, scope: BrowserSyncScope): { eventId: string; protocol: 2 | 3; ciphertextBytes: number } {
  const value = wireJSON(raw, true)
  if (!exact(value, ['protocol_version', 'encrypted_sync_version', 'device_id', 'items'])) failure()
  const body = value as WirePage & { device_id: string }
  if (body.device_id !== scope.deviceId) throw new BrowserPersistenceError('account_mismatch')
  if (!Array.isArray(body.items) || body.items.length !== 1 || body.encrypted_sync_version !== body.protocol_version) failure()
  try {
    const item = body.items[0]!
    let ciphertextBytes: number
    if (body.protocol_version === 2) ciphertextBytes = validateV2PushItem(item).ciphertextBytes
    else if (body.protocol_version === 3) {
      // The shared parser proves the descriptor/envelope allowlist. Reject receive-only fields in outbound input.
      if (!exact(item, ['event', 'object']) || !exact(item.event, item.event?.scope === 'account'
        ? ['event_id','canonical_user_id','scope','entity_id','entity_type','operation','revision','updated_at','deleted_at']
        : ['event_id','project_id','entity_id','entity_type','operation','revision','updated_at','deleted_at'])) failure()
      ciphertextBytes = validatePage({ protocol_version: 3, encrypted_sync_version: 3, next_cursor: 1, has_more: false,
        items: [{ ...item, event: { ...item.event, device_id: scope.deviceId, server_sequence: 1 } }] }, 0, scope.canonicalUserId)
    } else failure()
    return { eventId: item.event.event_id, protocol: body.protocol_version, ciphertextBytes }
  } catch (error) { if (error instanceof BrowserPersistenceError) throw error; return failure() }
}
export function validateBrowserInboundPage(raw: string, since: number, scope: BrowserSyncScope): WirePage {
  const page = wireJSON(raw) as WirePage
  validatePage(page, since, scope.canonicalUserId)
  return page
}
function cursor(value: unknown, scope: BrowserSyncScope): CursorRecord {
  if (value === undefined) return { record_version: 1, account_id: scope.accountId, device_id: scope.deviceId, pull_cursor: 0, ack_cursor: 0, proven_prefix: 0, next_local_ordinal: 1 }
  if (!exact(value, ['record_version','account_id','device_id','pull_cursor','ack_cursor','proven_prefix','next_local_ordinal'])) failure()
  const row = value as CursorRecord
  if (row.record_version !== 1 || row.account_id !== scope.accountId || row.device_id !== scope.deviceId || !safe(row.pull_cursor) || !safe(row.ack_cursor) || !safe(row.proven_prefix) || row.ack_cursor>row.proven_prefix || row.proven_prefix>row.pull_cursor || !safe(row.next_local_ordinal, 1)) failure()
  return row
}
export { cursor as validateBrowserSyncCursor }
export function readBrowserSyncCursor(tx: IDBTransaction, scope: BrowserSyncScope, ready: (state: CursorRecord) => void, fail: (error: unknown) => void): void {
  const request = tx.objectStore('cursors').get([scope.accountId, scope.deviceId])
  request.onsuccess = () => {
    try {
      const state = cursor(request.result, scope)
      if (request.result !== undefined) { ready(state); return }
      // Missing checkpoint is a fresh scope only when it has no retained sync data.
      const outbound = tx.objectStore('outbox').count(accountRange(scope.accountId))
      const inbound = tx.objectStore('inbox').count(accountRange(scope.accountId))
      let received = 0
      const finish = () => {
        if (++received !== 2) return
        try { if (outbound.result || inbound.result) failure(); ready(state) } catch (error) { fail(error) }
      }
      outbound.onsuccess = finish; inbound.onsuccess = finish
    } catch (error) { fail(error) }
  }
}
function outbox(value: unknown, scope: BrowserSyncScope): BrowserOutboxRecord {
  if (!exact(value, ['record_version','account_id','device_id','event_id','local_ordinal','protocol_version','state','raw_body'])) failure()
  const row = value as BrowserOutboxRecord
  if (row.record_version !== 1 || row.account_id !== scope.accountId || row.device_id !== scope.deviceId || !['sealed', 'accepted'].includes(row.state) || !safe(row.local_ordinal, 1)) failure()
  const parsed = validateBrowserSealedBody(row.raw_body, scope)
  if (parsed.eventId !== row.event_id || parsed.protocol !== row.protocol_version) failure()
  return row
}
export function validateBrowserUploadPair(value: unknown, receipt: unknown, scope: BrowserSyncScope): BrowserOutboxRecord {
  const row = outbox(value, scope)
  if (receipt === undefined) { if (row.state !== 'sealed') failure(); return row }
  if (!exact(receipt, ['record_version','account_id','device_id','event_id','protocol_version','server_sequence','duplicate'])) failure()
  const proof = receipt as BrowserAnyUploadReceipt
  if (row.state !== 'accepted' || proof.record_version !== 1 || proof.account_id !== row.account_id || proof.device_id !== row.device_id
    || proof.event_id !== row.event_id || proof.protocol_version !== row.protocol_version || !safe(proof.server_sequence, 1) || typeof proof.duplicate !== 'boolean') failure()
  return row
}

/** Version-aware storage read, NOT authenticated acceptance or permission to ACK. */
export function validateBrowserStoredUploadPair(value:unknown,receipt:unknown,scope:BrowserSyncScope):BrowserOutboxRecord{
 if((receipt as {record_version?:number})?.record_version===2){
  const r=validateContentReceipt2(receipt,scope),o=validateBrowserUploadPair(value,contentBaseReceipt(r),scope),e=JSON.parse(o.raw_body).items[0].event
  if(o.protocol_version!==3||e.entity_type!=='note'||e.operation!=='event'||e.deleted_at!==null)failure()
  return o
 }
 return validateBrowserUploadPair(value,receipt,scope)
}

export function prepareBrowserUploadAcceptance(scope: BrowserSyncScope, expected: BrowserOutboxRecord, response: unknown): BrowserUploadReceipt {
  scope = snapshotScope(scope)
  const event = outbox(expected, scope)
  try {
    const parsed = event.protocol_version === 2 ? parseV2PushResponse(response, [event.event_id]) : parseV3PushResponse(response, [event.event_id])
    const receipt = parsed.results[0]!
    return { record_version: 1, account_id: scope.accountId, device_id: scope.deviceId, event_id: receipt.event_id,
      protocol_version: event.protocol_version, server_sequence: receipt.server_sequence, duplicate: receipt.duplicate }
  } catch { return failure('conflicting_record') }
}

/** Migration validation only; never transforms or repairs C21.1.03 v2 records. */
export function validateBrowserV2SyncState(scopes: BrowserSyncScope[], outbound: unknown[], inbound: unknown[], checkpoints: unknown[]): void {
  if (inbound.some(value => (value as BrowserReceivedInboxRecord)?.record_version !== 1 || (value as BrowserReceivedInboxRecord)?.state !== 'received')) failure()
  const scopeFor = (value: unknown): BrowserSyncScope => {
    const id = (value as { account_id?: string } | null)?.account_id
    return scopes.find(scope => scope.accountId === id) ?? failure()
  }
  const outgoing = outbound.map(value => { const row = outbox(value, scopeFor(value)); if (row.state !== 'sealed') failure(); return row })
  const incoming = inbound.map(value => validateBrowserInboxRecord(value, scopeFor(value)))
  const states = checkpoints.map(value => {const row=cursor(value,scopeFor(value));if(row.ack_cursor!==0||row.proven_prefix!==0)failure();return row})
  for (const scope of scopes) {
    const out = outgoing.filter(row => row.account_id === scope.accountId), received = incoming.filter(row => row.account_id === scope.accountId)
    const state = states.find(row => row.account_id === scope.accountId)
    if (!state) { if (out.length || received.length) failure(); continue }
    const ordinal = out.reduce((max, row) => Math.max(max, row.local_ordinal), 0)
    if (out.length !== ordinal || state.next_local_ordinal !== ordinal + 1 || state.pull_cursor !== received.reduce((max, row) => Math.max(max, row.server_sequence), 0)) failure()
    for (const row of received) { const own = out.find(event => event.event_id === row.event_id); if (own) assertOwnEcho(own, wireJSON(row.wire_item) as WireItem) }
  }
}

/** Non-destructive v3 upgrade validation, including every immutable upload receipt and own echo. */
export function validateBrowserV3SyncState(scopes: BrowserSyncScope[], outbound: unknown[], inbound: unknown[], checkpoints: unknown[], proofs: unknown[]): void {
  const scopeFor = (value: unknown) => scopes.find(scope => scope.accountId === (value as { account_id?: string } | null)?.account_id) ?? failure()
  const rows = outbound.map(value => validateBrowserUploadPair(value, proofs.find(proof => (proof as BrowserUploadReceipt)?.account_id === (value as BrowserOutboxRecord)?.account_id && (proof as BrowserUploadReceipt)?.event_id === (value as BrowserOutboxRecord)?.event_id), scopeFor(value)))
  const sequences = new Set<string>(), events = new Set<string>()
  for (const proof of proofs) {
    const scope = scopeFor(proof), receipt = proof as BrowserUploadReceipt
    const row = rows.find(row => row.account_id === receipt.account_id && row.event_id === receipt.event_id)
    validateBrowserUploadPair(row, proof, scope)
    const key = JSON.stringify([receipt.account_id, receipt.server_sequence]), eventKey = JSON.stringify([receipt.account_id, receipt.event_id])
    if (sequences.has(key) || events.has(eventKey)) failure()
    sequences.add(key); events.add(eventKey)
    const atSequence = inbound.find(value => (value as BrowserReceivedInboxRecord)?.account_id === receipt.account_id && (value as BrowserReceivedInboxRecord)?.server_sequence === receipt.server_sequence) as BrowserReceivedInboxRecord | undefined
    const echo = inbound.find(value => (value as BrowserReceivedInboxRecord)?.account_id === receipt.account_id && (value as BrowserReceivedInboxRecord)?.event_id === receipt.event_id) as BrowserReceivedInboxRecord | undefined
    if (atSequence && atSequence.event_id !== receipt.event_id || echo && echo.server_sequence !== receipt.server_sequence) failure()
  }
  // Detached validation copies only; all original stored rows/bodies/proofs remain byte-for-byte unchanged.
  validateBrowserV2SyncState(scopes, rows.map(row => ({ ...row, state: 'sealed' })), inbound, checkpoints)
}

export function validateBrowserInboxRecord(value: unknown, scope: BrowserSyncScope): BrowserInboxRecord {
  if ((value as {record_version?:number})?.record_version === 12) return validateMapInbox(value, scope)
  if ((value as {record_version?:number})?.record_version === 11) return validateDocumentInbox(value, scope)

  if((value as {record_version?:number})?.record_version===9||(value as {state?:string})?.state==='content_note_self_echo_proven'){
    if(!exact(value,['record_version','account_id','device_id','event_id','server_sequence','protocol_version','state','wire_item','content_note_self_echo_proof']))failure()
    const r=value as BrowserContentSelfEchoInboxRecord
    if(r.record_version!==9||r.state!=='content_note_self_echo_proven'||r.protocol_version!==3)failure()
    const {content_note_self_echo_proof:proof,...base}=r
    // Received structural view only; never a synthetic Legacy/peer outcome or stored write.
    validateBrowserInboxRecord({...base,record_version:1,state:'received'},scope)
    const p=validateContentSelfEchoProof2(proof,scope)
    if(p.history_event_id!==r.event_id||p.server_sequence!==r.server_sequence||immutableWireIdentity(p.reference)!==immutableWireIdentity(JSON.parse(r.wire_item).event))failure()
    return r
  }

  const contentConflict=(value as BrowserInboxRecord)?.state==='content_note_conflict_preserved'
  const content=(value as BrowserInboxRecord)?.state==='content_note_proven'
  const order=(value as BrowserInboxRecord)?.state==='stage_order_proven'
  const stage=(value as BrowserInboxRecord)?.state==='stage_proven'
  const applied = (value as BrowserInboxRecord)?.state === 'applied', note = (value as BrowserInboxRecord)?.state === 'note_proven', conflict=(value as BrowserInboxRecord)?.state==='note_conflict_preserved'
  if (!exact(value, ['record_version','account_id','device_id','event_id','server_sequence','protocol_version','state','wire_item', ...(applied ? ['applied_proof'] : note ? ['note_proof'] : conflict ? ['note_conflict_proof'] : stage ? ['stage_proof'] : order ? ['stage_order_proof'] : content ? ['content_note_proof'] : contentConflict ? ['content_note_conflict_proof'] : [])])) failure()
  const row = value as Exclude<BrowserInboxRecord,BrowserContentSelfEchoInboxRecord|DocumentInbox|MapInbox>
  if (row.record_version !== (applied ? 2 : note ? 3 : conflict ? 4 : stage ? 5 : order ? 6 : content ? 7 : contentConflict ? 8 : 1) || row.account_id !== scope.accountId || row.device_id !== scope.deviceId || !['received','applied','note_proven','note_conflict_preserved','stage_proven','stage_order_proven','content_note_proven','content_note_conflict_preserved'].includes(row.state) || !safe(row.server_sequence, 1)) failure()
  const item = wireJSON(row.wire_item, true) as WireItem
  validatePage({ protocol_version: row.protocol_version, encrypted_sync_version: row.protocol_version, items: [item], next_cursor: row.server_sequence, has_more: false }, row.server_sequence - 1, scope.canonicalUserId)
  if (item.event.event_id !== row.event_id || item.event.server_sequence !== row.server_sequence) failure()
  if (row.state === 'applied') {
    const proof = validateBrowserMetadataProof(row.applied_proof, scope)
    if (proof.protocol_version !== row.protocol_version || proof.event_id !== row.event_id || proof.server_sequence !== row.server_sequence || immutableWireIdentity(proof.reference) !== immutableWireIdentity(item.event)) failure()
  }
  if(row.state==='content_note_conflict_preserved'){const p=validateContentNoteConflictProof(row.content_note_conflict_proof,scope);if(row.protocol_version!==3||p.reference.event_id!==row.event_id||p.server_sequence!==row.server_sequence||immutableWireIdentity(p.reference)!==immutableWireIdentity(item.event))failure()}
  if(row.state==='content_note_proven'){const p=validateContentNoteProof(row.content_note_proof,scope);if(row.protocol_version!==3||p.reference.event_id!==row.event_id||p.server_sequence!==row.server_sequence||immutableWireIdentity(p.reference)!==immutableWireIdentity(item.event))failure()}
  if(row.state==='stage_order_proven'){const p=validateStageOrderProof(row.stage_order_proof,scope);if(p.event_id!==row.event_id||p.server_sequence!==row.server_sequence||row.protocol_version!==3||immutableWireIdentity(p.reference)!==immutableWireIdentity(item.event))failure()}
  if(row.state==='stage_proven'){const p=validateStageProof(row.stage_proof,scope);if(p.event_id!==row.event_id||p.server_sequence!==row.server_sequence||row.protocol_version!==3||immutableWireIdentity(p.reference)!==immutableWireIdentity(item.event))failure()}
  if(row.state==='note_proven'){const proof=validateBrowserNoteProof(row.note_proof,scope);if(proof.protocol_version!==row.protocol_version||proof.reference.event_id!==row.event_id||proof.reference.server_sequence!==row.server_sequence||immutableWireIdentity(proof.reference)!==immutableWireIdentity(item.event))failure()}
  if(row.state==='note_conflict_preserved'){const proof=validateBrowserNoteConflictProof(row.note_conflict_proof,scope);if(proof.protocol_version!==row.protocol_version||immutableWireIdentity(proof.reference)!==immutableWireIdentity(item.event))failure()}
  return row
}

function assertOwnEcho(outbound: BrowserOutboxRecord, incoming: WireItem): void {
  const { device_id, server_sequence: _sequence, ...descriptor } = incoming.event
  const body = wireJSON(outbound.raw_body) as WirePage
  if (device_id !== outbound.device_id || immutableWireIdentity(body.items[0]) !== immutableWireIdentity({ event: descriptor, object: incoming.object })) failure('conflicting_record')
}

function historyReference(value: BrowserEncryptedHistoryReference, scope: BrowserSyncScope): BrowserEncryptedHistoryReference {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) failure()
  const account = 'scope' in value
  const keys = ['event_id','device_id','server_sequence','entity_id','entity_type','operation','revision','updated_at','deleted_at',
    ...(account ? ['canonical_user_id','scope'] : ['project_id'])]
  if (!exact(value, keys) || !safe(value.server_sequence, 1) || !safe(value.revision, 1)
    || !keys.filter(key => !['server_sequence','revision','deleted_at'].includes(key)).every(key => typeof (value as unknown as Record<string, unknown>)[key] === 'string')
    || !(value.deleted_at === null || typeof value.deleted_at === 'string')
    || !/^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(value.event_id)) failure()
  if (account && (value.scope !== 'account' || value.canonical_user_id !== scope.canonicalUserId)) throw new BrowserPersistenceError('account_mismatch')
  // All descriptor semantics are checked against the existing validated retained wire item below.
  return Object.freeze({ ...value })
}

/** Pure retained-wire validation. No decryption, ancestry/head inference, or apply claim. */
export function validateBrowserRetainedHistory(
  scope: BrowserSyncScope, reference: BrowserEncryptedHistoryReference,
  outbound: unknown, receipt: unknown, inbound: unknown,
): BrowserRetainedEncryptedHistory {
  scope = snapshotScope(scope); reference = historyReference(reference, scope)
  let outgoing: BrowserOutboxRecord | null = null, received: BrowserInboxRecord | null = null
  if (outbound !== undefined || receipt !== undefined) outgoing = validateBrowserStoredUploadPair(outbound, receipt, scope)
  if (inbound !== undefined) received = validateBrowserInboxRecord(inbound, scope)
  if (outgoing && outgoing.event_id !== reference.event_id || received && received.event_id !== reference.event_id) failure('conflicting_record')
  if (outgoing && received) {
    assertOwnEcho(outgoing, wireJSON(received.wire_item) as WireItem)
    if (receipt !== undefined && (receipt as BrowserAnyUploadReceipt).server_sequence !== received.server_sequence) failure('conflicting_record')
  }
  const accepted = outgoing?.state === 'accepted' ? outgoing : null
  if (!accepted && !received) throw new BrowserPersistenceError('missing_history')
  const descriptor = received ? (wireJSON(received.wire_item) as WireItem).event : {
    ...(wireJSON(accepted!.raw_body) as WirePage).items[0]!.event,
    device_id: accepted!.device_id, server_sequence: (receipt as BrowserAnyUploadReceipt).server_sequence,
  }
  if (immutableWireIdentity(descriptor) !== immutableWireIdentity(reference)) failure('conflicting_record')
  return { reference, outbox: accepted, receipt: accepted ? receipt as BrowserAnyUploadReceipt : null, inbox: received }
}

/** Dormant encrypted transport/upload proof durability. No fetch, keys, plaintext, apply or ACK methods. */
export class BrowserSyncDurability {
  constructor(private readonly transaction: BrowserSyncTransaction) {}

  enqueue(scope: BrowserSyncScope, bodies: readonly string[]): Promise<Array<{ status: 'enqueued' | 'already_enqueued'; item: BrowserOutboxRecord }>> {
    scope = snapshotScope(scope)
    if (!Array.isArray(bodies)) failure()
    limitCheck(bodies.length)
    if (bodies.length > 100) failure()
    // Snapshot before IDB/await; caller mutation cannot change the identity being committed.
    const prepared = bodies.map(raw => ({ raw, ...validateBrowserSealedBody(raw, scope) }))
    if (prepared.reduce((total, item) => total + item.ciphertextBytes, 0) > MAX_ENCRYPTED_SYNC_BATCH_CIPHERTEXT_BYTES) failure()
    return this.transaction(scope, ['outbox', 'inbox', 'cursors', 'receipts'], 'readwrite', (tx, result, fail) => {
      const store = tx.objectStore('outbox'), cursors = tx.objectStore('cursors')
      readBrowserSyncCursor(tx, scope, state => {
        try {
          const results: Array<{ status: 'enqueued' | 'already_enqueued'; item: BrowserOutboxRecord }> = []
          const next = (index: number) => {
            if (index === prepared.length) { cursors.put(state); result(results); return }
            const input = prepared[index]!, read = store.get([scope.accountId, input.eventId]), proof = tx.objectStore('receipts').get([scope.accountId, input.eventId])
            let ready = 0
            const finish = () => {
              if (++ready !== 2) return
              try {
                if (read.result !== undefined) {
                  const stored = validateBrowserStoredUploadPair(read.result, proof.result, scope)
                  if (immutableWireIdentity(wireJSON(stored.raw_body)) !== immutableWireIdentity(wireJSON(input.raw))) failure('conflicting_record')
                  results.push({ status: 'already_enqueued', item: stored })
                } else {
                  if (proof.result !== undefined) failure()
                  if (state.next_local_ordinal === Number.MAX_SAFE_INTEGER) failure()
                  const item: BrowserOutboxRecord = { record_version: 1, account_id: scope.accountId, device_id: scope.deviceId,
                    event_id: input.eventId, local_ordinal: state.next_local_ordinal++, protocol_version: input.protocol, state: 'sealed', raw_body: input.raw }
                  store.add(item)
                  results.push({ status: 'enqueued', item })
                }
                next(index + 1)
              } catch (error) { fail(error) }
            }
            read.onsuccess = finish; proof.onsuccess = finish
          }
          next(0)
        } catch (error) { fail(error) }
      }, fail)
    })
  }

  private listOutbound(scope: BrowserSyncScope, limit: number, eligibleOnly: boolean): Promise<BrowserOutboxRecord[]> {
    scope = snapshotScope(scope); limitCheck(limit)
    return this.transaction(scope, ['outbox', 'receipts'], 'readonly', (tx, result, fail) => {
      const records: BrowserOutboxRecord[] = []
      const request = tx.objectStore('outbox').index('ordinal').openCursor(accountRange(scope.accountId))
      request.onsuccess = () => {
        try {
          const current = request.result
          if (!current) { result(records); return }
          const proof = tx.objectStore('receipts').get([scope.accountId, current.value.event_id])
          proof.onsuccess = () => {
            try {
              const row = validateBrowserStoredUploadPair(current.value, proof.result, scope)
              if (!eligibleOnly || row.state === 'sealed') records.push(row)
              if (records.length === limit) result(records)
              else current.continue()
            } catch (error) { fail(error) }
          }
        } catch (error) { fail(error) }
      }
    })
  }
  listSealed(scope: BrowserSyncScope, limit = 200): Promise<BrowserOutboxRecord[]> { return this.listOutbound(scope, limit, true) }
  listOutbox(scope: BrowserSyncScope, limit = 200): Promise<BrowserOutboxRecord[]> { return this.listOutbound(scope, limit, false) }

  readUploadReceipt(scope: BrowserSyncScope, eventId: string): Promise<BrowserAnyUploadReceipt | null> {
    scope = snapshotScope(scope)
    return this.transaction(scope, ['outbox', 'receipts'], 'readonly', (tx, result, fail) => {
      const event = tx.objectStore('outbox').get([scope.accountId, eventId]), proof = tx.objectStore('receipts').get([scope.accountId, eventId])
      let ready = 0
      const finish = () => {
        if (++ready !== 2) return
        try {
          if (event.result === undefined && proof.result === undefined) { result(null); return }
          validateBrowserStoredUploadPair(event.result, proof.result, scope)
          result(proof.result ?? null)
        } catch (error) { fail(error) }
      }
      event.onsuccess = finish; proof.onsuccess = finish
    })
  }

  commitUploadAcceptance(scope: BrowserSyncScope, expectedEvent: BrowserOutboxRecord, response: unknown): Promise<'accepted' | 'already_accepted'> {
    scope = snapshotScope(scope)
    const expected = { ...expectedEvent }, receipt = prepareBrowserUploadAcceptance(scope, expected, response)
    return this.transaction(scope, ['outbox', 'inbox', 'cursors', 'receipts'], 'readwrite', (tx, result, fail) => {
      readBrowserSyncCursor(tx, scope, () => {
        const store = tx.objectStore('outbox'), proofs = tx.objectStore('receipts')
        const event = store.get([scope.accountId, expected.event_id]), proof = proofs.get([scope.accountId, expected.event_id])
        const sequence = proofs.index('server_sequence').get([scope.accountId, receipt.server_sequence])
        const received = tx.objectStore('inbox').index('event').get([scope.accountId, expected.event_id])
        const atSequence = tx.objectStore('inbox').get([scope.accountId, receipt.server_sequence])
        let ready = 0
        const finish = () => {
          if (++ready !== 5) return
          try {
            if (event.result === undefined) failure('conflicting_record')
            const row = validateBrowserStoredUploadPair(event.result, proof.result, scope)
            if (row.raw_body !== expected.raw_body || row.local_ordinal !== expected.local_ordinal || row.protocol_version !== expected.protocol_version
              || row.event_id !== expected.event_id || row.account_id !== expected.account_id || row.device_id !== expected.device_id) failure('conflicting_record')
            if (sequence.result !== undefined && sequence.result.event_id !== row.event_id) failure('conflicting_record')
            if (received.result !== undefined) { const item = validateBrowserInboxRecord(received.result, scope); if (item.server_sequence !== receipt.server_sequence) failure('conflicting_record'); assertOwnEcho(row, wireJSON(item.wire_item) as WireItem) }
            if (atSequence.result !== undefined && validateBrowserInboxRecord(atSequence.result, scope).event_id !== row.event_id) failure('conflicting_record')
            if (proof.result !== undefined) {
              if (proof.result.server_sequence !== receipt.server_sequence) failure('conflicting_record')
              result('already_accepted'); return // duplicate is response diagnostics; keep the first receipt unchanged.
            }
            proofs.add(receipt)
            store.put({ ...row, state: 'accepted' })
            result('accepted')
          } catch (error) { fail(error) }
        }
        event.onsuccess = finish; proof.onsuccess = finish; sequence.onsuccess = finish; received.onsuccess = finish; atSequence.onsuccess = finish
      }, fail)
    })
  }

  /** One bound read snapshot for every requested branch; never infer missing history as empty authority. */
  readRetainedHistory(scope: BrowserSyncScope, references: readonly BrowserEncryptedHistoryReference[]): Promise<BrowserRetainedEncryptedHistory[]> {
    scope = snapshotScope(scope)
    if (!Array.isArray(references)) failure()
    limitCheck(references.length)
    const expected = references.map(reference => historyReference(reference, scope)).sort((a, b) => a.event_id < b.event_id ? -1 : a.event_id > b.event_id ? 1 : 0)
    if (new Set(expected.map(reference => reference.event_id)).size !== expected.length) failure('conflicting_record')
    return this.transaction(scope, ['outbox', 'inbox', 'receipts', 'cursors'], 'readonly', (tx, result, fail) => {
      readBrowserSyncCursor(tx, scope, () => {
        const history: BrowserRetainedEncryptedHistory[] = new Array(expected.length)
        let completed = 0
        expected.forEach((reference, index) => {
          const outgoing = tx.objectStore('outbox').get([scope.accountId, reference.event_id])
          const proof = tx.objectStore('receipts').get([scope.accountId, reference.event_id])
          const incoming = tx.objectStore('inbox').index('event').get([scope.accountId, reference.event_id])
          let ready = 0
          const finish = () => {
            if (++ready !== 3) return
            try {
              history[index] = validateBrowserRetainedHistory(scope, reference, outgoing.result, proof.result, incoming.result)
              if (++completed === expected.length) result(history)
            } catch (error) { fail(error) }
          }
          outgoing.onsuccess = finish; proof.onsuccess = finish; incoming.onsuccess = finish
        })
      }, fail)
    })
  }

  readCheckpoint(scope: BrowserSyncScope): Promise<BrowserSyncCheckpoint> {
    scope = snapshotScope(scope)
    return this.transaction(scope, ['outbox', 'inbox', 'cursors'], 'readonly', (tx, result, fail) => {
      readBrowserSyncCursor(tx, scope, row => result({ pull_cursor: row.pull_cursor, ack_cursor: row.ack_cursor, proven_prefix: row.proven_prefix, next_local_ordinal: row.next_local_ordinal }), fail)
    })
  }

  listReceived(scope: BrowserSyncScope, limit = 200, afterServerSequence = 0): Promise<BrowserReceivedInboxRecord[]> {
    scope = snapshotScope(scope)
    limitCheck(limit)
    if (!safe(afterServerSequence)) failure()
    return this.transaction(scope, ['inbox'], 'readonly', (tx, result, fail) => {
      if (afterServerSequence === Number.MAX_SAFE_INTEGER) { result([]); return }
      const records: BrowserReceivedInboxRecord[] = []
      const request = tx.objectStore('inbox').openCursor(IDBKeyRange.bound([scope.accountId, afterServerSequence], [scope.accountId, Number.MAX_SAFE_INTEGER], true, false))
      request.onsuccess = () => {
        try {
          const current = request.result
          if (!current) { result(records); return }
          const row = validateBrowserInboxRecord(current.value, scope) // Unknown state/version cannot disappear behind a received-only index filter.
          if (row.state === 'received') records.push(row)
          if (records.length === limit) result(records)
          else current.continue()
        } catch (error) { fail(error) }
      }
    })
  }

  commitInboundPage(scope: BrowserSyncScope, since: number, rawPage: string): Promise<CommitInboundPageResult> {
    scope = snapshotScope(scope)
    if (!safe(since)) failure()
    const page = validateBrowserInboundPage(rawPage, since, scope)
    return this.transaction(scope, ['outbox', 'inbox', 'cursors', 'receipts'], 'readwrite', (tx, result, fail) => {
      const store = tx.objectStore('inbox'), cursors = tx.objectStore('cursors')
      readBrowserSyncCursor(tx, scope, state => {
        try {
          const replay = state.pull_cursor === page.next_cursor && state.pull_cursor > since
          if (state.pull_cursor !== since && !replay) failure('stale_cursor')
          if (page.items.length === 0) { result({ committed_cursor: since, new_events: 0, replayed_events: 0, has_more: false }); return }
          // Exact replay counts only the original page interval, not later inbox rows.
          const within = store.count(IDBKeyRange.bound([scope.accountId, since], [scope.accountId, page.next_cursor], true, false))
          within.onsuccess = () => {
            try {
              if (replay && within.result !== page.items.length) failure('conflicting_record')
              let added = 0, replayed = 0
              const next = (index: number) => {
                if (index === page.items.length) {
                  state.pull_cursor = page.next_cursor
                  cursors.put(state)
                  result({ committed_cursor: page.next_cursor, new_events: added, replayed_events: replayed, has_more: page.has_more })
                  return
                }
                const item = page.items[index]!, seq = item.event.server_sequence!, event = item.event.event_id
                const bySequence = store.get([scope.accountId, seq]), byEvent = store.index('event').get([scope.accountId, event])
                const outbound = tx.objectStore('outbox').get([scope.accountId, event]), proof = tx.objectStore('receipts').get([scope.accountId, event])
                let received = 0
                const finish = () => {
                  if (++received !== 4) return
                  try {
                    if (outbound.result !== undefined) {
                      assertOwnEcho(validateBrowserStoredUploadPair(outbound.result, proof.result, scope), item)
                      if (proof.result !== undefined && proof.result.server_sequence !== seq) failure('conflicting_record')
                    }
                    else if (proof.result !== undefined) failure()
                    if (bySequence.result !== undefined || byEvent.result !== undefined) {
                      const stored = validateBrowserInboxRecord(bySequence.result, scope), sameEvent = validateBrowserInboxRecord(byEvent.result, scope)
                      if (stored.event_id !== event || sameEvent.server_sequence !== seq
                        || immutableWireIdentity(wireJSON(stored.wire_item)) !== immutableWireIdentity(item)) failure('conflicting_record')
                      replayed++
                    } else {
                      if (replay) failure('conflicting_record')
                      store.add({ record_version: 1, account_id: scope.accountId, device_id: scope.deviceId, event_id: event,
                        server_sequence: seq, protocol_version: page.protocol_version, state: 'received', wire_item: JSON.stringify(item) } satisfies BrowserReceivedInboxRecord)
                      added++
                    }
                    next(index + 1)
                  } catch (error) { fail(error) }
                }
                bySequence.onsuccess = finish
                byEvent.onsuccess = finish
                outbound.onsuccess = finish; proof.onsuccess = finish
              }
              next(0)
            } catch (error) { fail(error) }
          }
        } catch (error) { fail(error) }
      }, fail)
    })
  }
}
