// @vitest-environment node
import { expect, it } from 'vitest'
import { stageSetup, stageRow, scope } from './stageApply.testFixture'
import { orderSetup, orderRow } from './stageOrder.testFixture'
import { replayCoverage } from './replayInventory'
import { replayAnchor, validateReplayManifest, type ReplayManifest } from './replayRecords'
import type { BrowserInboxRecord } from './syncDurability'

const manifest = (): ReplayManifest => ({ record_version: 5, contract_version: 5, metadata_cursor: 1, applied_cursor: 2, source_seeds: [],
  account_id: scope.accountId, device_id: scope.deviceId, canonical_user_id: scope.canonicalUserId, generation: crypto.randomUUID(),
  anchor: replayAnchor({ current_cursor: 3, projects: [] }, { supported_transport_version: 2, writer_transport_version: 3, cutover_epoch: 1 }),
  receive_cursor: 3, ciphertext_bytes: 0, phase: 'received', revision: 5 })
it('v5 coverage validates separately from Metadata, retaining v2/v3 only for migration', () => {
  const state = manifest(); expect(validateReplayManifest(state, scope, 5)).toEqual(state)
  const current = { ...state, record_version: 6, contract_version: 6 }
  expect(validateReplayManifest(current, scope)).toEqual(current)
  const old = { ...state, record_version: 2, contract_version: 2 }; delete old.applied_cursor; delete old.source_seeds
  expect(validateReplayManifest(old, scope, 2)).toEqual(old)
  expect(validateReplayManifest({ ...old, record_version: 3, contract_version: 3, applied_cursor: 2 }, scope, 3)).toMatchObject({ applied_cursor: 2 })
  expect(() => validateReplayManifest(old, scope)).toThrow()
})
it.each([{ applied_cursor: -1 }, { applied_cursor: 4 }, { metadata_cursor: 3 }, { record_version: 3 }, { ack: 1 }])('rejects invalid coverage/authority %j', change => {
  expect(() => validateReplayManifest({ ...manifest(), ...change }, scope, 5)).toThrow()
  expect(() => validateReplayManifest({ ...manifest(), record_version: 6, contract_version: 6, ...change }, scope)).toThrow()
})
it.each(['stage_proven', 'stage_order_proven', 'applied'] as const)('a later %s commit cannot turn Structural coverage into Metadata-prefix authority', state => {
  // Only journal arithmetic here: actual proof admission belongs to the native transaction/owners.
  expect(replayCoverage(manifest(), { state, server_sequence: 3 } as BrowserInboxRecord)).toMatchObject({ applied_cursor: 3, metadata_cursor: 1, revision: 6 })
})
it.each([false, true])('genuine Stage raw API timestamp, drift=%s', async drift => {
  const f = await stageSetup(), entry = await stageRow(f, 2), wire = JSON.parse(entry.row.wire_item)
  wire.event.updated_at = drift ? '2026-10-13T00:00:00.000001Z' : '2026-10-13T00:00:00Z'; entry.row.wire_item = JSON.stringify(wire)
  if (drift) { const old = structuredClone(f.db); await expect(f.stages.apply(scope, 2)).rejects.toThrow(); expect(f.db).toEqual(old) }
  else { expect(await f.stages.apply(scope, 2)).toBe('applied'); expect((await f.stages.read(scope, 'p', 's'))?.name).toContain('PRIVATE_STAGE') }
})
it('genuine StageOrder raw API timestamp preserves frozen proof/wire on replay', async () => {
  const f = await orderSetup(2), entry = await orderRow(f, 4), wire = JSON.parse(entry.row.wire_item)
  wire.event.updated_at = '2026-10-13T00:00:00Z'; entry.row.wire_item = JSON.stringify(wire)
  expect(await f.orders.apply(scope, 4)).toBe('applied')
  const saved = structuredClone(f.db); expect(await f.orders.apply(scope, 4)).toBe('already_applied'); expect(f.db).toEqual(saved)
})
