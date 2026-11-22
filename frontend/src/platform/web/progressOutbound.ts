import { encodeV3ProgressPush } from '@/api/encryptedSyncV3'
import { decryptObjectBytes, encryptObjectBytes, type AccountMasterKey } from '@/crypto'
import { decodeBase64Url, encodeBase64Url } from '@/api/base64url'
import { frameProgressEvent, type ProgressEvent } from '@/cloud/progressCodec'
import { canonical } from '@/cloud/projectMetadataCodec'
import { validateLocalEnvelope, type LocalEnvelope } from './localAuthorityRecords'
import { progressDigest } from './progressRecords'
import { BrowserPersistenceError } from './persistence'
import { snapshotBrowserSyncScope, validateBrowserInboxRecord, validateBrowserRetainedHistory,
  type BrowserOutboxRecord, type BrowserSyncScope, type BrowserUploadReceipt } from './syncDurability'

/** Seal a prepared event using the existing v3 format. The caller must first
 * authenticate the parent and dependencies. This function grants no authority,
 * performs no I/O and does not itself add a record to the outbox. */
export async function sealProgressForOutbox(scope: BrowserSyncScope, event: ProgressEvent, amk: AccountMasterKey): Promise<string> {
  scope = snapshotBrowserSyncScope(scope)
  event = structuredClone(event)
  const h = event.header
  if (h.account_id !== scope.canonicalUserId || h.device_id !== scope.deviceId
    || !['append', 'tombstone'].includes(h.operation)) throw new Error('progress_scope_mismatch')
  const frame = await frameProgressEvent(event)
  try {
    const encrypted = await encryptObjectBytes(amk, { userId: scope.canonicalUserId, projectId: h.project_id,
      entityId: h.entity_id, entityType: 'progress' }, frame)
    try {
      return encodeV3ProgressPush(scope.deviceId, [{
        event: { event_id: h.event_id, project_id: h.project_id, entity_id: h.entity_id, entity_type: 'progress',
          operation: 'event', revision: h.revision, updated_at: h.updated_at, deleted_at: null },
        object: encrypted,
      }])
    } finally { encrypted.nonce.fill(0); encrypted.ciphertext.fill(0) }
  } finally { frame.fill(0) }
}

export interface ProgressPublicationProof {
  version: 1
  envelope: LocalEnvelope
}

async function publicationEvidence(scope: BrowserSyncScope, outbound: BrowserOutboxRecord,
  receipt: BrowserUploadReceipt, inbound: unknown) {
  scope = snapshotBrowserSyncScope(scope)
  outbound = structuredClone(outbound)
  receipt = structuredClone(receipt)
  const row = validateBrowserInboxRecord(inbound, scope)
  const { event } = JSON.parse(row.wire_item)
  if (row.protocol_version !== 3 || event.entity_type !== 'progress' || event.device_id !== scope.deviceId
    || receipt.record_version !== 1 || receipt.server_sequence !== row.server_sequence) {
    throw new BrowserPersistenceError('conflicting_record')
  }
  const retained = validateBrowserRetainedHistory(scope, event, outbound, receipt, row)
  if (!retained.outbox || !retained.receipt) throw new BrowserPersistenceError('conflicting_record')
  return {
    binding: { userId: scope.canonicalUserId, projectId: event.project_id as string,
      entityId: event.entity_id as string, entityType: 'progress' as const },
    bytes: new TextEncoder().encode(canonical({ kind: 'progress_publication', version: 1, scope,
      event_id: row.event_id, outbound: await progressDigest(retained.outbox),
      receipt: await progressDigest(retained.receipt), echo: await progressDigest(row.wire_item) })),
  }
}

/** Seal transport evidence after receipt and echo verification by the caller.
 * The Progress owner must still authenticate the event and its ancestry. */
export async function sealProgressPublicationProof(scope: BrowserSyncScope, outbound: BrowserOutboxRecord,
  receipt: BrowserUploadReceipt, inbound: unknown, amk: AccountMasterKey): Promise<ProgressPublicationProof> {
  const { binding, bytes } = await publicationEvidence(scope, outbound, receipt, inbound)
  try {
    const sealed = await encryptObjectBytes(amk, binding, bytes)
    try {
      return { version: 1, envelope: { crypto_version: 1, aad_version: 1,
        nonce: encodeBase64Url(sealed.nonce), ciphertext: encodeBase64Url(sealed.ciphertext) } }
    } finally { sealed.nonce.fill(0); sealed.ciphertext.fill(0) }
  } finally { bytes.fill(0) }
}

export async function verifyProgressPublicationProof(scope: BrowserSyncScope, outbound: BrowserOutboxRecord,
  receipt: BrowserUploadReceipt, inbound: unknown, proof: ProgressPublicationProof, amk: AccountMasterKey): Promise<void> {
  if (proof.version !== 1) throw new BrowserPersistenceError('unsupported_version')
  validateLocalEnvelope(proof.envelope)
  const envelope = { ...proof.envelope, nonce: decodeBase64Url(proof.envelope.nonce),
    ciphertext: decodeBase64Url(proof.envelope.ciphertext) }
  let opened: Uint8Array | undefined
  let bytes: Uint8Array | undefined
  try {
    const evidence = await publicationEvidence(scope, outbound, receipt, inbound)
    bytes = evidence.bytes
    opened = await decryptObjectBytes(amk, evidence.binding, envelope)
    if (opened.length !== bytes.length || !opened.every((byte, i) => byte === bytes![i])) {
      throw new BrowserPersistenceError('conflicting_record')
    }
  } finally { bytes?.fill(0); opened?.fill(0); envelope.nonce.fill(0); envelope.ciphertext.fill(0) }
}
