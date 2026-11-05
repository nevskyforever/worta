// @vitest-environment node
import { expect, it } from 'vitest'
import { stageSetup, scope } from './stageApply.testFixture'
import { amk } from './bootstrap.testFixture'
import { inventory } from './provenPrefix.testFixture'
import golden from '@/cloud/__fixtures__/documentCodecV1.json'
import { frameDocumentEvent, type DocumentEvent } from '@/cloud/documentCodec'
import { encryptObjectBytes } from '@/crypto'
import { encodeBase64Url } from '@/api/base64url'
import { prepareDocumentGenesis } from './documentPrepare'

async function fixture() {
  const f = await stageSetup(), event = structuredClone(golden.examples[0]!.event) as DocumentEvent
  Object.assign(event.header, { account_id: scope.canonicalUserId, device_id: '44444444-4444-4444-8444-444444444444', project_id: 'p', stage_id: null,
    entity_id: 'doc', event_id: crypto.randomUUID(), bootstrap_id: f.metadataEvent.header.bootstrap_id, metadata_event_id: f.metadataEvent.header.event_id,
    stage_event_ids: [], parents: [], revision: 1, generation: 1, operation: 'create' })
  Object.assign(event.document!, { id: 'doc', project_id: 'p', stage_id: null })
  const frame = await frameDocumentEvent(event), object = await encryptObjectBytes(amk, { userId: scope.canonicalUserId, projectId: 'p', entityId: 'doc', entityType: 'document' }, frame)
  const wire = { event: { event_id: event.header.event_id, device_id: event.header.device_id, project_id: 'p', entity_id: 'doc', entity_type: 'document', operation: 'event', revision: 1, updated_at: event.header.updated_at, deleted_at: null, server_sequence: 2 },
    object: { crypto_version: 1, aad_version: 1, nonce: encodeBase64Url(object.nonce), ciphertext: encodeBase64Url(object.ciphertext) } }
  frame.fill(0); object.nonce.fill(0); object.ciphertext.fill(0)
  return { f, event, input: { server_sequence: 2, wire_item: JSON.stringify(wire) } }
}
it('prepares genuine encrypted codec10 genesis against authenticated Metadata without writing proofs', async () => {
  const { f, event, input } = await fixture(), before = structuredClone(f.db)
  const plan = await prepareDocumentGenesis(scope, input, inventory(f), amk, () => true)
  expect(plan.projection).toEqual(event.document); expect(plan.wireItem).toBe(input.wire_item); expect(f.db).toEqual(before)
})
it('rejects damaged ciphertext, wrong sequence, missing Metadata and revoked sessions', async () => {
  const { f, input } = await fixture(), wire = JSON.parse(input.wire_item), original = wire.object.ciphertext
  wire.object.ciphertext = (original[0] === 'A' ? 'B' : 'A') + original.slice(1)
  await expect(prepareDocumentGenesis(scope, { ...input, wire_item: JSON.stringify(wire) }, inventory(f), amk, () => true)).rejects.toThrow()
  await expect(prepareDocumentGenesis(scope, { ...input, server_sequence: 3 }, inventory(f), amk, () => true)).rejects.toThrow()
  const missing = inventory(f); missing.owners = []
  await expect(prepareDocumentGenesis(scope, input, missing, amk, () => true)).rejects.toThrow()
  await expect(prepareDocumentGenesis(scope, input, inventory(f), amk, () => false)).rejects.toThrow()
})
