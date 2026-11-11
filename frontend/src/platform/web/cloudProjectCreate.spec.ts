// @vitest-environment node
import { afterEach, expect, it, vi } from 'vitest'
import { encryptedSyncV2Api } from '@/api/encryptedSyncV2'
import { cloudProjectsApi } from '@/api/cloudProjects'
import { encodeV3MetadataPush, encryptedSyncV3Api, type V3MetadataPushItem } from '@/api/encryptedSyncV3'
import { encodeBase64Url } from '@/api/base64url'
import { wrapAmkWithPassphrase } from '@/crypto/wrapping'
import { BrowserCloudAccountSession } from './cloudAccountSession'
import { amk, fixture, scope } from './bootstrap.testFixture'
import { stageCloudProjectMetadata } from './cloudProjectCreate'
import { BrowserBootstrapCompletion } from './bootstrapCompletion'
import { BrowserSelfMetadataCreate } from './selfMetadataCreate'

afterEach(() => vi.restoreAllMocks())

async function setup() {
  const pending = new Map<string, string>()
  vi.stubGlobal('window', Object.assign(new EventTarget(), { localStorage: {
    getItem: (key: string) => pending.get(key) ?? null,
    setItem: (key: string, value: string) => pending.set(key, value),
  } }))
  const f = await fixture([])
  const capabilities = vi.spyOn(encryptedSyncV2Api, 'capabilities').mockResolvedValue({
    supported_transport_version: 2, writer_transport_version: 3, cutover_epoch: 2,
  })
  const register = vi.spyOn(cloudProjectsApi, 'registerBootstrap').mockImplementation(async (_token, projectId, body) => ({
    project: { project_id: projectId, bootstrap_id: body.bootstrap_id, origin_device_id: body.device_id,
      state: 'initializing', initial_event_count: null, initial_max_server_sequence: null },
    current_cursor: 0,
  }))
  const saved = new Map<string, { item: ReturnType<typeof JSON.parse>; sequence: number }>()
  const push = vi.spyOn(encryptedSyncV3Api, 'pushMetadata').mockImplementation(async (_token, device, items) => {
    const body = encodeV3MetadataPush(device, items)
    expect(f.db.outbox).toContainEqual(expect.objectContaining({ state: 'sealed', raw_body: body }))
    expect(body).not.toContain('Private project')
    const [item] = JSON.parse(body).items
    const previous = saved.get(item.event.event_id)
    if (previous) expect(item).toEqual(previous.item)
    const sequence = previous?.sequence ?? saved.size + 1
    saved.set(item.event.event_id, { item, sequence })
    return { protocol_version: 3, encrypted_sync_version: 3, current_cursor: saved.size,
      results: [{ event_id: item.event.event_id, server_sequence: sequence, duplicate: previous !== undefined }] }
  })
  const pull = vi.spyOn(encryptedSyncV3Api, 'pullRaw').mockImplementation(async (_token, _device, since) => JSON.stringify({
    protocol_version: 3, encrypted_sync_version: 3, next_cursor: saved.size, has_more: false,
    items: [...saved.values()].filter(({ sequence }) => sequence > since).map(({ item, sequence }) =>
      ({ ...item, event: { ...item.event, device_id: scope.deviceId, server_sequence: sequence } })),
  }))
  const complete = vi.spyOn(cloudProjectsApi, 'completeBootstrap').mockImplementation(async (_token, projectId, body) => {
    expect(f.db.inbox).toContainEqual(expect.objectContaining({ state: 'applied', server_sequence: body.initial_max_server_sequence }))
    return {
      project: { project_id: projectId, bootstrap_id: body.bootstrap_id, origin_device_id: body.device_id, state: 'active',
        initial_event_count: body.initial_event_count, initial_max_server_sequence: body.initial_max_server_sequence },
      current_cursor: saved.size,
    }
  })
  const storage = { sync: f.sync, projects: f.projects, completion: new BrowserBootstrapCompletion(f.transaction, () => true),
    selfMetadataCreate: new BrowserSelfMetadataCreate(f.transaction, f.sync),
    device: { bind: () => ({ registerOnce: async () => ({ deviceId: scope.deviceId, serverAckCursor: 0, ready: false as const }) }) } }
  const stage = (name = 'Private project', signal?: AbortSignal) => stageCloudProjectMetadata(scope, name, {
    auth: f.auth, bindings: f.bindings as never, keys: f.keys as never, storage,
  }, signal)
  return { ...f, register, push, pull, complete, stage, storage, pending, capabilities }

}

