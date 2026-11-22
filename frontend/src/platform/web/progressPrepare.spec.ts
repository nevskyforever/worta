// @vitest-environment node
import { expect, it } from 'vitest'
import { stageSetup, stageRow, scope } from './stageApply.testFixture'
import { amk } from './bootstrap.testFixture'
import { inventory } from './provenPrefix.testFixture'
import golden from '@/cloud/__fixtures__/progressCodecV1.json'
import { frameProgressEvent, type ProgressEvent } from '@/cloud/progressCodec'
import { encryptObjectBytes } from '@/crypto'
import { encodeBase64Url } from '@/api/base64url'
import { inspectPreparedProgressEvent, prepareProgressGenesis } from './progressPrepare'

async function fixture(stageScoped = false) {
  const f = await stageSetup(), event = structuredClone(golden.examples[0]!.event) as ProgressEvent
  const stage = stageScoped ? await stageRow(f, 2) : null
  if (stage) await f.stages.apply(scope, 2)
  const sequence = stage ? 3 : 2
  Object.assign(event.header, { account_id: scope.canonicalUserId, device_id: '44444444-4444-4444-8444-444444444444', project_id: 'p', stage_id: stage ? 's' : null,
    entity_id: stage ? 'stage:s' : 'project', event_id: crypto.randomUUID(), bootstrap_id: f.metadataEvent.header.bootstrap_id, metadata_event_id: f.metadataEvent.header.event_id,
    stage_event_ids: stage ? [stage.event.header.event_id] : [], parents: [], revision: 1, generation: 1, operation: 'genesis' })
  const frame = await frameProgressEvent(event), object = await encryptObjectBytes(amk, { userId: scope.canonicalUserId, projectId: 'p', entityId: event.header.entity_id, entityType: 'progress' }, frame)
  const wire = { event: { event_id: event.header.event_id, device_id: event.header.device_id, project_id: 'p', entity_id: event.header.entity_id, entity_type: 'progress', operation: 'event', revision: 1, updated_at: event.header.updated_at, deleted_at: null, server_sequence: sequence },
    object: { crypto_version: 1, aad_version: 1, nonce: encodeBase64Url(object.nonce), ciphertext: encodeBase64Url(object.ciphertext) } }
  frame.fill(0); object.nonce.fill(0); object.ciphertext.fill(0)
  return { f, event, input: { server_sequence: sequence, wire_item: JSON.stringify(wire) } }
}
it('prepares genuine encrypted codec11 genesis against authenticated Metadata without writing proofs', async () => {
  const { f, event, input } = await fixture(), before = structuredClone(f.db)
  const plan = await prepareProgressGenesis(scope, input, inventory(f), amk, () => true)
  const data = inspectPreparedProgressEvent(plan, scope, inventory(f))
  expect(data.projection).toEqual({ base_total: event.base_total, entries: event.entries }); expect(data.wireItem).toBe(input.wire_item); expect(f.db).toEqual(before)
  const changed = inventory(f); changed.cursor.pull_cursor++
  expect(() => inspectPreparedProgressEvent(plan, scope, changed)).toThrow('stale_source')
  expect(() => inspectPreparedProgressEvent({} as typeof plan, scope, inventory(f))).toThrow()
})
it('rejects damaged ciphertext, wrong sequence, missing Metadata and revoked sessions', async () => {
  const { f, input } = await fixture(), wire = JSON.parse(input.wire_item), original = wire.object.ciphertext
  wire.object.ciphertext = (original[0] === 'A' ? 'B' : 'A') + original.slice(1)
  await expect(prepareProgressGenesis(scope, { ...input, wire_item: JSON.stringify(wire) }, inventory(f), amk, () => true)).rejects.toThrow()
  await expect(prepareProgressGenesis(scope, { ...input, server_sequence: 3 }, inventory(f), amk, () => true)).rejects.toThrow()
  const missing = inventory(f); missing.owners = []
  await expect(prepareProgressGenesis(scope, input, missing, amk, () => true)).rejects.toThrow()
  await expect(prepareProgressGenesis(scope, input, inventory(f), amk, () => false)).rejects.toThrow()
})
it.each(['tombstone', 'correct'] as const)('authenticates a Desktop %s after its encrypted parent', async operation => {
  const { f, event, input } = await fixture()
  const snapshot = inventory(f)
  snapshot.rows.push({ state: 'progress_proven', event_id: event.header.event_id, ...input } as never)
  const deletion = structuredClone(event)
  Object.assign(deletion.header, { operation, event_id: crypto.randomUUID(), parents: [event.header.event_id], revision: 2, generation: 2 })
  const original = event.entries[0]!
  const corrected = { ...original, entry_id: 'corrected', new_total: '130.000000', delta: '30.000000' }
  const entries = operation === 'correct' ? [corrected] : []
  Object.assign(deletion, { migration: null, base_total: null, selected_event_id: event.header.event_id,
    target_entry_id: original.entry_id, entries, rebased_from: operation === 'correct' ? [original.entry_id] : [] })
  const frame = await frameProgressEvent(deletion)
  const object = await encryptObjectBytes(amk, { userId: scope.canonicalUserId, projectId: 'p', entityId: 'project', entityType: 'progress' }, frame)
  const wire = JSON.parse(input.wire_item)
  Object.assign(wire.event, { event_id: deletion.header.event_id, revision: 2, server_sequence: 3 })
  wire.object = { crypto_version: 1, aad_version: 1, nonce: encodeBase64Url(object.nonce), ciphertext: encodeBase64Url(object.ciphertext) }
  const token = await prepareProgressGenesis(scope, { server_sequence: 3, wire_item: JSON.stringify(wire) }, snapshot, amk, () => true)
  expect(inspectPreparedProgressEvent(token, scope, snapshot).projection).toEqual({ base_total: event.base_total, entries })
})

