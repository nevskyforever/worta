import type { BrowserSyncScope } from './syncDurability'
import { encodeV3DocumentPush } from '@/api/encryptedSyncV3'
import { encryptObjectBytes, type AccountMasterKey } from '@/crypto'
import { frameDocumentEvent, type DocumentEvent } from '@/cloud/documentCodec'

/** Seal a prepared event using the existing v3 format. The caller must first
 * authenticate the parent and dependencies. This function grants no authority,
 * performs no I/O and does not itself add a record to the outbox. */
export async function sealDocumentForOutbox(scope: BrowserSyncScope, event: DocumentEvent, amk: AccountMasterKey): Promise<string> {
  event = structuredClone(event)
  const h = event.header
  if (h.account_id !== scope.canonicalUserId || h.device_id !== scope.deviceId
    || !['create', 'update', 'resolution'].includes(h.operation)) throw new Error('document_scope_mismatch')
  const frame = await frameDocumentEvent(event)
  try {
    const encrypted = await encryptObjectBytes(amk, { userId: scope.canonicalUserId, projectId: h.project_id,
      entityId: h.entity_id, entityType: 'document' }, frame)
    try {
      return encodeV3DocumentPush(scope.deviceId, [{
        event: { event_id: h.event_id, project_id: h.project_id, entity_id: h.entity_id, entity_type: 'document',
          operation: 'event', revision: h.revision, updated_at: h.updated_at, deleted_at: null },
        object: encrypted,
      }])
    } finally { encrypted.nonce.fill(0); encrypted.ciphertext.fill(0) }
  } finally { frame.fill(0) }
}

/** Use the Desktop ID and suffix convention, reserving IDs from prior cloud history. */
export async function newCloudDocumentId(projectId: string, stageId: string | null, allocated: ReadonlySet<string>) {
  const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(`${projectId}:${stageId ?? 'project'}`))
  const base = 'document-' + Array.from(new Uint8Array(digest).slice(0, 20), byte => byte.toString(16).padStart(2, '0')).join('')
  for (let index = 0; index < 4096; index++) {
    const candidate = index === 0 ? base : `${base}-${index}`
    if (!allocated.has(candidate)) return candidate
  }
  throw new Error('document_identity_collision')
}
