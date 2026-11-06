import { encryptObjectBytes, decryptObjectBytes, type AccountMasterKey } from '@/crypto'
import { encodeBase64Url, decodeBase64Url } from '@/api/base64url'
import { canonical } from '@/cloud/projectMetadataCodec'
import { BrowserPersistenceError } from './persistence'
import { prepareDocumentEvent, inspectPreparedDocumentGenesis, type PreparedDocumentGenesis } from './documentPrepare'
import type { BrowserPrefixInventory } from './provenPrefixRecords'
import { validateBrowserInboxRecord, type BrowserSyncScope, type BrowserSyncTransaction } from './syncDurability'

import { joinDocumentRecords, documentDigest, type DocumentInbox, type DocumentLedger as DocumentGenesisLedger, type DocumentProjection } from './documentRecords'
export type { DocumentLedger as DocumentGenesisLedger, DocumentProjection } from './documentRecords'
const fail = (): never => { throw new BrowserPersistenceError('corrupt_record') }

/** Transaction adapter; caller must supply the replay CAS transaction and its inventory reader. */
export async function commitDocumentEvent(transaction: BrowserSyncTransaction, scope: BrowserSyncScope,
  token: PreparedDocumentGenesis, expected: BrowserPrefixInventory, amk: AccountMasterKey,
  current: () => boolean, readInventory: (tx: IDBTransaction, done: (inventory: BrowserPrefixInventory) => void, abort: (error: unknown) => void) => void): Promise<'applied' | 'already_applied'> {
  const plan = inspectPreparedDocumentGenesis(token, scope, expected), h = plan.event.header
  const ledger: DocumentGenesisLedger = { account_id: scope.accountId, device_id: scope.deviceId, project_id: h.project_id,
    entity_id: h.entity_id, event_id: h.event_id, server_sequence: plan.serverSequence, outcome: 'applied', wire_item: plan.wireItem }
  const bytes = new TextEncoder().encode(canonical({ document: plan.projection, event_id: h.event_id, server_sequence: plan.serverSequence }))
  let projection: DocumentProjection
  try {
    const sealed = await encryptObjectBytes(amk, { userId: scope.canonicalUserId, projectId: h.project_id, entityId: h.entity_id, entityType: 'document' }, bytes)
    try { projection = { account_id: scope.accountId, device_id: scope.deviceId, project_id: h.project_id, entity_id: h.entity_id, head_event_id: h.event_id,
      envelope: { crypto_version: 1, aad_version: 1, nonce: encodeBase64Url(sealed.nonce), ciphertext: encodeBase64Url(sealed.ciphertext) } } }
    finally { sealed.nonce.fill(0); sealed.ciphertext.fill(0) }
  } finally { bytes.fill(0) }
  // Read the authenticated parent projection through the same full replay CAS.
  // It remains encrypted, byte-for-byte, inside the new append-only ledger row.
  if (h.parents.length) {
    ledger.previous_projection = await transaction(scope, ['documentProjection'], 'readonly', (tx, done, abort) => {
      const read = tx.objectStore('documentProjection').get([scope.accountId, h.project_id, h.entity_id])
      read.onsuccess = () => { try { const previous = read.result as DocumentProjection | undefined
        if (!previous || previous.head_event_id !== h.parents[0]) fail()
        done(structuredClone(previous!))
      } catch (error) { abort(error) } }
    })
  }
  const proof = { record_version: 1 as const, owner_kind: 'document' as const, outcome: 'applied' as const, ledger_digest: await documentDigest(ledger), projection_digest: await documentDigest(projection) }
  if (!current()) throw new BrowserPersistenceError('closed')
  return transaction(scope, ['documentLedger', 'documentProjection', 'inbox', 'cursors', 'ownerHeads', 'projectBindings', 'outbox', 'receipts', 'legacyNoteHistory'], 'readwrite', (tx, result, abort) => {
    readInventory(tx, inventory => {
      try {
        inspectPreparedDocumentGenesis(token, scope, inventory)
        if (!current()) throw new BrowserPersistenceError('closed')
        const rows = inventory.rows.map(row => validateBrowserInboxRecord(row, scope))
        const found = rows.find(row => row.server_sequence === plan.serverSequence)
        const sourceRow = found?.state === 'received' ? found : undefined
        if (!sourceRow || sourceRow.state !== 'received' || sourceRow.protocol_version !== 3
          || sourceRow.event_id !== h.event_id || sourceRow.wire_item !== plan.wireItem) fail()
        const ledgers = tx.objectStore('documentLedger'), projections = tx.objectStore('documentProjection')
        const old = ledgers.get([scope.accountId, h.event_id]), source = projections.getAll(IDBKeyRange.bound([scope.accountId, h.project_id], [scope.accountId, h.project_id, []]))
        let loaded = 0
        const finish = () => {
          if (++loaded !== 2) return
          try {
            if (!current()) throw new BrowserPersistenceError('closed')
            if (old.result) fail()
            const selected = (source.result as DocumentProjection[]).filter(value => value.entity_id === h.entity_id)
            if (h.operation === 'create' ? selected.length !== 0
              : selected.length !== 1 || canonical(selected[0]) !== canonical(ledger.previous_projection)) fail()
            ledgers.add(ledger)
            if (h.operation === 'create') projections.add(projection)
            else projections.put(projection)
            tx.objectStore('inbox').put({ ...sourceRow!, record_version: 11, state: 'document_proven', document_proof: proof } satisfies DocumentInbox)
            result('applied')
          } catch (error) { abort(error) }
        }
        old.onsuccess = finish; source.onsuccess = finish
      } catch (error) { abort(error) }
    }, abort)
  })
}

