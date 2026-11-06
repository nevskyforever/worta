// @vitest-environment node
import { expect, it } from 'vitest'
import { stageSetup, stageRow, scope } from './stageApply.testFixture'
import { amk } from './bootstrap.testFixture'
import { inventory } from './provenPrefix.testFixture'
import golden from '@/cloud/__fixtures__/mapCodecV1.json'
import { frameMapEvent, type MapEvent } from '@/cloud/mapCodec'
import { encryptObjectBytes } from '@/crypto'
import { encodeBase64Url } from '@/api/base64url'
import { inspectPreparedMapEvent, prepareMapGenesis, prepareMapEvent } from './mapPrepare'

async function fixture(stageScoped = false) {
  const f = await stageSetup(), event = structuredClone(golden.examples[0]!.event) as MapEvent
  const stage = stageScoped ? await stageRow(f, 2) : null
  if (stage) await f.stages.apply(scope, 2)
  const sequence = stage ? 3 : 2
  Object.assign(event.header, { account_id: scope.canonicalUserId, device_id: '44444444-4444-4444-8444-444444444444', project_id: 'p', stage_id: stage ? 's' : null,
    entity_id: stage ? 'stage-map-' + 'a'.repeat(64) : 'project-map', event_id: crypto.randomUUID(), bootstrap_id: f.metadataEvent.header.bootstrap_id, metadata_event_id: f.metadataEvent.header.event_id,
    stage_event_ids: stage ? [stage.event.header.event_id] : [], parents: [], revision: 1, generation: 1, operation: 'create' })
  const frame = await frameMapEvent(event), object = await encryptObjectBytes(amk, { userId: scope.canonicalUserId, projectId: 'p', entityId: event.header.entity_id, entityType: 'map' }, frame)
  const wire = { event: { event_id: event.header.event_id, device_id: event.header.device_id, project_id: 'p', entity_id: event.header.entity_id, entity_type: 'map', operation: 'event', revision: 1, updated_at: event.header.updated_at, deleted_at: null, server_sequence: sequence },
    object: { crypto_version: 1, aad_version: 1, nonce: encodeBase64Url(object.nonce), ciphertext: encodeBase64Url(object.ciphertext) } }
  frame.fill(0); object.nonce.fill(0); object.ciphertext.fill(0)
  return { f, event, input: { server_sequence: sequence, wire_item: JSON.stringify(wire) } }
}
it('prepares genuine encrypted codec9 genesis against authenticated Metadata without writing proofs', async () => {
  const { f, event, input } = await fixture(), before = structuredClone(f.db)
  const plan = await prepareMapGenesis(scope, input, inventory(f), amk, () => true)
  const data = inspectPreparedMapEvent(plan, scope, inventory(f))
  expect(data.projection).toEqual(event.map); expect(data.wireItem).toBe(input.wire_item); expect(f.db).toEqual(before)
  const changed = inventory(f); changed.cursor.pull_cursor++
  expect(() => inspectPreparedMapEvent(plan, scope, changed)).toThrow('stale_source')
  expect(() => inspectPreparedMapEvent({} as typeof plan, scope, inventory(f))).toThrow()
})
it('rejects damaged ciphertext, wrong sequence, missing Metadata and revoked sessions', async () => {
  const { f, input } = await fixture(), wire = JSON.parse(input.wire_item), original = wire.object.ciphertext
  wire.object.ciphertext = (original[0] === 'A' ? 'B' : 'A') + original.slice(1)
  await expect(prepareMapGenesis(scope, { ...input, wire_item: JSON.stringify(wire) }, inventory(f), amk, () => true)).rejects.toThrow()
  await expect(prepareMapGenesis(scope, { ...input, server_sequence: 3 }, inventory(f), amk, () => true)).rejects.toThrow()
  const missing = inventory(f); missing.owners = []
  await expect(prepareMapGenesis(scope, input, missing, amk, () => true)).rejects.toThrow()
  await expect(prepareMapGenesis(scope, input, inventory(f), amk, () => false)).rejects.toThrow()
})

it('rejects an encrypted codec9 update without an authenticated current parent', async () => {
  const { f, event } = await fixture()
  Object.assign(event.header, { operation: 'update', revision: 2, generation: 2, parents: [crypto.randomUUID()] })
  const frame = await frameMapEvent(event)
  const sealed = await encryptObjectBytes(amk, { userId: scope.canonicalUserId, projectId: 'p', entityId: event.header.entity_id, entityType: 'map' }, frame)
  try {
    const wire = { event: { event_id: event.header.event_id, device_id: event.header.device_id, project_id: 'p', entity_id: 'project-map', entity_type: 'map', operation: 'event', revision: 2, updated_at: event.header.updated_at, deleted_at: null, server_sequence: 3 },
      object: { crypto_version: 1, aad_version: 1, nonce: encodeBase64Url(sealed.nonce), ciphertext: encodeBase64Url(sealed.ciphertext) } }
    const before = structuredClone(f.db)
    await expect(prepareMapEvent(scope, { server_sequence: 3, wire_item: JSON.stringify(wire) }, inventory(f), amk, () => true)).rejects.toThrow()
    expect(f.db).toEqual(before)
  } finally { frame.fill(0); sealed.nonce.fill(0); sealed.ciphertext.fill(0) }
})
