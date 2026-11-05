import type { AccountMasterKey } from '@/crypto'
import type { DocumentEvent, PortableDocument } from '@/cloud/documentCodec'
import { decodeBase64Url } from '@/api/base64url'
import { canonicalizeSyncTimestamp } from '@/cloud/syncTimestamp'
import { openBrowserCanonicalObject } from './canonicalReaders'
import { prepareMetadataPrefix, verifiedMetadataSequences } from './metadataPrefixProof'
import { validateBrowserMetadataHead } from './metadataApplyRecords'
import { validateBrowserProjectBinding } from './projectBindings'
import { BrowserPersistenceError } from './persistence'
import type { BrowserPrefixInventory } from './provenPrefixRecords'
import type { BrowserSyncScope } from './syncDurability'

export interface DocumentGenesisInput { server_sequence: number; wire_item: string }
export interface PreparedDocumentGenesis {
  readonly event: DocumentEvent
  readonly projection: PortableDocument
  readonly serverSequence: number
  readonly wireItem: string
}
const reject = (): never => { throw new BrowserPersistenceError('unsupported_local_source') }

/** Native document apply preparation only. This object is not an owner proof or ACK. */
export async function prepareDocumentGenesis(scope: BrowserSyncScope, input: DocumentGenesisInput,
  inventory: BrowserPrefixInventory, amk: AccountMasterKey, current: () => boolean): Promise<PreparedDocumentGenesis> {
  if (!current()) throw new BrowserPersistenceError('closed')
  const wire = JSON.parse(input.wire_item), descriptor = wire.event
  if (!Number.isSafeInteger(input.server_sequence) || input.server_sequence < 1 || descriptor.server_sequence !== input.server_sequence
    || descriptor.entity_type !== 'document' || descriptor.operation !== 'event' || descriptor.revision !== 1
    || descriptor.deleted_at !== null || descriptor.device_id === scope.deviceId) reject()
  const nonce = decodeBase64Url(wire.object.nonce), ciphertext = decodeBase64Url(wire.object.ciphertext)
  let event: DocumentEvent
  try {
    event = await openBrowserCanonicalObject(amk, { accountId: scope.canonicalUserId, projectId: descriptor.project_id,
      entityId: descriptor.entity_id, entityType: 'document', eventId: descriptor.event_id }, { ...wire.object, nonce, ciphertext }) as DocumentEvent
  } finally { nonce.fill(0); ciphertext.fill(0) }
  const h = event.header
  if (h.account_id !== scope.canonicalUserId || h.device_id !== descriptor.device_id || h.project_id !== descriptor.project_id
    || h.entity_id !== descriptor.entity_id || h.event_id !== descriptor.event_id || h.revision !== descriptor.revision
    || h.updated_at !== canonicalizeSyncTimestamp(descriptor.updated_at) || h.operation !== 'create' || h.generation !== 1
    || h.parents.length || h.stage_id !== null || h.stage_event_ids.length || event.mutation !== 'upsert' || !event.document) reject()
  const binding = inventory.bindings.map(value => validateBrowserProjectBinding(value, scope)).find(value => value.project_id === h.project_id)
  if (!binding || binding.mode !== 'import_remote' || binding.phase !== 'captured' || binding.remote_state !== 'active' || binding.bootstrap_id !== h.bootstrap_id) reject()
  const certificate = await prepareMetadataPrefix(scope, inventory, amk, current, true)
  const verified = verifiedMetadataSequences(certificate, scope, inventory)
  const raw = inventory.owners.find(value => (value as { owner_kind: string; owner_id: string }).owner_kind === 'project_metadata' && (value as { owner_id: string }).owner_id === h.project_id)
  if (!raw) reject()
  const head = validateBrowserMetadataHead(raw, scope), reference = head.history.find(value => value.event_id === h.metadata_event_id)
  if (!reference || head.bootstrap_id !== h.bootstrap_id || reference.operation === 'delete' || reference.server_sequence >= input.server_sequence
    || !verified.has(reference.server_sequence) || head.tip_event_ids.length !== 1
    || head.history.find(value => value.event_id === head.tip_event_ids[0])?.operation === 'delete') reject()
  if (!current()) throw new BrowserPersistenceError('closed')
  return Object.freeze({ event: structuredClone(event), projection: structuredClone(event.document!), serverSequence: input.server_sequence, wireItem: input.wire_item })
}
