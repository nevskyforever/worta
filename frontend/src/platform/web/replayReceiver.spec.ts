// @vitest-environment node
import { beforeEach, describe, expect, it, vi, type Mock, type MockInstance } from 'vitest'
import { NormalUserAuthRuntime } from '@/auth/userAuth'
import type { AuthoritativeKeyContextLease } from '@/auth/keyContext'
import { cloudProjectsApi } from '@/api/cloudProjects'
import { ApiError } from '@/api/client'
import { encryptedSyncV2Api } from '@/api/encryptedSyncV2'
import { encryptedSyncV3Api } from '@/api/encryptedSyncV3'
import { encodeBase64Url } from '@/api/base64url'
import { generateAccountMasterKey } from '@/crypto'
import { sealProjectMetadataEvent, type ProjectMetadataEvent } from '@/cloud/projectMetadataCodec'
import vectors from '@/cloud/__fixtures__/projectMetadataV2.json'
import type { BrowserReceivedInboxRecord } from './syncDurability'
import { replayAnchor, validateReplayManifest, type ReplayAnchor } from './replayRecords'
import type { ReplaySnapshot, ReplayStorage, openBrowserReplayStorage } from './replayStorage'
import { receiveBrowserAccountHistory } from './replayReceiver'

const base = vectors[0]!.event as ProjectMetadataEvent
const scope = { accountId: '22222222-2222-4222-8222-222222222222', canonicalUserId: base.header.account_id, deviceId: base.header.device_id }
let auth: NormalUserAuthRuntime, lease: AuthoritativeKeyContextLease, active: boolean, controller: AbortController
let rows: unknown[], anchor: ReplayAnchor, state: ReplaySnapshot, store: ReplayStorage
let openStorage: Mock<typeof openBrowserReplayStorage>, pull: MockInstance<typeof encryptedSyncV3Api.pullRaw>
const wirePage = (selected: unknown[], next: number, more: boolean) => JSON.stringify({ protocol_version: 3, encrypted_sync_version: 3, items: selected, next_cursor: next, has_more: more })
const run = () => receiveBrowserAccountHistory({ auth, lease, scope, signal: controller.signal, current: () => active, openStorage })

