// @vitest-environment node
import { afterEach, describe, expect, it, vi } from 'vitest'
import { cloudProjectsApi } from '@/api/cloudProjects'
import type { BrowserProjectBinding } from './projectBindings'
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
  const lineage = await f.projects.prepareMetadataCreate(scope, h.project_id, h.bootstrap_id, h.event_id)
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

describe('Metadata Create bootstrap', () => {
  afterEach(() => vi.restoreAllMocks())

  async function applied() {
    const f = await setup()
    await f.owner.commit(scope, await f.prepare())
    return f
  }

  const activeResponse = (binding: BrowserProjectBinding) => ({
    project: {
      project_id: binding.project_id, bootstrap_id: binding.bootstrap_id, origin_device_id: binding.device_id,
      state: 'active' as const, initial_event_count: 1, initial_max_server_sequence: 1,
    },
    current_cursor: 1,
  })

  it('captures exactly one proven event and completes using the existing API', async () => {
    const f = await applied()
    const captured = await f.completion.captureMetadataCreate(scope, f.registered)
    expect(captured).toMatchObject({ phase: 'captured', initial_event_count: 1, initial_local_ordinal_hi: 1 })
    expect(await f.completion.captureMetadataCreate(scope, f.registered)).toEqual(captured)
    const completing = await f.completion.markCompleting(scope, captured)
    expect(completing).toMatchObject({ phase: 'completing', initial_max_server_sequence: 1 })
    expect(await f.completion.markCompleting(scope, captured)).toEqual(completing)
    const complete = vi.spyOn(cloudProjectsApi, 'completeBootstrap').mockResolvedValue(activeResponse(completing))
    const before = structuredClone(f.db)
    const active = await f.completion.completeRemote(scope, completing)
    expect(active).toMatchObject({ phase: 'completing', remote_state: 'active' })
    expect(complete).toHaveBeenCalledExactlyOnceWith('fixture-token', completing.project_id, {
      bootstrap_id: completing.bootstrap_id, device_id: scope.deviceId, initial_event_count: 1, initial_max_server_sequence: 1,
    }, expect.any(AbortSignal))
    expect({ ...f.db, projectBindings: before.projectBindings }).toEqual(before)
    expect(await f.completion.completeRemote(scope, active)).toEqual(active)
    expect(complete).toHaveBeenCalledOnce()
  })

  it.each(['receipt', 'history', 'owner'] as const)('refuses a missing %s at capture', async missing => {
    const f = await applied()
    if (missing === 'receipt') f.db.receipts.pop()
    if (missing === 'history') f.db.inbox.pop()
    if (missing === 'owner') f.db.ownerHeads!.pop()
    const before = structuredClone(f.db)
    await expect(f.completion.captureMetadataCreate(scope, f.registered)).rejects.toMatchObject({ code: 'bootstrap_incomplete' })
    expect(f.db).toEqual(before)
  })

  it('requires Metadata owner proof and prevents Legacy capture of the marked lineage', async () => {
    const f = await setup()
    await expect(f.completion.captureMetadataCreate(scope, f.registered)).rejects.toMatchObject({ code: 'bootstrap_incomplete' })
    await expect(f.capture.capture(scope, f.registered)).rejects.toMatchObject({ code: 'invalid_lineage_transition' })
    expect(await f.projects.read(scope, f.registered.project_id)).toEqual(f.registered)
  })

  it.each(['bootstrap', 'event', 'ciphertext', 'receipt', 'digest'] as const)('rejects mismatched %s', async kind => {
    const f = await applied()
    if (kind === 'bootstrap') {
      const row = f.db.projectBindings.find(row => (row as BrowserProjectBinding).project_id === f.registered.project_id)!
      Object.assign(row as object, { bootstrap_id: crypto.randomUUID() })
    } else if (kind === 'event') {
      Object.assign(f.db.receipts[0] as object, { event_id: crypto.randomUUID() })
    } else if (kind === 'ciphertext') {
      const row = f.db.outbox[0] as { raw_body: string }
      const wire = JSON.parse(row.raw_body)
      const cipher: string = wire.items[0].object.ciphertext
      wire.items[0].object.ciphertext = (cipher.startsWith('A') ? 'B' : 'A') + cipher.slice(1)
      row.raw_body = JSON.stringify(wire)
    } else if (kind === 'receipt') {
      Object.assign(f.db.receipts[0] as object, { server_sequence: 2 })
    } else {
      const inbox = f.db.inbox[0] as { applied_proof: { wire_digest: string } }
      const head = f.db.ownerHeads![0] as { history: Array<{ wire_digest: string }> }
      inbox.applied_proof.wire_digest = head.history[0]!.wire_digest = '0'.repeat(64)
    }
    const before = structuredClone(f.db)
    await expect(f.completion.captureMetadataCreate(scope, f.registered)).rejects.toThrow()
    expect(f.db).toEqual(before)
  })

  it('retries an interrupted completion with the same immutable server cohort', async () => {
    const f = await applied()
    const captured = await f.completion.captureMetadataCreate(scope, f.registered)
    const completing = await f.completion.markCompleting(scope, captured)
    const complete = vi.spyOn(cloudProjectsApi, 'completeBootstrap')
      .mockRejectedValueOnce(new Error('response lost')).mockResolvedValue(activeResponse(completing))
    const before = structuredClone(f.db)
    await expect(f.completion.completeRemote(scope, completing)).rejects.toMatchObject({ code: 'bootstrap_completion_failed' })
    expect(f.db).toEqual(before)
    await expect(f.completion.completeRemote(scope, completing)).resolves.toMatchObject({ remote_state: 'active' })
    expect(complete.mock.calls[0]!.slice(0, 3)).toEqual(complete.mock.calls[1]!.slice(0, 3))
    expect(f.db.outbox).toHaveLength(1)
    expect(f.db.ownerHeads).toHaveLength(1)
    expect(f.db.inbox).toHaveLength(1)
  })

  it('rejects stale capture CAS even when the Create proof is valid', async () => {
    const f = await applied()
    const fresh = await f.projects.confirmRegistration(scope, f.registered, { remote_state: 'initializing', remote_high_water: 1 })
    await expect(f.completion.captureMetadataCreate(scope, f.registered)).rejects.toMatchObject({ code: 'stale_lineage' })
    expect(await f.projects.read(scope, f.registered.project_id)).toEqual(fresh)
  })

  it.each(['lineage', 'receipt', 'response'] as const)('rejects changed %s during remote completion', async kind => {
    const f = await applied()
    const completing = await f.completion.markCompleting(scope, await f.completion.captureMetadataCreate(scope, f.registered))
    vi.spyOn(cloudProjectsApi, 'completeBootstrap').mockImplementation(async () => {
      const response = activeResponse(completing)
      if (kind === 'lineage') {
        const row = f.db.projectBindings.find(row => (row as BrowserProjectBinding).project_id === completing.project_id)!
        Object.assign(row as object, { remote_high_water: 1 })
      } else if (kind === 'receipt') {
        Object.assign(f.db.receipts[0] as object, { server_sequence: 2 })
      } else response.project.initial_event_count = 2
      return response
    })
    await expect(f.completion.completeRemote(scope, completing)).rejects.toMatchObject({
      code: kind === 'lineage' ? 'stale_lineage' : kind === 'receipt' ? 'conflicting_record' : 'bootstrap_response_rejected',
    })
    expect(await f.projects.read(scope, completing.project_id)).toMatchObject({ remote_state: 'initializing' })
  })
})
