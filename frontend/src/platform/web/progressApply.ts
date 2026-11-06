import type { ProgressChain } from '@/cloud/progressTransition'
import { encryptObjectBytes, decryptObjectBytes, type AccountMasterKey } from '@/crypto'
import { encodeBase64Url, decodeBase64Url } from '@/api/base64url'
import { canonical } from '@/cloud/projectMetadataCodec'
import { BrowserPersistenceError } from './persistence'
import { prepareProgressEvent, inspectPreparedProgressEvent, type PreparedProgressEvent } from './progressPrepare'
import type { BrowserPrefixInventory } from './provenPrefixRecords'
import { validateBrowserInboxRecord, type BrowserSyncScope, type BrowserSyncTransaction } from './syncDurability'

import { joinProgressRecords, progressDigest, type ProgressInbox, type ProgressLedger as ProgressGenesisLedger, type ProgressProjection } from './progressRecords'
export type { ProgressLedger as ProgressGenesisLedger, ProgressProjection } from './progressRecords'
const fail = (): never => { throw new BrowserPersistenceError('corrupt_record') }

/** Transaction adapter; caller must supply the replay CAS transaction and its inventory reader. */
export async function commitProgressEvent(transaction: BrowserSyncTransaction, scope: BrowserSyncScope,
  token: PreparedProgressEvent, expected: BrowserPrefixInventory, amk: AccountMasterKey,
  current: () => boolean, readInventory: (tx: IDBTransaction, done: (inventory: BrowserPrefixInventory) => void, abort: (error: unknown) => void) => void): Promise<'applied' | 'already_applied'> {
  const plan = inspectPreparedProgressEvent(token, scope, expected), h = plan.event.header
  const ledger: ProgressGenesisLedger = { account_id: scope.accountId, device_id: scope.deviceId, project_id: h.project_id,
    entity_id: h.entity_id, event_id: h.event_id, server_sequence: plan.serverSequence, outcome: 'applied', wire_item: plan.wireItem }
  const bytes = new TextEncoder().encode(canonical({ chain: plan.projection, event_id: h.event_id, server_sequence: plan.serverSequence }))
  let projection: ProgressProjection
  try {
    const sealed = await encryptObjectBytes(amk, { userId: scope.canonicalUserId, projectId: h.project_id, entityId: h.entity_id, entityType: 'progress' }, bytes)
    try { projection = { account_id: scope.accountId, device_id: scope.deviceId, project_id: h.project_id, entity_id: h.entity_id, head_event_id: h.event_id,
      envelope: { crypto_version: 1, aad_version: 1, nonce: encodeBase64Url(sealed.nonce), ciphertext: encodeBase64Url(sealed.ciphertext) } } }
    finally { sealed.nonce.fill(0); sealed.ciphertext.fill(0) }
  } finally { bytes.fill(0) }
  // Read the authenticated parent projection through the same full replay CAS.
  // It remains encrypted, byte-for-byte, inside the new append-only ledger row.
  if (h.parents.length) {
    ledger.previous_projection = await transaction(scope, ['progressProjection'], 'readonly', (tx, done, abort) => {
      const read = tx.objectStore('progressProjection').get([scope.accountId, h.project_id, h.entity_id])
      read.onsuccess = () => { try { const previous = read.result as ProgressProjection | undefined
        if (!previous || previous.head_event_id !== h.parents[0]) fail()
        done(structuredClone(previous!))
      } catch (error) { abort(error) } }
    })
  }
  const proof = { record_version: 1 as const, owner_kind: 'progress' as const, outcome: 'applied' as const, ledger_digest: await progressDigest(ledger), projection_digest: await progressDigest(projection) }
  if (!current()) throw new BrowserPersistenceError('closed')
  return transaction(scope, ['progressLedger', 'progressProjection', 'inbox', 'cursors', 'ownerHeads', 'projectBindings', 'outbox', 'receipts', 'legacyNoteHistory'], 'readwrite', (tx, result, abort) => {
    readInventory(tx, inventory => {
      try {
        inspectPreparedProgressEvent(token, scope, inventory)
        if (!current()) throw new BrowserPersistenceError('closed')
        const rows = inventory.rows.map(row => validateBrowserInboxRecord(row, scope))
        const found = rows.find(row => row.server_sequence === plan.serverSequence)
        const sourceRow = found?.state === 'received' ? found : undefined
        if (!sourceRow || sourceRow.state !== 'received' || sourceRow.protocol_version !== 3
          || sourceRow.event_id !== h.event_id || sourceRow.wire_item !== plan.wireItem) fail()
        const ledgers = tx.objectStore('progressLedger'), projections = tx.objectStore('progressProjection')
        const old = ledgers.get([scope.accountId, h.event_id]), source = projections.getAll(IDBKeyRange.bound([scope.accountId, h.project_id], [scope.accountId, h.project_id, []]))
        let loaded = 0
        const finish = () => {
          if (++loaded !== 2) return
          try {
            if (!current()) throw new BrowserPersistenceError('closed')
            if (old.result) fail()
            const selected = (source.result as ProgressProjection[]).filter(value => value.entity_id === h.entity_id)
            if (h.operation === 'genesis' ? selected.length !== 0
              : selected.length !== 1 || canonical(selected[0]) !== canonical(ledger.previous_projection)) fail()
            ledgers.add(ledger)
            if (h.operation === 'genesis') projections.add(projection)
            else projections.put(projection)
            tx.objectStore('inbox').put({ ...sourceRow!, record_version: 13, state: 'progress_proven', progress_proof: proof } satisfies ProgressInbox)
            result('applied')
          } catch (error) { abort(error) }
        }
        old.onsuccess = finish; source.onsuccess = finish
      } catch (error) { abort(error) }
    }, abort)
  })
}

