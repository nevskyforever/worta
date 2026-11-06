import { encryptObjectBytes, decryptObjectBytes, type AccountMasterKey } from '@/crypto'
import { encodeBase64Url, decodeBase64Url } from '@/api/base64url'
import { canonical } from '@/cloud/projectMetadataCodec'
import { BrowserPersistenceError } from './persistence'
import { prepareMapEvent, inspectPreparedMapEvent, type PreparedMapEvent } from './mapPrepare'
import type { BrowserPrefixInventory } from './provenPrefixRecords'
import { validateBrowserInboxRecord, type BrowserSyncScope, type BrowserSyncTransaction } from './syncDurability'

import { joinMapRecords, mapDigest, type MapInbox, type MapLedger as MapGenesisLedger, type MapProjection } from './mapRecords'
export type { MapLedger as MapGenesisLedger, MapProjection } from './mapRecords'
const fail = (): never => { throw new BrowserPersistenceError('corrupt_record') }

/** Transaction adapter; caller must supply the replay CAS transaction and its inventory reader. */
export async function commitMapEvent(transaction: BrowserSyncTransaction, scope: BrowserSyncScope,
  token: PreparedMapEvent, expected: BrowserPrefixInventory, amk: AccountMasterKey,
  current: () => boolean, readInventory: (tx: IDBTransaction, done: (inventory: BrowserPrefixInventory) => void, abort: (error: unknown) => void) => void): Promise<'applied' | 'already_applied'> {
  const plan = inspectPreparedMapEvent(token, scope, expected), h = plan.event.header
  const ledger: MapGenesisLedger = { account_id: scope.accountId, device_id: scope.deviceId, project_id: h.project_id,
    entity_id: h.entity_id, event_id: h.event_id, server_sequence: plan.serverSequence, outcome: 'applied', wire_item: plan.wireItem }
  const bytes = new TextEncoder().encode(canonical({ map: plan.projection, event_id: h.event_id, server_sequence: plan.serverSequence }))
  let projection: MapProjection
  try {
    const sealed = await encryptObjectBytes(amk, { userId: scope.canonicalUserId, projectId: h.project_id, entityId: h.entity_id, entityType: 'map' }, bytes)
    try { projection = { account_id: scope.accountId, device_id: scope.deviceId, project_id: h.project_id, entity_id: h.entity_id, head_event_id: h.event_id,
      envelope: { crypto_version: 1, aad_version: 1, nonce: encodeBase64Url(sealed.nonce), ciphertext: encodeBase64Url(sealed.ciphertext) } } }
    finally { sealed.nonce.fill(0); sealed.ciphertext.fill(0) }
  } finally { bytes.fill(0) }
  // Read the authenticated parent projection through the same full replay CAS.
  // It remains encrypted, byte-for-byte, inside the new append-only ledger row.
  if (h.parents.length) {
    ledger.previous_projection = await transaction(scope, ['mapProjection'], 'readonly', (tx, done, abort) => {
      const read = tx.objectStore('mapProjection').get([scope.accountId, h.project_id, h.entity_id])
      read.onsuccess = () => { try { const previous = read.result as MapProjection | undefined
        if (!previous || previous.head_event_id !== h.parents[0]) fail()
        done(structuredClone(previous!))
      } catch (error) { abort(error) } }
    })
  }
  const proof = { record_version: 1 as const, owner_kind: 'map' as const, outcome: 'applied' as const, ledger_digest: await mapDigest(ledger), projection_digest: await mapDigest(projection) }
  if (!current()) throw new BrowserPersistenceError('closed')
  return transaction(scope, ['mapLedger', 'mapProjection', 'inbox', 'cursors', 'ownerHeads', 'projectBindings', 'outbox', 'receipts', 'legacyNoteHistory'], 'readwrite', (tx, result, abort) => {
    readInventory(tx, inventory => {
      try {
        inspectPreparedMapEvent(token, scope, inventory)
        if (!current()) throw new BrowserPersistenceError('closed')
        const rows = inventory.rows.map(row => validateBrowserInboxRecord(row, scope))
        const found = rows.find(row => row.server_sequence === plan.serverSequence)
        const sourceRow = found?.state === 'received' ? found : undefined
        if (!sourceRow || sourceRow.state !== 'received' || sourceRow.protocol_version !== 3
          || sourceRow.event_id !== h.event_id || sourceRow.wire_item !== plan.wireItem) fail()
        const ledgers = tx.objectStore('mapLedger'), projections = tx.objectStore('mapProjection')
        const old = ledgers.get([scope.accountId, h.event_id]), source = projections.getAll(IDBKeyRange.bound([scope.accountId, h.project_id], [scope.accountId, h.project_id, []]))
        let loaded = 0
        const finish = () => {
          if (++loaded !== 2) return
          try {
            if (!current()) throw new BrowserPersistenceError('closed')
            if (old.result) fail()
            const selected = (source.result as MapProjection[]).filter(value => value.entity_id === h.entity_id)
            if (h.operation === 'create' ? selected.length !== 0
              : selected.length !== 1 || canonical(selected[0]) !== canonical(ledger.previous_projection)) fail()
            ledgers.add(ledger)
            if (h.operation === 'create') projections.add(projection)
            else projections.put(projection)
            tx.objectStore('inbox').put({ ...sourceRow!, record_version: 12, state: 'map_proven', map_proof: proof } satisfies MapInbox)
            result('applied')
          } catch (error) { abort(error) }
        }
        old.onsuccess = finish; source.onsuccess = finish
      } catch (error) { abort(error) }
    }, abort)
  })
}