it('authenticates both Desktop branches and a rebase without choosing a branch by delivery order', async () => {
  const { f, event: root, input } = await fixture()
  const snapshot = inventory(f)
  snapshot.rows.push({ ...input, state: 'progress_proven', event_id: root.header.event_id } as never)
  async function add(event: ProgressEvent, sequence: number) {
    const frame = await frameProgressEvent(event)
    const object = await encryptObjectBytes(amk, { userId: scope.canonicalUserId, projectId: 'p', entityId: 'project', entityType: 'progress' }, frame)
    const wire = JSON.parse(input.wire_item)
    Object.assign(wire.event, { event_id: event.header.event_id, revision: event.header.revision, server_sequence: sequence })
    wire.object = { crypto_version: 1, aad_version: 1, nonce: encodeBase64Url(object.nonce), ciphertext: encodeBase64Url(object.ciphertext) }
    const row = { state: 'progress_proven', event_id: event.header.event_id, server_sequence: sequence, wire_item: JSON.stringify(wire) }
    const token = await prepareProgressGenesis(scope, row, snapshot, amk, () => true)
    const result = inspectPreparedProgressEvent(token, scope, snapshot)
    snapshot.rows.push(row as never)
    return result
  }
  function branch(delta: number, entryId: string): ProgressEvent {
    return { ...root, migration: null, base_total: null, selected_event_id: root.header.event_id,
      header: { ...root.header, operation: 'append', revision: 2, generation: 2,
        parents: [root.header.event_id], event_id: crypto.randomUUID() },
      entries: [{ ...root.entries[0]!, entry_id: entryId, new_total: (120 + delta).toFixed(6), delta: delta.toFixed(6) }] }
  }
  const first = branch(10, 'first'), second = branch(20, 'second')
  expect((await add(first, 3)).tips).toEqual([first.header.event_id])
  expect((await add(second, 4)).tips).toEqual([first.header.event_id, second.header.event_id].sort())
  const resolution: ProgressEvent = {
    ...first, selected_event_id: first.header.event_id, rebased_from: ['second'],
    header: { ...first.header, operation: 'rebase', revision: 3, generation: 3,
      parents: [first.header.event_id, second.header.event_id].sort(), event_id: crypto.randomUUID() },
    entries: [{ ...second.entries[0]!, entry_id: 'transferred', new_total: '150.000000' }],
  }
  await expect(add({ ...resolution, entries: [{ ...resolution.entries[0]!, delta: '19.000000' }] }, 5))
    .rejects.toThrow('progress_rebase_invalid')
  const resolved = await add(resolution, 5)
  expect(resolved.tips).toEqual([resolution.header.event_id])
  expect(resolved.projection.entries.map(entry => entry.new_total)).toEqual(['120.000000', '130.000000', '150.000000'])
})

it('requires a matching accepted outbox and receipt for a sealed Web Progress echo', async () => {
  const { f, event: parent, input } = await fixture()
  const { prepareProgressAppend } = await import('@/cloud/progressAuthor')
  const { sealProgressForOutbox } = await import('./progressOutbound')
  const { transitionProgress } = await import('@/cloud/progressTransition')
  const event = prepareProgressAppend({
    parent, chain: transitionProgress(parent), deviceId: scope.deviceId, now: parent.header.updated_at,
    eventId: crypto.randomUUID(), metadataEventId: parent.header.metadata_event_id, stageEventIds: [],
  }, { entryId: crypto.randomUUID(), totalSymbols: '135.000000', unit: 'symbols', writingDay: '2026-10-03' })
  const raw = await sealProgressForOutbox(scope, event, amk)
  const item = JSON.parse(raw).items[0]
  const row = { record_version: 1 as const, state: 'received' as const, event_id: event.header.event_id,
    server_sequence: 3, protocol_version: 3 as const, account_id: scope.accountId, device_id: scope.deviceId,
    wire_item: JSON.stringify({ ...item, event: { ...item.event, device_id: scope.deviceId, server_sequence: 3 } }) }
  const base = inventory(f)
  base.rows.push({ ...input, state: 'progress_proven', event_id: parent.header.event_id } as never, row as never)
  await expect(prepareProgressGenesis(scope, row, base, amk, () => true)).rejects.toThrow()
  base.out.push({ record_version: 1, state: 'accepted', account_id: scope.accountId, device_id: scope.deviceId,
    event_id: row.event_id, raw_body: raw, local_ordinal: 1, protocol_version: 3 })
  base.receipts.push({ record_version: 1, account_id: scope.accountId, device_id: scope.deviceId,
    event_id: row.event_id, server_sequence: 4, duplicate: false, protocol_version: 3 })
  await expect(prepareProgressGenesis(scope, row, base, amk, () => true)).rejects.toThrow('conflicting_record')
  ;(base.receipts[0] as {server_sequence: number}).server_sequence = 3
  const plan = await prepareProgressGenesis(scope, row, base, amk, () => true)
  expect(inspectPreparedProgressEvent(plan, scope, base).projection.entries.at(-1)?.new_total).toBe('135.000000')
})
