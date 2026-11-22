// @vitest-environment node
import { expect, it } from 'vitest'
import { stageSetup, stageRow, scope } from './stageApply.testFixture'
import { amk } from './bootstrap.testFixture'
import { inventory } from './provenPrefix.testFixture'
import golden from '@/cloud/__fixtures__/documentCodecV1.json'
import { frameDocumentEvent, type DocumentEvent } from '@/cloud/documentCodec'
import { encryptObjectBytes } from '@/crypto'
import { encodeBase64Url } from '@/api/base64url'
import { inspectPreparedDocumentGenesis, prepareDocumentGenesis, prepareDocumentEvent } from './documentPrepare'

async function fixture(stageScoped = false) {
  const f = await stageSetup(), event = structuredClone(golden.examples[0]!.event) as DocumentEvent
  const stage = stageScoped ? await stageRow(f, 2) : null
  if (stage) await f.stages.apply(scope, 2)
  const sequence = stage ? 3 : 2
  Object.assign(event.header, { account_id: scope.canonicalUserId, device_id: '44444444-4444-4444-8444-444444444444', project_id: 'p', stage_id: stage ? 's' : null,
    entity_id: 'doc', event_id: crypto.randomUUID(), bootstrap_id: f.metadataEvent.header.bootstrap_id, metadata_event_id: f.metadataEvent.header.event_id,
    stage_event_ids: stage ? [stage.event.header.event_id] : [], parents: [], revision: 1, generation: 1, operation: 'create' })
  Object.assign(event.document!, { id: 'doc', project_id: 'p', stage_id: stage ? 's' : null })
  const frame = await frameDocumentEvent(event), object = await encryptObjectBytes(amk, { userId: scope.canonicalUserId, projectId: 'p', entityId: 'doc', entityType: 'document' }, frame)
  const wire = { event: { event_id: event.header.event_id, device_id: event.header.device_id, project_id: 'p', entity_id: 'doc', entity_type: 'document', operation: 'event', revision: 1, updated_at: event.header.updated_at, deleted_at: null, server_sequence: sequence },
    object: { crypto_version: 1, aad_version: 1, nonce: encodeBase64Url(object.nonce), ciphertext: encodeBase64Url(object.ciphertext) } }
  frame.fill(0); object.nonce.fill(0); object.ciphertext.fill(0)
  return { f, event, input: { server_sequence: sequence, wire_item: JSON.stringify(wire) } }
}
it('prepares genuine encrypted codec10 genesis against authenticated Metadata without writing proofs', async () => {
  const { f, event, input } = await fixture(), before = structuredClone(f.db)
  const plan = await prepareDocumentGenesis(scope, input, inventory(f), amk, () => true)
  const data = inspectPreparedDocumentGenesis(plan, scope, inventory(f))
  expect(data.projection).toEqual(event.document); expect(data.wireItem).toBe(input.wire_item); expect(f.db).toEqual(before)
  const changed = inventory(f); changed.cursor.pull_cursor++
  expect(() => inspectPreparedDocumentGenesis(plan, scope, changed)).toThrow('stale_source')
  expect(() => inspectPreparedDocumentGenesis({} as typeof plan, scope, inventory(f))).toThrow()
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

it('rejects an encrypted codec10 update without an authenticated current parent', async () => {
  const { f, event } = await fixture()
  Object.assign(event.header, { operation: 'update', revision: 2, generation: 2, parents: [crypto.randomUUID()] })
  const frame = await frameDocumentEvent(event)
  const sealed = await encryptObjectBytes(amk, { userId: scope.canonicalUserId, projectId: 'p', entityId: 'doc', entityType: 'document' }, frame)
  try {
    const wire = { event: { event_id: event.header.event_id, device_id: event.header.device_id, project_id: 'p', entity_id: 'doc', entity_type: 'document', operation: 'event', revision: 2, updated_at: event.header.updated_at, deleted_at: null, server_sequence: 3 },
      object: { crypto_version: 1, aad_version: 1, nonce: encodeBase64Url(sealed.nonce), ciphertext: encodeBase64Url(sealed.ciphertext) } }
    const before = structuredClone(f.db)
    await expect(prepareDocumentEvent(scope, { server_sequence: 3, wire_item: JSON.stringify(wire) }, inventory(f), amk, () => true)).rejects.toThrow()
    expect(f.db).toEqual(before)
  } finally { frame.fill(0); sealed.nonce.fill(0); sealed.ciphertext.fill(0) }
})

it('requires genuine Stage owner certification for a Stage-scoped Document', async () => {
  const { f, event, input } = await fixture(true), v = inventory(f)
  const token = await prepareDocumentEvent(scope, input, v, amk, () => true)
  expect(inspectPreparedDocumentGenesis(token, scope, v).projection).toEqual(event.document)
  const missing = inventory(f); missing.owners = missing.owners.filter(value => (value as { owner_kind: string }).owner_kind !== 'stage')
  await expect(prepareDocumentEvent(scope, input, missing, amk, () => true)).rejects.toThrow()
  const corrupted = inventory(f)
  const head = corrupted.owners.find(value => (value as { owner_kind: string }).owner_kind === 'stage') as { history: { wire_digest: string }[] }
  head.history[0]!.wire_digest = '0'.repeat(64)
  await expect(prepareDocumentEvent(scope, input, corrupted, amk, () => true)).rejects.toThrow()
})

it('reopens an encrypted Web revision only with its exact receipt and server echo', async () => {
  const { f, event, input } = await fixture(true)
  const base = inventory(f)
  const row = (sequence: number, id: string, wire: string) => ({
    account_id: scope.accountId, device_id: scope.deviceId, event_id: id,
    server_sequence: sequence, protocol_version: 3, record_version: 1,
    state: 'received', wire_item: wire,
  })
  base.rows.push({
    ...row(input.server_sequence, event.header.event_id, input.wire_item), record_version: 11, state: 'document_proven',
    document_proof: { record_version: 1, owner_kind: 'document', outcome: 'applied', ledger_digest: '0'.repeat(64), projection_digest: '0'.repeat(64) },
  })
  const { sealDocumentForOutbox } = await import('./documentOutbound')
  const { BrowserSyncDurability } = await import('./syncDurability')
  const update = structuredClone(event)
  Object.assign(update.header, { device_id: scope.deviceId, event_id: crypto.randomUUID(),
    parents: [event.header.event_id], operation: 'update', revision: 2, generation: 2 })
  update.document!.content_json = { type: 'doc', content: [{ type: 'paragraph', content: [{ type: 'text', text: 'Web edit', marks: [{ type: 'bold' }] }] }] }
  const body = await sealDocumentForOutbox(scope, update, amk)
  const [queued] = await new BrowserSyncDurability(f.transaction).enqueue(scope, [body])
  const sequence = input.server_sequence + 1
  const wire = JSON.parse(body).items[0]
  Object.assign(wire.event, { device_id: scope.deviceId, server_sequence: sequence })
  const echo = row(sequence, update.header.event_id, JSON.stringify(wire))
  base.rows.push(echo)
  base.out = [queued!.item]
  await expect(prepareDocumentEvent(scope, echo, base, amk, () => true)).rejects.toThrow()
  base.out = [{ ...queued!.item, state: 'accepted' }]
  base.receipts = [{ record_version: 1, account_id: scope.accountId, device_id: scope.deviceId,
    event_id: update.header.event_id, protocol_version: 3, server_sequence: sequence, duplicate: false }]
  const token = await prepareDocumentEvent(scope, echo, base, amk, () => true)
  expect(inspectPreparedDocumentGenesis(token, scope, base).projection).toEqual(update.document)
})