/** Verify encrypted projection against a freshly authenticated event token. */
export async function authenticateMapProjection(scope: BrowserSyncScope, token: PreparedMapEvent,
  inventory: BrowserPrefixInventory, ledger: MapGenesisLedger, projection: MapProjection, amk: AccountMasterKey) {
  const plan = inspectPreparedMapEvent(token, scope, inventory), h = plan.event.header
  if (ledger.account_id !== scope.accountId || ledger.device_id !== scope.deviceId || ledger.project_id !== h.project_id
    || ledger.entity_id !== h.entity_id || ledger.event_id !== h.event_id || ledger.server_sequence !== plan.serverSequence
    || ledger.outcome !== 'applied' || ledger.wire_item !== plan.wireItem || projection.account_id !== scope.accountId
    || projection.device_id !== scope.deviceId || projection.project_id !== h.project_id || projection.entity_id !== h.entity_id || projection.head_event_id !== h.event_id) fail()
  const nonce = decodeBase64Url(projection.envelope.nonce), ciphertext = decodeBase64Url(projection.envelope.ciphertext)
  let bytes: Uint8Array | undefined
  try {
    bytes = await decryptObjectBytes(amk, { userId: scope.canonicalUserId, projectId: h.project_id, entityId: h.entity_id, entityType: 'map' }, { ...projection.envelope, nonce, ciphertext })
    if (new TextDecoder('utf-8', { fatal: true }).decode(bytes) !== canonical({ map: plan.projection, event_id: h.event_id, server_sequence: plan.serverSequence })) fail()
    inspectPreparedMapEvent(token, scope, inventory)
    return structuredClone(plan.projection)
  } finally { nonce.fill(0); ciphertext.fill(0); bytes?.fill(0) }
}

export function mapPreparationInventory(snapshot: import('./replayInventory').ReplaySnapshot): BrowserPrefixInventory {
  const c=snapshot.checkpoint
  if(!c) throw new BrowserPersistenceError('corrupt_record')
  return { rows:snapshot.rows, out:snapshot.author?.out??[], receipts:snapshot.author?.receipts??[], bindings:snapshot.bindings,
    owners:snapshot.owners, history:snapshot.noteHistory??[], cursor:{pull_cursor:c.pull_cursor,ack_cursor:c.ack_cursor,proven_prefix:c.proven_prefix,next_local_ordinal:c.next_local_ordinal} }
}

/** Reauthenticate durable outcomes using the same source/ledger/projection checks
 * for recovery, source reimport and unlocked presentation. Digests alone never
 * authorize a row. The caller retains its full snapshot CAS/lifetime check. */
export async function authenticateReplayMaps(scope: BrowserSyncScope,
  snapshot: import('./replayInventory').ReplaySnapshot, amk: AccountMasterKey, current: () => boolean) {
  if (!current()) throw new BrowserPersistenceError('closed')
  if (!snapshot.rows.some(row => row.state === 'map_proven')) return []
  const inventory = mapPreparationInventory(snapshot), maps: BrowserProjectMap[] = [], verified = new Map<number, PreparedMapEvent>()
  for (const row of snapshot.rows) if (row.state === 'map_proven') {
    if (!current()) throw new BrowserPersistenceError('closed')
    const joined = joinMapRecords(scope, row, snapshot.maps?.ledger ?? [], snapshot.maps?.projections ?? [])
    if (await mapDigest(joined.ledger) !== row.map_proof.ledger_digest
      || await mapDigest(joined.projection) !== row.map_proof.projection_digest) fail()
    const token = await prepareMapEvent(scope, row, inventory, amk, current, verified)
    const map = await authenticateMapProjection(scope, token, inventory, joined.ledger, joined.projection, amk)
    if (snapshot.maps?.projections.some(value => (value as MapProjection).head_event_id === row.event_id)) maps.push({ project_id: JSON.parse(row.wire_item).event.project_id, event_id: row.event_id, stage_id: inspectPreparedMapEvent(token, scope, inventory).event.header.stage_id, ...map })
  }
  if (!current()) throw new BrowserPersistenceError('closed')
  return maps
}

export interface BrowserProjectMap { project_id: string; event_id: string; stage_id: string | null; data: Record<string, unknown>; annotations: Record<string, unknown> }
export const commitMapGenesis = commitMapEvent