/** Verify encrypted projection against a freshly authenticated event token. */
export async function authenticateDocumentProjection(scope: BrowserSyncScope, token: PreparedDocumentGenesis,
  inventory: BrowserPrefixInventory, ledger: DocumentGenesisLedger, projection: DocumentProjection, amk: AccountMasterKey) {
  const plan = inspectPreparedDocumentGenesis(token, scope, inventory), h = plan.event.header
  if (ledger.account_id !== scope.accountId || ledger.device_id !== scope.deviceId || ledger.project_id !== h.project_id
    || ledger.entity_id !== h.entity_id || ledger.event_id !== h.event_id || ledger.server_sequence !== plan.serverSequence
    || ledger.outcome !== 'applied' || ledger.wire_item !== plan.wireItem || projection.account_id !== scope.accountId
    || projection.device_id !== scope.deviceId || projection.project_id !== h.project_id || projection.entity_id !== h.entity_id || projection.head_event_id !== h.event_id) fail()
  const nonce = decodeBase64Url(projection.envelope.nonce), ciphertext = decodeBase64Url(projection.envelope.ciphertext)
  let bytes: Uint8Array | undefined
  try {
    bytes = await decryptObjectBytes(amk, { userId: scope.canonicalUserId, projectId: h.project_id, entityId: h.entity_id, entityType: 'document' }, { ...projection.envelope, nonce, ciphertext })
    if (new TextDecoder('utf-8', { fatal: true }).decode(bytes) !== canonical({ document: plan.projection, event_id: h.event_id, server_sequence: plan.serverSequence })) fail()
    inspectPreparedDocumentGenesis(token, scope, inventory)
    return structuredClone(plan.projection)
  } finally { nonce.fill(0); ciphertext.fill(0); bytes?.fill(0) }
}

export function documentPreparationInventory(snapshot: import('./replayInventory').ReplaySnapshot): BrowserPrefixInventory {
  const c=snapshot.checkpoint
  if(!c) throw new BrowserPersistenceError('corrupt_record')
  return { rows:snapshot.rows, out:snapshot.author?.out??[], receipts:snapshot.author?.receipts??[], bindings:snapshot.bindings,
    owners:snapshot.owners, history:snapshot.noteHistory??[], cursor:{pull_cursor:c.pull_cursor,ack_cursor:c.ack_cursor,proven_prefix:c.proven_prefix,next_local_ordinal:c.next_local_ordinal} }
}

/** Reauthenticate durable outcomes using the same source/ledger/projection checks
 * for recovery, source reimport and unlocked presentation. Digests alone never
 * authorize a row. The caller retains its full snapshot CAS/lifetime check. */
export async function authenticateReplayDocuments(scope: BrowserSyncScope,
  snapshot: import('./replayInventory').ReplaySnapshot, amk: AccountMasterKey, current: () => boolean) {
  if (!current()) throw new BrowserPersistenceError('closed')
  if (!snapshot.rows.some(row => row.state === 'document_proven')) return []
  const inventory = documentPreparationInventory(snapshot), documents = [], verified = new Map<number, PreparedDocumentGenesis>()
  for (const row of snapshot.rows) if (row.state === 'document_proven') {
    if (!current()) throw new BrowserPersistenceError('closed')
    const joined = joinDocumentRecords(scope, row, snapshot.documents?.ledger ?? [], snapshot.documents?.projections ?? [])
    if (await documentDigest(joined.ledger) !== row.document_proof.ledger_digest
      || await documentDigest(joined.projection) !== row.document_proof.projection_digest) fail()
    const token = await prepareDocumentEvent(scope, row, inventory, amk, current, verified)
    const document = await authenticateDocumentProjection(scope, token, inventory, joined.ledger, joined.projection, amk)
    if (snapshot.documents?.projections.some(value => (value as DocumentProjection).head_event_id === row.event_id)) documents.push(document)
  }
  const slots = new Set<string>()
  for (const document of documents) {
    const slot = JSON.stringify([document.project_id, document.stage_id])
    if (slots.has(slot)) fail()
    slots.add(slot)
  }
  if (!current()) throw new BrowserPersistenceError('closed')
  return documents
}

export const commitDocumentGenesis = commitDocumentEvent
