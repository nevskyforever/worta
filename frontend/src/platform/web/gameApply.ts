import { gameOwner, gameProject, type GameState } from '@/cloud/gameProjectTransition'
import type { GameEvent } from '@/cloud/gameCodec'
import { encryptAccountObject, decryptAccountObject } from '@/crypto/accountObjectCrypto'
import { encryptObjectBytes, decryptObjectBytes, type AccountMasterKey } from '@/crypto'
import { encodeBase64Url, decodeBase64Url } from '@/api/base64url'
import { canonical } from '@/cloud/projectMetadataCodec'
import { BrowserPersistenceError } from './persistence'
import { prepareGameEvent, inspectPreparedGameEvent, type PreparedGameEvent } from './gamePrepare'
import type { ReplaySnapshot } from './replayInventory'
import { validateBrowserInboxRecord, type BrowserSyncScope, type BrowserSyncTransaction } from './syncDurability'

import { joinGameRecords, gameDigest, type GameInbox, type GameLedger as GameGenesisLedger, type GameProjection } from './gameRecords'
export type { GameLedger as GameGenesisLedger, GameProjection } from './gameRecords'
const fail = (): never => { throw new BrowserPersistenceError('corrupt_record') }

/** Transaction adapter; caller must supply the replay CAS transaction and its inventory reader. */
export async function commitGameEvent(transaction: BrowserSyncTransaction, scope: BrowserSyncScope,
  token: PreparedGameEvent, expected: ReplaySnapshot, amk: AccountMasterKey,
  current: () => boolean, readInventory: (tx: IDBTransaction, done: (inventory: ReplaySnapshot) => void, abort: (error: unknown) => void) => void): Promise<'applied' | 'already_applied'> {
  const plan = inspectPreparedGameEvent(token, scope, expected), h = plan.event.header
  const ledger: GameGenesisLedger = { account_id: scope.accountId, device_id: scope.deviceId, project_id: gameProject(plan.event),
    entity_id: gameOwner(plan.event), event_id: h.event_id, server_sequence: plan.serverSequence, outcome: 'applied', wire_item: plan.wireItem }
  const bytes = new TextEncoder().encode(canonical({ chain: plan.projection, event_id: h.event_id, server_sequence: plan.serverSequence }))
  let projection: GameProjection
  try {
    const sealed = h.scope === 'account'
      ? await encryptAccountObject(amk, {userId:scope.canonicalUserId,scope:'account',entityId:gameOwner(plan.event),entityType:'account_game'}, bytes)
      : await encryptObjectBytes(amk, { userId: scope.canonicalUserId, projectId: h.project_id, entityId: gameOwner(plan.event), entityType: 'project_game' }, bytes)
    try { projection = { account_id: scope.accountId, device_id: scope.deviceId, project_id: gameProject(plan.event), entity_id: gameOwner(plan.event), head_event_id: h.event_id,
      envelope: { crypto_version: sealed.crypto_version, aad_version: sealed.aad_version, nonce: encodeBase64Url(sealed.nonce), ciphertext: encodeBase64Url(sealed.ciphertext) } } }
    finally { sealed.nonce.fill(0); sealed.ciphertext.fill(0) }
  } finally { bytes.fill(0) }
  // Read the authenticated parent projection through the same full replay CAS.
  // It remains encrypted, byte-for-byte, inside the new append-only ledger row.
  if (h.parents.length) {
    ledger.previous_projection = await transaction(scope, ['gameProjection'], 'readonly', (tx, done, abort) => {
      const read = tx.objectStore('gameProjection').get([scope.accountId, gameProject(plan.event), gameOwner(plan.event)])
      read.onsuccess = () => { try { const previous = read.result as GameProjection | undefined
        if (!previous || previous.head_event_id !== h.parents[0]) fail()
        done(structuredClone(previous!))
      } catch (error) { abort(error) } }
    })
  }
  const proof = { record_version: 1 as const, owner_kind: 'game' as const, outcome: 'applied' as const, ledger_digest: await gameDigest(ledger), projection_digest: await gameDigest(projection) }
  if (!current()) throw new BrowserPersistenceError('closed')
  return transaction(scope, ['gameLedger', 'gameProjection', 'inbox', 'cursors', 'ownerHeads', 'projectBindings', 'outbox', 'receipts', 'legacyNoteHistory'], 'readwrite', (tx, result, abort) => {
    readInventory(tx, inventory => {
      try {
        inspectPreparedGameEvent(token, scope, inventory)
        if (!current()) throw new BrowserPersistenceError('closed')
        const rows = inventory.rows.map(row => validateBrowserInboxRecord(row, scope))
        const found = rows.find(row => row.server_sequence === plan.serverSequence)
        const sourceRow = found?.state === 'received' ? found : undefined
        if (!sourceRow || sourceRow.state !== 'received' || sourceRow.protocol_version !== 3
          || sourceRow.event_id !== h.event_id || sourceRow.wire_item !== plan.wireItem) fail()
        const ledgers = tx.objectStore('gameLedger'), projections = tx.objectStore('gameProjection')
        const old = ledgers.get([scope.accountId, h.event_id]), source = projections.getAll(IDBKeyRange.bound([scope.accountId, gameProject(plan.event)], [scope.accountId, gameProject(plan.event), []]))
        let loaded = 0
        const finish = () => {
          if (++loaded !== 2) return
          try {
            if (!current()) throw new BrowserPersistenceError('closed')
            if (old.result) fail()
            const selected = (source.result as GameProjection[]).filter(value => value.entity_id === gameOwner(plan.event))
            if (plan.event.action.kind === 'genesis' ? selected.length !== 0
              : selected.length !== 1 || canonical(selected[0]) !== canonical(ledger.previous_projection)) fail()
            ledgers.add(ledger)
            if (plan.event.action.kind === 'genesis') projections.add(projection)
            else projections.put(projection)
            tx.objectStore('inbox').put({ ...sourceRow!, record_version: 14, state: 'game_proven', game_proof: proof } satisfies GameInbox)
            result('applied')
          } catch (error) { abort(error) }
        }
        old.onsuccess = finish; source.onsuccess = finish
      } catch (error) { abort(error) }
    }, abort)
  })
}

