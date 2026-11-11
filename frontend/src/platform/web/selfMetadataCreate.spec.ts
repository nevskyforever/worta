// @vitest-environment node
import { expect, it } from 'vitest'
import { encodeV3MetadataPush, type V3Descriptor } from '@/api/encryptedSyncV3'
import { createCloudProjectMetadata } from './cloudProjectMetadataCreate'
import { amk, fixture, scope } from './bootstrap.testFixture'
import { BrowserSelfMetadataCreate } from './selfMetadataCreate'

async function setup({ receipt = true, history = true } = {}) {
  const f = await fixture([])
  const { event, object } = await createCloudProjectMetadata({ ...scope, accountId: scope.canonicalUserId }, 'Private project', amk)
  const h = event.header
  const descriptor = { event_id: h.event_id, project_id: h.project_id, entity_id: h.entity_id,
    entity_type: 'project_metadata', operation: 'upsert', revision: 1, updated_at: h.updated_at, deleted_at: null } as const
  const raw = encodeV3MetadataPush(scope.deviceId, [{ event: descriptor, object }])
  const lineage = await f.projects.prepareMetadataCreate(scope, h.project_id, h.bootstrap_id)
  const registered = await f.projects.confirmRegistration(scope, lineage, { remote_state: 'initializing', remote_high_water: 0 })
  const [queued] = await f.sync.enqueue(scope, [raw])
  if (receipt) await f.sync.commitUploadAcceptance(scope, queued!.item, { protocol_version: 3,
    encrypted_sync_version: 3, current_cursor: 1, results: [{ event_id: h.event_id, server_sequence: 1, duplicate: false }] })
  const reference: V3Descriptor = { ...descriptor, updated_at: history ? new Date(h.updated_at).toISOString() : h.updated_at,
    device_id: scope.deviceId, server_sequence: 1 }
  const wire = { ...JSON.parse(raw).items[0], event: reference }
  if (history) await f.sync.commitInboundPage(scope, 0, JSON.stringify({ protocol_version: 3, encrypted_sync_version: 3,
    items: [wire], next_cursor: 1, has_more: false }))
  const owner = new BrowserSelfMetadataCreate(f.transaction, f.sync)
  const prepare = () => owner.prepare(scope, reference, f.auth, f.bindings as never, f.keys as never)
  return { ...f, wire, owner, prepare, registered }
}

it.each([{ receipt: false }, { history: false }])('requires both independent sources: %j', async options => {
  const f = await setup(options)
  await expect(f.prepare()).rejects.toMatchObject({ code: 'missing_history' })
  expect(f.db.ownerHeads).toHaveLength(0)
})

it.each(['ciphertext', 'receipt'] as const)('rejects mismatched %s', async kind => {
  const f = await setup()
  if (kind === 'ciphertext') {
    const object = f.wire.object
    object.ciphertext = (object.ciphertext.startsWith('A') ? 'B' : 'A') + object.ciphertext.slice(1)
    Object.assign(f.db.inbox[0] as object, { wire_item: JSON.stringify(f.wire) })
  } else Object.assign(f.db.receipts[0] as object, { server_sequence: 2 })
  await expect(f.prepare()).rejects.toMatchObject({ code: 'conflicting_record' })
  expect(f.db.ownerHeads).toHaveLength(0)
})

it('rejects stale lineage at commit and leaves the inbox untouched', async () => {
  const f = await setup()
  const plan = await f.prepare()
  await f.projects.confirmRegistration(scope, f.registered, { remote_state: 'initializing', remote_high_water: 1 })
  await expect(f.owner.commit(scope, plan)).rejects.toMatchObject({ code: 'stale_lineage' })
  expect(f.db.ownerHeads).toHaveLength(0)
  expect(f.db.inbox).toMatchObject([{ state: 'received' }])
})

it('rejects a head installed between proof preparation and commit', async () => {
  const f = await setup()
  const plan = await f.prepare()
  await f.owner.commit(scope, await f.prepare())
  // A head alone is never an idempotent replay of the atomic commit.
  const row = f.db.inbox[0] as Record<string, unknown>
  delete row.applied_proof
  Object.assign(row, { state: 'received', record_version: 1 })
  await expect(f.owner.commit(scope, plan)).rejects.toMatchObject({ code: 'stale_authority' })
})

it('rejects a prepared proof after account key lock', async () => {
  const f = await setup()
  const plan = await f.prepare()
  f.lock()
  await expect(f.owner.commit(scope, plan)).rejects.toMatchObject({ code: 'closed' })
  expect(f.db.ownerHeads).toHaveLength(0)
  expect(f.db.inbox).toMatchObject([{ state: 'received' }])
})

it('rejects matching wire copies that fail authentication', async () => {
  const f = await setup()
  const outbound = f.db.outbox[0] as { raw_body: string }
  const body = JSON.parse(outbound.raw_body)
  const ciphertext: string = body.items[0].object.ciphertext
  const invalid = (ciphertext.startsWith('A') ? 'B' : 'A') + ciphertext.slice(1)
  body.items[0].object.ciphertext = invalid
  outbound.raw_body = JSON.stringify(body)
  f.wire.object.ciphertext = invalid
  Object.assign(f.db.inbox[0] as object, { wire_item: JSON.stringify(f.wire) })
  await expect(f.prepare()).rejects.toThrow()
  expect(f.db.ownerHeads).toHaveLength(0)
})

it('rolls back the head and inbox together on a failed commit', async () => {
  const f = await setup()
  const plan = await f.prepare()
  f.abort()
  await expect(f.owner.commit(scope, plan)).rejects.toMatchObject({ code: 'storage_failed' })
  expect(f.db.ownerHeads).toHaveLength(0)
  expect(f.db.inbox).toMatchObject([{ state: 'received' }])
  f.abort(false)
  await expect(f.owner.commit(scope, plan)).resolves.toBe('applied')
  await expect(f.owner.commit(scope, plan)).resolves.toBe('already_applied')
})
