import { encodeV3ProgressPush } from '@/api/encryptedSyncV3'
import { encryptObjectBytes, type AccountMasterKey } from '@/crypto'
import { frameProgressEvent, type ProgressEvent } from '@/cloud/progressCodec'
import { snapshotBrowserSyncScope, type BrowserSyncScope } from './syncDurability'

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
