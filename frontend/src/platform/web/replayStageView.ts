import { decodeBase64Url } from '@/api/base64url'
import type { AccountMasterKey } from '@/crypto'
import type { StructuralEvent } from '@/cloud/stageCodec'
import { openBrowserCanonicalObject } from './canonicalReaders'
import { prepareMetadataPrefix } from './metadataPrefixProof'
import { prepareStagePrefix, stageSequences } from './stageApply'
import { prepareStageOrderPrefix, stageOrderSequences } from './stageOrderApply'
import { validateStageHead } from './stageRecords'
import { validateStageOrderHead } from './stageOrderRecords'
import { BrowserReplayError } from './replayRecords'
import type { ReplaySnapshot } from './replayInventory'
import type { BrowserSyncScope } from './syncDurability'

export interface ReplayStageView { id: string; name: string }

/** Read names and order only from the exact owner-certified encrypted snapshot. */
export async function readReplayStages(scope: BrowserSyncScope, snapshot: ReplaySnapshot, projectId: string,
  amk: AccountMasterKey, current: () => boolean): Promise<ReplayStageView[]> {
  if (!snapshot.manifest || snapshot.manifest.applied_cursor !== snapshot.manifest.receive_cursor)
    throw new BrowserReplayError('unsupported_history')
  const inventory = { rows: snapshot.rows.filter(row => ['project_metadata', 'stage', 'stage_order'].includes(JSON.parse(row.wire_item).event.entity_type)),
    bindings: snapshot.bindings, owners: snapshot.owners, out: [], receipts: [], history: [],
    cursor: { pull_cursor: snapshot.manifest.receive_cursor, ack_cursor: 0, proven_prefix: 0, next_local_ordinal: 1 } }
  const meta = await prepareMetadataPrefix(scope, inventory, amk, current, true)
  const stages = stageSequences(await prepareStagePrefix(scope, inventory, amk, current, meta, true), scope, inventory)
  const orders = stageOrderSequences(await prepareStageOrderPrefix(scope, inventory, amk, current, meta, true), scope, inventory)
  const heads = snapshot.owners.filter(head => head.owner_kind === 'stage' && head.project_id === projectId).map(head => validateStageHead(head, scope))
  const orderHeads = snapshot.owners.filter(head => head.owner_kind === 'stage_order' && head.project_id === projectId).map(head => validateStageOrderHead(head, scope))
  if (!heads.length && !orderHeads.length) return []
  if (orderHeads.length !== 1 || heads.some(head => head.history.some(proof => !stages.has(proof.server_sequence)))
      || orderHeads[0]!.history.some(proof => !orders.has(proof.server_sequence))) throw new BrowserReplayError('unsupported_history')
  async function open(eventId: string): Promise<StructuralEvent> {
    const row = snapshot.rows.find(item => item.event_id === eventId)
    if (!row) throw new BrowserReplayError('unsupported_history')
    const wire = JSON.parse(row.wire_item), nonce = decodeBase64Url(wire.object.nonce), ciphertext = decodeBase64Url(wire.object.ciphertext)
    try {
      return await openBrowserCanonicalObject(amk, { accountId: scope.canonicalUserId, projectId,
        entityId: wire.event.entity_id, entityType: wire.event.entity_type, eventId }, { ...wire.object, nonce, ciphertext }) as StructuralEvent
    } finally { nonce.fill(0); ciphertext.fill(0) }
  }
  const order = await open(orderHeads[0]!.tip_event_ids[0]!)
  if (!order.stage_ids || order.stage_ids.length !== heads.length) throw new BrowserReplayError('unsupported_history')
  const result: ReplayStageView[] = []
  for (const id of order.stage_ids) {
    const head = heads.find(value => value.stage_id === id)
    if (!head || head.tip_event_ids.length !== 1) throw new BrowserReplayError('unsupported_history')
    const event = await open(head.tip_event_ids[0]!)
    if (!event.stage) throw new BrowserReplayError('unsupported_history')
    result.push({ id, name: event.stage.name })
  }
  if (!current()) throw new BrowserReplayError('session_closed')
  return result
}