it('durably stages Metadata before push and applies only after receipt and server history', async () => {
  const f = await setup()
  const result = await f.stage()
  expect(result.serverSequence).toBe(1)
  expect(f.db.ownerHeads).toMatchObject([{ owner_id: result.projectId, history: [{ operation: 'create', server_sequence: 1 }] }])
  expect(f.db.inbox).toMatchObject([{ state: 'applied' }])
  expect(f.db.receipts).toHaveLength(1)
  expect(f.complete).toHaveBeenCalledOnce()
  expect(await f.projects.read(scope, result.projectId)).toMatchObject({ remote_state: 'active' })
  expect(await f.sync.readCheckpoint(scope)).toMatchObject({ ack_cursor: 0, proven_prefix: 0 })
  await expect(f.stage()).resolves.toEqual(result)
  expect(f.push).toHaveBeenCalledOnce()
  expect(f.pull).toHaveBeenCalledOnce()
  expect(f.db.ownerHeads).toHaveLength(1)
  expect(f.complete).toHaveBeenCalledOnce()
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

it('does not complete without the server echo', async () => {
  const f = await setup()
  f.pull.mockResolvedValueOnce(JSON.stringify({ protocol_version: 3, encrypted_sync_version: 3,
    next_cursor: 0, has_more: false, items: [] }))
  await expect(f.stage()).rejects.toMatchObject({ code: 'missing_history' })
  expect(f.complete).not.toHaveBeenCalled()
  expect(f.pending.size).toBe(1)
})

it('resumes completion with the same identities after the server response is lost', async () => {
  const f = await setup()
  f.complete.mockRejectedValueOnce(new Error('response lost'))
  await expect(f.stage()).rejects.toMatchObject({ code: 'bootstrap_completion_failed' })
  const pending = [...f.pending.values()][0]
  await expect(f.stage('Another project')).rejects.toMatchObject({ code: 'metadata_rejected' })
  expect([...f.pending.values()][0]).toBe(pending)
  await expect(f.stage()).resolves.toMatchObject({ serverSequence: 1 })
  expect(f.complete.mock.calls[0]!.slice(0, 3)).toEqual(f.complete.mock.calls[1]!.slice(0, 3))
  expect(f.register).toHaveBeenCalledOnce()
  expect(f.push).toHaveBeenCalledOnce()
})

it('cancels during completion and keeps the same retry identity', async () => {
  const f = await setup()
  const controller = new AbortController()
  const respond = f.complete.getMockImplementation()!
  f.complete.mockImplementationOnce(async (...args) => {
    controller.abort()
    expect(args[3]?.aborted).toBe(true)
    return respond(...args)
  })
  await expect(f.stage('Private project', controller.signal)).rejects.toMatchObject({ code: 'closed' })
  const pending = [...f.pending.values()][0]
  expect(await f.projects.read(scope, JSON.parse(pending!).projectId)).toMatchObject({ remote_state: 'initializing' })
  await expect(f.stage()).resolves.toMatchObject({ serverSequence: 1 })
  expect([...f.pending.values()][0]).toBe(pending)
  expect(f.push).toHaveBeenCalledOnce()
})

it('creates two distinct active projects through the public unlocked session', async () => {
  const f = await setup()
  const password = await wrapAmkWithPassphrase(amk, 'fixture passphrase')
  const session = new BrowserCloudAccountSession({
    auth: f.auth,
    cryptoTransport: { get: async () => ({ provisioned: true, recovery: null, password: {
      ...password, nonce: encodeBase64Url(password.nonce), ciphertext: encodeBase64Url(password.ciphertext),
      kdf: { ...password.kdf, salt: encodeBase64Url(password.kdf.salt) },
    } }) },
    legalTransport: { status: async () => ({ documents: [], missing_required_document_ids: [] }) } as never,
    openStorage: (async () => ({
      ...f.storage,
      read: async () => ({ local_account_id: scope.accountId, device_id: scope.deviceId }),
      ensure: async (account: string, user: string) => {
        expect([account, user]).toEqual([scope.accountId, scope.canonicalUserId])
      },
      close: async () => {},
    })) as never,
  })
  try {
    await session.login('fixture', 'fixture')
    await expect(session.createCloudProject('Private project')).rejects.toMatchObject({ code: 'key_locked' })
    await session.unlock('fixture passphrase')
    const first = await session.createCloudProject('Private project')
    await expect(session.createCloudProject(' Private project ')).resolves.toEqual(first)
    expect(f.complete).toHaveBeenCalledOnce()

    const second = await session.createCloudProject('Another project')
    expect(second.projectId).not.toBe(first.projectId)
    expect([first.serverSequence, second.serverSequence]).toEqual([1, 2])
    for (const { projectId } of [first, second]) {
      expect(await f.projects.read(scope, projectId)).toMatchObject({ remote_state: 'active' })
    }
    expect(f.push).toHaveBeenCalledTimes(2)
    expect(f.complete).toHaveBeenCalledTimes(2)
    expect(f.pending.size).toBe(1)
  } finally { await session.dispose() }
})

it('refuses older transport modes before reserving a project or writing pending state', async () => {
  const f = await setup()
  f.capabilities.mockResolvedValueOnce({ supported_transport_version: 2, writer_transport_version: 1, cutover_epoch: 0 })
  await expect(f.stage()).rejects.toMatchObject({ code: 'sync_transport_mode_incompatible' })
  expect(f.register).not.toHaveBeenCalled()
  expect(f.push).not.toHaveBeenCalled()
  expect(f.pending.size).toBe(0)
  expect(f.db.outbox).toHaveLength(0)
})
