import { requireContentStageDependency } from './contentStageDependency'
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
import { immutableWireIdentity, type BrowserSyncScope } from './syncDurability'

export interface DocumentGenesisInput { server_sequence: number; wire_item: string }
interface PreparedDocumentGenesisData {
  readonly event: DocumentEvent
  readonly projection: PortableDocument
  readonly serverSequence: number
  readonly wireItem: string
}
export interface PreparedDocumentGenesis { readonly documentGenesisToken: unique symbol }
const prepared = new WeakMap<object, { scope: string; inventory: string; current: () => boolean; data: PreparedDocumentGenesisData }>()
function reject(): never { throw new BrowserPersistenceError('unsupported_local_source') }

/** Native linear document apply preparation (genesis and sequential update). This object is not an owner proof or ACK. */
export async function prepareDocumentEvent(scope: BrowserSyncScope, input: DocumentGenesisInput,
  inventory: BrowserPrefixInventory, amk: AccountMasterKey, current: () => boolean, preparedChain = new Map<number, PreparedDocumentGenesis>()): Promise<PreparedDocumentGenesis> {
  if (!current()) throw new BrowserPersistenceError('closed')
  const cached = preparedChain.get(input.server_sequence)
  if(cached){const data=inspectPreparedDocumentGenesis(cached,scope,inventory);if(data.wireItem!==input.wire_item)reject();return cached}
  const initialInventory = immutableWireIdentity(inventory), initialScope = immutableWireIdentity(scope)
  const wire = JSON.parse(input.wire_item), descriptor = wire.event
  if (!Number.isSafeInteger(input.server_sequence) || input.server_sequence < 1 || descriptor.server_sequence !== input.server_sequence
    || descriptor.entity_type !== 'document' || descriptor.operation !== 'event' || !Number.isSafeInteger(descriptor.revision) || descriptor.revision < 1
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
    || h.updated_at !== canonicalizeSyncTimestamp(descriptor.updated_at) || !['create', 'update'].includes(h.operation) || h.generation !== h.revision
    || event.mutation !== 'upsert' || !event.document) reject()
  const binding = inventory.bindings.map(value => validateBrowserProjectBinding(value, scope)).find(value => value.project_id === h.project_id)
  if (!binding || binding.mode !== 'import_remote' || binding.phase !== 'captured' || binding.remote_state !== 'active' || binding.bootstrap_id !== h.bootstrap_id) reject()
  const dependencies = { ...inventory, rows: inventory.rows.filter(row => ['project_metadata', 'stage', 'stage_order'].includes(JSON.parse((row as {wire_item:string}).wire_item).event.entity_type)) }
  const certificate = await prepareMetadataPrefix(scope, dependencies, amk, current, true)
  const verified = verifiedMetadataSequences(certificate, scope, dependencies)
  const raw = inventory.owners.find(value => (value as { owner_kind: string; owner_id: string }).owner_kind === 'project_metadata' && (value as { owner_id: string }).owner_id === h.project_id)
  if (!raw) reject()
  const head = validateBrowserMetadataHead(raw, scope), reference = head.history.find(value => value.event_id === h.metadata_event_id)
  if (!reference || head.bootstrap_id !== h.bootstrap_id || reference.operation === 'delete' || reference.server_sequence >= input.server_sequence
    || !verified.has(reference.server_sequence) || head.tip_event_ids.length !== 1
    || head.history.find(value => value.event_id === head.tip_event_ids[0])?.operation === 'delete') reject()
  await requireContentStageDependency(scope, input, event, dependencies, amk, current, certificate)
  // Native parent_revision + full current-tip rule, restricted to a linear
  // peer chain. Parent source is freshly decrypted, not inferred from its ID.
  const prior = inventory.rows.filter(value => {
    const row = value as { state: string; server_sequence: number; wire_item: string }, e = JSON.parse(row.wire_item).event
    return row.state === 'document_proven' && row.server_sequence < input.server_sequence && e.project_id === h.project_id && e.entity_id === h.entity_id
  }).sort((a, b) => (a as DocumentGenesisInput).server_sequence - (b as DocumentGenesisInput).server_sequence)
  if (h.operation === 'create') {
    if (h.revision !== 1 || h.parents.length || prior.length) reject()
  } else {
    const parent = prior.at(-1) as (DocumentGenesisInput & { event_id: string }) | undefined
    if (!parent || h.parents.length !== 1 || h.parents[0] !== parent.event_id) reject()
    const token = await prepareDocumentEvent(scope, parent, inventory, amk, current, preparedChain)
    const previous = inspectPreparedDocumentGenesis(token, scope, inventory).event.header
    if (previous.stage_id !== h.stage_id || previous.entity_id !== h.entity_id || previous.project_id !== h.project_id || previous.bootstrap_id !== h.bootstrap_id
      || h.revision !== previous.revision + 1 || h.generation !== previous.generation + 1) reject()
  }
  if (!current()) throw new BrowserPersistenceError('closed')
  if (initialInventory !== immutableWireIdentity(inventory) || initialScope !== immutableWireIdentity(scope)) throw new BrowserPersistenceError('stale_source')
  const token = Object.freeze({}) as PreparedDocumentGenesis
  prepared.set(token, { scope: initialScope, inventory: initialInventory, current,
    data: { event: structuredClone(event), projection: structuredClone(event.document!), serverSequence: input.server_sequence, wireItem: input.wire_item } })
  preparedChain.set(input.server_sequence, token)
  return token
}

/** Validate against the freshly loaded inventory before any future owner commit. */
export function inspectPreparedDocumentGenesis(token: PreparedDocumentGenesis, scope: BrowserSyncScope, inventory: BrowserPrefixInventory): PreparedDocumentGenesisData {
  const plan = prepared.get(token)
  if (!plan || !plan.current()) throw new BrowserPersistenceError('closed')
  if (plan.scope !== immutableWireIdentity(scope) || plan.inventory !== immutableWireIdentity(inventory)) throw new BrowserPersistenceError('stale_source')
  return structuredClone(plan.data)
}

// Retain the accepted genesis preparation entry point for existing callers.
export const prepareDocumentGenesis = prepareDocumentEvent
