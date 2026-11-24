import { expect, it } from 'vitest'
import golden from '@/cloud/__fixtures__/gameCodecV1.json'
import type { AccountGameEvent } from '@/cloud/gameCodec'
import { prepareGameShopAction } from '@/cloud/gameShop'
import { transitionGame } from '@/cloud/gameProjectTransition'
import { generateAccountMasterKey } from '@/crypto/keys'
import { assertAccountGameEcho, sealAccountGame } from './gameOutbound'
import { validateBrowserSealedBody } from './syncDurability'
import type { ReplaySnapshot } from './replayInventory'

it('seals a Desktop-compatible shop action and requires its exact accepted server echo', async () => {
  const parent = structuredClone(golden.examples.find(value => value.name === 'account-genesis')!.event) as AccountGameEvent
  if (parent.action.kind !== 'genesis') throw new Error('fixture')
  const scope = { canonicalUserId: parent.header.account_id, accountId: crypto.randomUUID(), deviceId: crypto.randomUUID() }
  const event = prepareGameShopAction(parent.action.base, parent, scope.deviceId, {
    operation: 'buy', category: 'Зелья', item_id: 'Микро зелье здоровья', count: 1,
  }, '2026-10-05T00:00:00Z', crypto.randomUUID())
  expect(transitionGame(event, parent.action.base)).toMatchObject({ coins: '221.000000' })
  const raw = await sealAccountGame(scope, event, await generateAccountMasterKey())
  expect(validateBrowserSealedBody(raw, scope).eventId).toBe(event.header.event_id)
  const item = JSON.parse(raw).items[0]
  const wire = { ...item, event: { ...item.event, updated_at: '2026-10-05T00:00:00Z', device_id: scope.deviceId, server_sequence: 2 } }
  const record = { record_version: 1 as const, account_id: scope.accountId, device_id: scope.deviceId, event_id: event.header.event_id,
    state: 'received' as const, protocol_version: 3 as const, server_sequence: 2, wire_item: JSON.stringify(wire) }
  const outbound = { account_id: scope.accountId, device_id: scope.deviceId, event_id: event.header.event_id,
    record_version: 1, protocol_version: 3, state: 'accepted', local_ordinal: 1, raw_body: raw }
  const receipt = { account_id: scope.accountId, device_id: scope.deviceId, event_id: event.header.event_id,
    record_version: 1, protocol_version: 3, server_sequence: 2, duplicate: false }
  const snapshot: ReplaySnapshot = { manifest: null, rows: [record], bindings: [], owners: [],
    author: { out: [outbound], receipts: [receipt], intents: [] } }
  expect(() => assertAccountGameEcho(scope, snapshot, record)).not.toThrow()
  expect(() => assertAccountGameEcho(scope, { ...snapshot, author: undefined }, record)).toThrow('conflicting_record')
  expect(() => assertAccountGameEcho(scope, { ...snapshot, author: { ...snapshot.author!, receipts: [{ ...receipt, server_sequence: 3 }] } }, record)).toThrow('conflicting_record')
})