beforeEach(async () => {
  active = true; controller = new AbortController()
  auth = new NormalUserAuthRuntime({ login: async () => ({ access_token: 'test', refresh_token: 'test', access_expires_in: 600 }),
    me: async () => ({ id: scope.canonicalUserId, username: 'writer', email: 'test@example.test', email_verified: true, role: 'user', status: 'active', created_at: 'now' }),
    refresh: vi.fn(), logout: async () => undefined })
  await auth.login('writer', 'password')
  lease = { localAccountId: scope.accountId, canonicalUserId: scope.canonicalUserId, authEpoch: auth.requireContext().authEpoch,
    keyEpoch: 1, keyContextId: 'key', isCurrent: () => active, use: vi.fn() }
  const key = await generateAccountMasterKey()
  try {
    rows = await Promise.all(['project', 'second-project', 'project'].map(async (project, index) => {
      const event = structuredClone(base)
      event.header = { ...event.header, event_id: crypto.randomUUID(), project_id: project, entity_id: project }
      const object = await sealProjectMetadataEvent(key, event)
      return { event: { event_id: event.header.event_id, device_id: event.header.device_id, project_id: project, entity_id: project,
        entity_type: 'project_metadata', operation: 'upsert', revision: 1, updated_at: event.header.updated_at, deleted_at: null, server_sequence: index + 1 },
      object: { ...object, nonce: encodeBase64Url(object.nonce), ciphertext: encodeBase64Url(object.ciphertext) } }
    }))
  } finally { key.fill(0) }
  anchor = replayAnchor({ current_cursor: 3, projects: ['project', 'second-project'].map(project_id => ({ project_id, bootstrap_id: crypto.randomUUID(),
    origin_device_id: scope.deviceId, state: 'active', initial_event_count: 0, initial_max_server_sequence: 0 })) },
  { supported_transport_version: 2, writer_transport_version: 3, cutover_epoch: 2 })
  state = { manifest: null, rows: [], bindings: [], owners: [] }
  // Unit tests cover receiver orchestration only. Native IDB atomicity/schema
  // and restart are exercised by the real Chromium/PostgreSQL acceptance.
  store = {
    authenticateMaps: vi.fn(), authenticateDocuments: vi.fn(), applyDocumentGenesis: vi.fn(), recoverV10: vi.fn(), importRecoveryProject: vi.fn(), stageRecoveryExtension: vi.fn(),
    contentAuthorOwner:vi.fn(),receiveContentAuthorEcho:vi.fn(),inspectNoteReaderReadiness: vi.fn(), applyContentUpdate: vi.fn(), applyOrderedContentUpdate: vi.fn(), applyMetadata: vi.fn(), applyStructure: vi.fn(), applyInitialNotes: vi.fn(), seedNoteSource: vi.fn(),
    start: vi.fn(async () => {
      state.manifest ??= { record_version: 5, contract_version: 5, metadata_cursor: 0, applied_cursor: 0, source_seeds: [], account_id: scope.accountId, canonical_user_id: scope.canonicalUserId,
        device_id: scope.deviceId, generation: crypto.randomUUID(), anchor, receive_cursor: 0, ciphertext_bytes: 0, phase: 'receiving', revision: 1 }
      return structuredClone(state)
    }),
    read: vi.fn(async () => structuredClone(state)),
    commitPage: vi.fn(async (_expected, _since, raw) => {
      const page = JSON.parse(raw)
      state.rows.push(...page.items.map((item: { event: { event_id: string; server_sequence: number } }): BrowserReceivedInboxRecord => ({ record_version: 1,
        account_id: scope.accountId, device_id: scope.deviceId, event_id: item.event.event_id, server_sequence: item.event.server_sequence,
        protocol_version: 3, state: 'received', wire_item: JSON.stringify(item) })))
      state.manifest!.receive_cursor = page.next_cursor
      return structuredClone(state)
    }),
    finish: vi.fn(async () => { state.manifest!.phase = 'received'; return structuredClone(state) }), close: vi.fn(async () => undefined),
  }
  openStorage = vi.fn(async () => store)
  vi.spyOn(encryptedSyncV2Api, 'capabilities').mockImplementation(async () => structuredClone(anchor.transport))
  vi.spyOn(cloudProjectsApi, 'listBootstraps').mockImplementation(async () => structuredClone(anchor.registry))
  pull = vi.spyOn(encryptedSyncV3Api, 'pullRaw').mockImplementation(async (_token, _device, since, limit) => {
    const count = Math.min(2, limit!, rows.length - since), next = since + count
    return wirePage(rows.slice(since, next), next, next < rows.length)
  })
})

