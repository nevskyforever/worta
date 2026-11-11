// @vitest-environment node
import { afterEach, expect, it, vi } from 'vitest'
import { cloudProjectsApi } from '@/api/cloudProjects'
import { encodeV3MetadataPush, encryptedSyncV3Api, type V3MetadataPushItem } from '@/api/encryptedSyncV3'
import { fixture, scope } from './bootstrap.testFixture'
import { stageCloudProjectMetadata } from './cloudProjectCreate'
import { BrowserSelfMetadataCreate } from './selfMetadataCreate'

afterEach(() => vi.restoreAllMocks())

async function setup() {
  const pending = new Map<string, string>()
  vi.stubGlobal('window', { localStorage: {
    getItem: (key: string) => pending.get(key) ?? null,
    setItem: (key: string, value: string) => pending.set(key, value),
  } })
  const f = await fixture([])
  const register = vi.spyOn(cloudProjectsApi, 'registerBootstrap').mockImplementation(async (_token, projectId, body) => ({
    project: { project_id: projectId, bootstrap_id: body.bootstrap_id, origin_device_id: body.device_id,
      state: 'initializing', initial_event_count: null, initial_max_server_sequence: null },
    current_cursor: 0,
  }))
  let saved: ReturnType<typeof JSON.parse> | undefined
  const push = vi.spyOn(encryptedSyncV3Api, 'pushMetadata').mockImplementation(async (_token, device, items) => {
    const body = encodeV3MetadataPush(device, items)
    expect(f.db.outbox).toMatchObject([{ state: 'sealed', raw_body: body }])
    expect(body).not.toContain('Private project')
    const duplicate = saved !== undefined
    const [item] = JSON.parse(body).items
    if (duplicate) expect(item).toEqual(saved)
    saved = item
    return { protocol_version: 3, encrypted_sync_version: 3, current_cursor: 1,
      results: [{ event_id: item.event.event_id, server_sequence: 1, duplicate }] }
  })
  const pull = vi.spyOn(encryptedSyncV3Api, 'pullRaw').mockImplementation(async () => JSON.stringify({
    protocol_version: 3, encrypted_sync_version: 3, next_cursor: 1, has_more: false,
    items: [{ ...saved, event: { ...saved.event, device_id: scope.deviceId, server_sequence: 1 } }],
  }))
  const complete = vi.spyOn(cloudProjectsApi, 'completeBootstrap')
  const stage = () => stageCloudProjectMetadata(scope, 'Private project', {
    auth: f.auth, bindings: f.bindings as never, keys: f.keys as never,
    storage: { sync: f.sync, projects: f.projects, selfMetadataCreate: new BrowserSelfMetadataCreate(f.transaction, f.sync),
      device: { bind: () => ({ registerOnce: async () => ({ deviceId: scope.deviceId, serverAckCursor: 0, ready: false }) }) } },
  })
  return { ...f, register, push, pull, complete, stage }
}

it('durably stages Metadata before push and applies only after receipt and server history', async () => {
  const f = await setup()
  const result = await f.stage()
  expect(result.serverSequence).toBe(1)
  expect(f.db.ownerHeads).toMatchObject([{ owner_id: result.projectId, history: [{ operation: 'create', server_sequence: 1 }] }])
  expect(f.db.inbox).toMatchObject([{ state: 'applied' }])
  expect(f.db.receipts).toHaveLength(1)
  expect(f.complete).not.toHaveBeenCalled()
  expect(await f.sync.readCheckpoint(scope)).toMatchObject({ ack_cursor: 0, proven_prefix: 0 })
  await expect(f.stage()).resolves.toEqual(result)
  expect(f.push).toHaveBeenCalledOnce()
  expect(f.pull).toHaveBeenCalledOnce()
  expect(f.db.ownerHeads).toHaveLength(1)
})

it('replays identical ciphertext and project after a lost push response', async () => {
  const f = await setup()
  const send = f.push.getMockImplementation()!
  f.push.mockImplementationOnce(async (...args) => { await send(...args); throw new Error('lost response') })
  await expect(f.stage()).rejects.toThrow('lost response')
  expect(f.db.outbox).toHaveLength(1)
  expect(f.db.receipts).toHaveLength(0)
  expect(f.db.ownerHeads).toHaveLength(0)
  await expect(f.stage()).resolves.toMatchObject({ serverSequence: 1 })
  expect(f.register).toHaveBeenCalledOnce()
  expect(f.push).toHaveBeenCalledTimes(2)
  expect(f.db.receipts).toMatchObject([{ duplicate: true }])
  expect(f.db.outbox).toHaveLength(1)
})

it('resumes from the stored receipt when fetching history fails', async () => {
  const f = await setup()
  f.pull.mockRejectedValueOnce(new Error('network down'))
  await expect(f.stage()).rejects.toThrow('network down')
  expect(f.db.receipts).toHaveLength(1)
  expect(f.db.ownerHeads).toHaveLength(0)
  await expect(f.stage()).resolves.toMatchObject({ serverSequence: 1 })
  expect(f.push).toHaveBeenCalledOnce()
})

it('stops before receipt or owner commit when the key is revoked during push', async () => {
  const f = await setup()
  const send = f.push.getMockImplementation()!
  f.push.mockImplementationOnce(async (...args: [string, string, readonly V3MetadataPushItem[], AbortSignal?]) => {
    const response = await send(...args)
    f.lock()
    return response
  })
  await expect(f.stage()).rejects.toMatchObject({ code: 'closed' })
  expect(f.db.outbox).toHaveLength(1)
  expect(f.db.receipts).toHaveLength(0)
  expect(f.pull).not.toHaveBeenCalled()
  expect(f.db.ownerHeads).toHaveLength(0)
})
