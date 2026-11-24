import { encodeAccountPush } from '@/api/encryptedSyncV3'
import type { AccountMasterKey } from '@/crypto'
import { encryptAccountObject } from '@/crypto/accountObjectCrypto'
import { frameGameEvent, type AccountGameEvent } from '@/cloud/gameCodec'
import { BrowserPersistenceError } from './persistence'
import { validateBrowserRetainedHistory, type BrowserSyncScope } from './syncDurability'
import type { ReplaySnapshot } from './replayInventory'

export async function sealAccountGame(scope: BrowserSyncScope, event: AccountGameEvent, amk: AccountMasterKey) {
  const h = event.header
  if (h.account_id !== scope.canonicalUserId || h.device_id !== scope.deviceId
    || h.scope !== 'account' || event.action.kind !== 'inventory') throw new BrowserPersistenceError('unsupported_local_source')
  const frame = frameGameEvent(event)
  try {
    const object = await encryptAccountObject(amk, {
      userId: h.account_id, scope: 'account', entityId: h.entity_id, entityType: 'account_game',
    }, frame)
    try {
      return encodeAccountPush(scope.deviceId, [{ event: {
        event_id: h.event_id, canonical_user_id: h.account_id, scope: 'account', entity_id: h.entity_id,
        entity_type: 'account_game', operation: 'upsert', revision: h.revision, updated_at: h.updated_at, deleted_at: null,
      }, object }])
    } finally { object.nonce.fill(0); object.ciphertext.fill(0) }
  } finally { frame.fill(0) }
}

export function assertAccountGameEcho(scope: BrowserSyncScope, snapshot: ReplaySnapshot, input: { wire_item: string }) {
  const descriptor = JSON.parse(input.wire_item).event
  const row = snapshot.rows.find(value => value.event_id === descriptor.event_id)
  const out = snapshot.author?.out.find(value => (value as {event_id: string}).event_id === descriptor.event_id)
  const receipt = snapshot.author?.receipts.find(value => (value as {event_id: string}).event_id === descriptor.event_id)
  if (descriptor.entity_type !== 'account_game' || descriptor.device_id !== scope.deviceId || row?.wire_item !== input.wire_item
    || (receipt as {record_version?: number} | undefined)?.record_version !== 1) {
    throw new BrowserPersistenceError('conflicting_record')
  }
  const evidence = validateBrowserRetainedHistory(scope, descriptor, out, receipt, row)
  if (!evidence.outbox || !evidence.receipt || !evidence.inbox) throw new BrowserPersistenceError('conflicting_record')
}