describe('Online-Only ciphertext receiver, without apply proofs', () => {
  it('uses the explicit recovery delivery with the pinned snapshot and no ordinary pull', async () => {
    const recovery = vi.spyOn(encryptedSyncV3Api, 'recoveryRaw').mockImplementation(async (_token, since, limit, cursor, epoch) => {
      expect(cursor).toBe(3); expect(epoch).toBe(2)
      const next = Math.min(since + limit, rows.length)
      return wirePage(rows.slice(since, next), next, next < rows.length)
    })
    expect(await receiveBrowserAccountHistory({ auth, lease, scope, signal: controller.signal, current: () => active, openStorage, delivery: 'recovery' }))
      .toMatchObject({ events: 3, ready: false, projectOpened: false })
    expect(recovery).toHaveBeenCalled(); expect(pull).not.toHaveBeenCalled()
  })
  it('rejects a changed retained wire before any anchor transition or write', async () => {
    const recovery = vi.spyOn(encryptedSyncV3Api, 'recoveryRaw').mockImplementation(async (_token, since, limit, cursor) => {
      const next = Math.min(since + limit, rows.length)
      return wirePage(rows.slice(since, next), next, next < cursor)
    })
    const input = { auth, lease, scope, signal: controller.signal, current: () => active, openStorage, delivery: 'recovery' as const }
    await receiveBrowserAccountHistory(input)
    const before = structuredClone(state), starts = vi.mocked(store.start).mock.calls.length, commits = vi.mocked(store.commitPage).mock.calls.length
    anchor = { ...anchor, registry: { ...anchor.registry, current_cursor: 4 } }
    recovery.mockImplementationOnce(async () => {
      const changed = structuredClone(rows[0]) as { object: { ciphertext: string } }
      changed.object.ciphertext = (changed.object.ciphertext[0] === 'A' ? 'B' : 'A') + changed.object.ciphertext.slice(1)
      return wirePage([changed, ...rows.slice(1)], 3, true)
    })
    await expect(receiveBrowserAccountHistory(input)).rejects.toMatchObject({ code: 'retained_wire_mismatch' })
    expect(state).toEqual(before)
    expect(store.start).toHaveBeenCalledTimes(starts)
    expect(store.commitPage).toHaveBeenCalledTimes(commits)
  })
  it('rejects an out-of-order retained prefix before changing the cache', async () => {
    const recovery = vi.spyOn(encryptedSyncV3Api, 'recoveryRaw').mockImplementation(async (_token, since, limit, cursor) => {
      const next = Math.min(since + limit, rows.length)
      return wirePage(rows.slice(since, next), next, next < cursor)
    })
    const input = { auth, lease, scope, signal: controller.signal, current: () => active, openStorage, delivery: 'recovery' as const }
    await receiveBrowserAccountHistory(input)
    const before = structuredClone(state), starts = vi.mocked(store.start).mock.calls.length
    anchor = { ...anchor, registry: { ...anchor.registry, current_cursor: 4 } }
    recovery.mockImplementationOnce(async () => wirePage([rows[1], rows[0], rows[2]], 3, true))
    await expect(receiveBrowserAccountHistory(input)).rejects.toThrow()
    expect(state).toEqual(before)
    expect(store.start).toHaveBeenCalledTimes(starts)
  })
  it('stages real shared-codec AEAD across projects/pages with no decrypt, ACK, push or declaration', async () => {
    const network = vi.spyOn(globalThis, 'fetch')
    expect(await run()).toMatchObject({ kind: 'received_ciphertext', cursor: 3, events: 3, ready: false,
      declarationAllowed: false, writerAuthorized: false, projectOpened: false, ownerProofs: 0 })
    expect(pull.mock.calls.map(call => call[2])).toEqual([0, 2])
    expect(lease.use).not.toHaveBeenCalled(); expect(network).not.toHaveBeenCalled()
  })
  it('reconciles the entire zero-origin prefix before resuming after network interruption', async () => {
    pull.mockImplementationOnce(async () => wirePage(rows.slice(0, 2), 2, true)).mockRejectedValueOnce(new ApiError(0, 'network_error', 'secret'))
    await expect(run()).rejects.toMatchObject({ code: 'connection_failed' })
    expect(state.manifest!.phase).toBe('receiving'); expect(state.rows).toHaveLength(2)
    const generation = state.manifest!.generation
    expect(await run()).toMatchObject({ events: 3, generation })
    expect(state.rows).toHaveLength(3)
    expect(store.commitPage).toHaveBeenCalledTimes(2)
  })
  it('revalidates a completed cache online without appending duplicate events', async () => {
    const first = await run(), commits = vi.mocked(store.commitPage).mock.calls.length
    expect(await run()).toEqual(first)
    expect(store.commitPage).toHaveBeenCalledTimes(commits)
    expect(pull).toHaveBeenCalledTimes(4)
  })
  it('rejects changed retained wire rather than trusting the saved receive cursor', async () => {
    await run()
    state.rows[0]!.wire_item = state.rows[0]!.wire_item.replace('project_metadata', 'stage')
    await expect(run()).rejects.toMatchObject({ code: 'retained_wire_mismatch' })
  })
  it.each(['gap', 'duplicate', 'unknown_family', 'unknown_version', 'plaintext_field', 'foreign_account', 'unknown_project', 'terminal_page'] as const)('rejects %s without final receipt', async kind => {
    const page = JSON.parse(wirePage(rows, 3, false))
    if (kind === 'gap') page.items[0].event.server_sequence = 2
    if (kind === 'duplicate') page.items[2].event.event_id = page.items[0].event.event_id
    if (kind === 'unknown_family') page.items[0].event.entity_type = 'future'
    if (kind === 'unknown_version') page.protocol_version = 4
    if (kind === 'plaintext_field') page.items[0].object.content = 'SECRET_PLAINTEXT'
    if (kind === 'foreign_account') page.items[0].event = { event_id: crypto.randomUUID(), device_id: scope.deviceId, canonical_user_id: crypto.randomUUID(), scope: 'account',
      entity_id: 'folder', entity_type: 'folder', operation: 'upsert', revision: 1, updated_at: base.header.updated_at, deleted_at: null, server_sequence: 1 }
    if (kind === 'unknown_project') page.items[0].event.project_id = page.items[0].event.entity_id = 'not-registered'
    if (kind === 'terminal_page') { page.items.pop(); page.next_cursor = 2 }
    pull.mockResolvedValue(JSON.stringify(page))
    await expect(run()).rejects.toThrow()
    expect(store.finish).not.toHaveBeenCalled(); expect(state.rows).toHaveLength(0)
  })
  it.each(['registry', 'epoch'] as const)('rejects changed final %s without marking received', async kind => {
    if (kind === 'registry') vi.mocked(cloudProjectsApi.listBootstraps).mockResolvedValueOnce(structuredClone(anchor.registry)).mockResolvedValue({ ...anchor.registry, current_cursor: 4 })
    else vi.mocked(encryptedSyncV2Api.capabilities).mockResolvedValueOnce(anchor.transport).mockResolvedValue({ ...anchor.transport, cutover_epoch: 3 })
    await expect(run()).rejects.toMatchObject({ code: 'server_changed' })
    expect(store.finish).not.toHaveBeenCalled()
  })
  it.each(['abort', 'lock', 'logout'] as const)('rejects %s during receive without a late receipt', async kind => {
    pull.mockImplementationOnce(async () => {
      if (kind === 'abort') controller.abort()
      if (kind === 'lock') active = false
      if (kind === 'logout') await auth.logout()
      return wirePage(rows, 3, false)
    })
    await expect(run()).rejects.toMatchObject({ code: 'session_closed' })
    expect(store.finish).not.toHaveBeenCalled(); expect(store.commitPage).not.toHaveBeenCalled()
  })
  it.each(['sync_device_not_registered', 'compression_reader_required', 'content_note_reader_required', 'map_reader_required', 'document_reader_required', 'progress_reader_required'])('preserves server gate %s', async code => {
    pull.mockRejectedValue(new ApiError(409, code, 'DO_NOT_EXPOSE'))
    await expect(run()).rejects.toMatchObject({ code })
    expect(store.finish).not.toHaveBeenCalled()
  })
  it('does not return cached success after a server failure', async () => {
    await run(); pull.mockRejectedValue(new ApiError(503, 'unavailable', 'SECRET'))
    await expect(run()).rejects.toMatchObject({ code: 'server_rejected' })
  })
  it('checks empty accounts against the server and returns no project state', async () => {
    rows = []; anchor.registry = { projects: [], current_cursor: 0 }
    expect(await run()).toMatchObject({ events: 0, cursor: 0, projectOpened: false, ownerProofs: 0 })
    expect(pull).toHaveBeenCalledOnce(); expect(store.commitPage).not.toHaveBeenCalled()
  })
  it.each(['limit', 'mode', 'lineage', 'key_binding'] as const)('rejects %s before opening storage', async kind => {
    if (kind === 'limit') anchor.registry.current_cursor = 201
    if (kind === 'mode') anchor.transport.writer_transport_version = 2
    if (kind === 'lineage') anchor.registry.projects[0]!.state = 'legacy'
    if (kind === 'key_binding') lease = { ...lease, localAccountId: crypto.randomUUID() }
    await expect(run()).rejects.toThrow(); expect(openStorage).not.toHaveBeenCalled()
  })
  it('strictly validates stored manifest versions, identity, phase and technical fields', async () => {
    await run()
    const original = state.manifest!
    expect(validateReplayManifest(original, scope, 5)).toEqual(original)
    for (const invalid of [{ ...original, record_version: 3 }, { ...original, account_id: crypto.randomUUID() }, { ...original, content: 'PLAINTEXT' },
      { ...original, receive_cursor: 4 }, { ...original, ciphertext_bytes: -1 }, { ...original, revision: Number.MAX_SAFE_INTEGER + 1 }]) {
      expect(() => validateReplayManifest(invalid, scope, 5)).toThrow()
    }
  })
})