/** Verify encrypted projection against a freshly authenticated event token. */
export async function authenticateProgressProjection(scope: BrowserSyncScope, token: PreparedProgressEvent,
  inventory: BrowserPrefixInventory, ledger: ProgressGenesisLedger, projection: ProgressProjection, amk: AccountMasterKey) {
  const plan = inspectPreparedProgressEvent(token, scope, inventory), h = plan.event.header
  if (ledger.account_id !== scope.accountId || ledger.device_id !== scope.deviceId || ledger.project_id !== h.project_id
    || ledger.entity_id !== h.entity_id || ledger.event_id !== h.event_id || ledger.server_sequence !== plan.serverSequence
    || ledger.outcome !== 'applied' || ledger.wire_item !== plan.wireItem || projection.account_id !== scope.accountId
    || projection.device_id !== scope.deviceId || projection.project_id !== h.project_id || projection.entity_id !== h.entity_id || projection.head_event_id !== h.event_id) fail()
  const nonce = decodeBase64Url(projection.envelope.nonce), ciphertext = decodeBase64Url(projection.envelope.ciphertext)
  let bytes: Uint8Array | undefined
  try {
    bytes = await decryptObjectBytes(amk, { userId: scope.canonicalUserId, projectId: h.project_id, entityId: h.entity_id, entityType: 'progress' }, { ...projection.envelope, nonce, ciphertext })
    if (new TextDecoder('utf-8', { fatal: true }).decode(bytes) !== canonical({ chain: plan.projection, event_id: h.event_id, server_sequence: plan.serverSequence })) fail()
    inspectPreparedProgressEvent(token, scope, inventory)
    return structuredClone(plan.projection)
  } finally { nonce.fill(0); ciphertext.fill(0); bytes?.fill(0) }
}

export function progressPreparationInventory(snapshot: import('./replayInventory').ReplaySnapshot): BrowserPrefixInventory {
  const c=snapshot.checkpoint
  if(!c) throw new BrowserPersistenceError('corrupt_record')
  return { rows:snapshot.rows, out:snapshot.author?.out??[], receipts:snapshot.author?.receipts??[], bindings:snapshot.bindings,
    owners:snapshot.owners, history:snapshot.noteHistory??[], cursor:{pull_cursor:c.pull_cursor,ack_cursor:c.ack_cursor,proven_prefix:c.proven_prefix,next_local_ordinal:c.next_local_ordinal} }
}

/** Reauthenticate durable outcomes using the same source/ledger/projection checks
 * for recovery, source reimport and unlocked presentation. Digests alone never
 * authorize a row. The caller retains its full snapshot CAS/lifetime check. */
export async function authenticateReplayProgress(scope: BrowserSyncScope,
  snapshot: import('./replayInventory').ReplaySnapshot, amk: AccountMasterKey, current: () => boolean) {
  if (!current()) throw new BrowserPersistenceError('closed')
  if (!snapshot.rows.some(row => row.state === 'progress_proven')) return []
  const inventory = progressPreparationInventory(snapshot), values: BrowserProjectProgress[] = [], verified = new Map<number, PreparedProgressEvent>()
  for (const row of snapshot.rows) if (row.state === 'progress_proven') {
    if (!current()) throw new BrowserPersistenceError('closed')
    const joined = joinProgressRecords(scope, row, snapshot.progress?.ledger ?? [], snapshot.progress?.projections ?? [])
    if (await progressDigest(joined.ledger) !== row.progress_proof.ledger_digest
      || await progressDigest(joined.projection) !== row.progress_proof.projection_digest) fail()
    const token = await prepareProgressEvent(scope, row, inventory, amk, current, verified)
    const chain = await authenticateProgressProjection(scope, token, inventory, joined.ledger, joined.projection, amk)
    if (snapshot.progress?.projections.some(value => (value as ProgressProjection).head_event_id === row.event_id)) values.push({ project_id: JSON.parse(row.wire_item).event.project_id, event_id: row.event_id, stage_id: inspectPreparedProgressEvent(token, scope, inventory).event.header.stage_id, ...chain })
  }
  if (!current()) throw new BrowserPersistenceError('closed')
  const ids = new Set<string>()
  for (const value of values) for (const entry of value.entries) {
    if (ids.has(entry.entry_id)) fail()
    ids.add(entry.entry_id)
  }
  return values
}

export interface BrowserProjectProgress extends ProgressChain { project_id: string; event_id: string; stage_id: string | null }
export const commitProgressGenesis = commitProgressEvent