/** Verify encrypted projection against a freshly authenticated event token. */
export async function authenticateGameProjection(scope: BrowserSyncScope, token: PreparedGameEvent,
  inventory: ReplaySnapshot, ledger: GameGenesisLedger, projection: GameProjection, amk: AccountMasterKey) {
  const plan = inspectPreparedGameEvent(token, scope, inventory), h = plan.event.header
  if (ledger.account_id !== scope.accountId || ledger.device_id !== scope.deviceId || ledger.project_id !== gameProject(plan.event)
    || ledger.entity_id !== gameOwner(plan.event) || ledger.event_id !== h.event_id || ledger.server_sequence !== plan.serverSequence
    || ledger.outcome !== 'applied' || ledger.wire_item !== plan.wireItem || projection.account_id !== scope.accountId
    || projection.device_id !== scope.deviceId || projection.project_id !== gameProject(plan.event) || projection.entity_id !== gameOwner(plan.event) || projection.head_event_id !== h.event_id) fail()
  const nonce = decodeBase64Url(projection.envelope.nonce), ciphertext = decodeBase64Url(projection.envelope.ciphertext)
  let bytes: Uint8Array | undefined
  try {
    bytes = h.scope === 'account'
      ? await decryptAccountObject(amk, {userId:scope.canonicalUserId,scope:'account',entityId:gameOwner(plan.event),entityType:'account_game'}, {...projection.envelope,crypto_version:2,aad_version:2,nonce,ciphertext})
      : await decryptObjectBytes(amk, { userId: scope.canonicalUserId, projectId: h.project_id, entityId: gameOwner(plan.event), entityType: 'project_game' }, { ...projection.envelope,crypto_version:1,aad_version:1, nonce, ciphertext })
    if (new TextDecoder('utf-8', { fatal: true }).decode(bytes) !== canonical({ chain: plan.projection, event_id: h.event_id, server_sequence: plan.serverSequence })) fail()
    inspectPreparedGameEvent(token, scope, inventory)
    return structuredClone(plan.projection)
  } finally { nonce.fill(0); ciphertext.fill(0); bytes?.fill(0) }
}

/** Reauthenticate durable outcomes using the same source/ledger/projection checks
 * for recovery, source reimport and unlocked presentation. Digests alone never
 * authorize a row. The caller retains its full snapshot CAS/lifetime check. */
export async function authenticateReplayGames(scope: BrowserSyncScope,
  snapshot: import('./replayInventory').ReplaySnapshot, amk: AccountMasterKey, current: () => boolean) {
  if (!current()) throw new BrowserPersistenceError('closed')
  if (!snapshot.rows.some(row => row.state === 'game_proven')) return []
  const inventory = snapshot, values: BrowserProjectGame[] = [], events:GameEvent[] = [], verified = new Map<number, PreparedGameEvent>()
  for (const row of snapshot.rows) if (row.state === 'game_proven') {
    if (!current()) throw new BrowserPersistenceError('closed')
    const joined = joinGameRecords(scope, row, snapshot.games?.ledger ?? [], snapshot.games?.projections ?? [])
    if (await gameDigest(joined.ledger) !== row.game_proof.ledger_digest
      || await gameDigest(joined.projection) !== row.game_proof.projection_digest) fail()
    const token = await prepareGameEvent(scope, row, inventory, amk, current, verified)
    const chain = await authenticateGameProjection(scope, token, inventory, joined.ledger, joined.projection, amk)
    events.push(inspectPreparedGameEvent(token,scope,inventory).event)
    if (snapshot.games?.projections.some(value => (value as GameProjection).head_event_id === row.event_id)) values.push({ event:inspectPreparedGameEvent(token, scope, inventory).event, state:chain })
  }
  if (!current()) throw new BrowserPersistenceError('closed')
  return values.map(value=>({...value,history:events.filter(event=>gameOwner(event)===gameOwner(value.event))}))
}

export interface BrowserProjectGame { event: GameEvent; state: GameState }
export const commitGameGenesis = commitGameEvent
