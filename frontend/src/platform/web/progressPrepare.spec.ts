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
