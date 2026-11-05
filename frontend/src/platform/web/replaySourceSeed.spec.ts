// @vitest-environment node
import { beforeEach, expect, it, vi } from 'vitest'
import { NormalUserAuthRuntime } from '@/auth/userAuth'
import type { AuthoritativeAccountBinding } from '@/auth/accountBinding'
import type { RuntimeKeyContext } from '@/auth/keyContext'
import { generateAccountMasterKey } from '@/crypto'
import { encodeBase64Url } from '@/api/base64url'
import { cloudProjectsApi } from '@/api/cloudProjects'
import { encryptedSyncV2Api } from '@/api/encryptedSyncV2'
import { encryptedSyncV3Api } from '@/api/encryptedSyncV3'
import { sealNoteSyncEvent } from '@/cloud/encryptedSyncProtocol'
import type { NoteSyncRecord } from '@/cloud/noteSyncCodec'
import { sealProjectMetadataEvent, type ProjectMetadataEvent } from '@/cloud/projectMetadataCodec'
import metadataFixture from '@/cloud/__fixtures__/projectMetadataV2.json'
import noteFixture from '@/cloud/__fixtures__/noteSyncPlaintextV1.json'
import { seedReplayNoteSource } from './replaySourceSeed'
import { replayAnchor, type ReplayManifest, type ReplaySourceSeedReference } from './replayRecords'
import type { ReplaySnapshot } from './replayInventory'
import type { LocalProjectRecord } from './localAuthorityRecords'
import type { BrowserReceivedInboxRecord } from './syncDurability'

const scope = { accountId: '22222222-2222-4222-8222-222222222222', canonicalUserId: '11111111-1111-4111-8111-111111111111',
  deviceId: '33333333-3333-4333-8333-333333333333' }
const source = '44444444-4444-4444-8444-444444444444', project = 'project'
let auth: NormalUserAuthRuntime, bindings: AuthoritativeAccountBinding, keys: RuntimeKeyContext, state: ReplaySnapshot
type Commit = (expected: ReplaySnapshot, row: LocalProjectRecord, reference: ReplaySourceSeedReference, server: () => Promise<void>) => Promise<void>
let commit: ReturnType<typeof vi.fn<Commit>>, active: boolean, amk: Awaited<ReturnType<typeof generateAccountMasterKey>>
const encoded = (object: { crypto_version: number; aad_version: number; nonce: Uint8Array; ciphertext: Uint8Array }) =>
  ({ crypto_version: object.crypto_version, aad_version: object.aad_version, nonce: encodeBase64Url(object.nonce), ciphertext: encodeBase64Url(object.ciphertext) })
const row = (event: Record<string, unknown>, object: ReturnType<typeof encoded>, sequence: number): BrowserReceivedInboxRecord => ({
  record_version: 1, account_id: scope.accountId, device_id: scope.deviceId, event_id: event.event_id as string,
  server_sequence: sequence, protocol_version: 3, state: 'received', wire_item: JSON.stringify({ event: { ...event, device_id: source, server_sequence: sequence }, object }) })
const run = () => seedReplayNoteSource({ read: async () => structuredClone(state), readSource: vi.fn(), commit }, scope, project,
  new AbortController().signal, () => active, auth, bindings, keys)
async function appendMetadataUpdate(deviceId = source): Promise<void> {
  const genesis = JSON.parse(state.rows[1]!.wire_item).event
  const metadata = structuredClone(metadataFixture[0]!.event) as ProjectMetadataEvent
  metadata.header = { ...metadata.header, account_id: scope.canonicalUserId, project_id: project, entity_id: project,
    bootstrap_id: state.manifest!.anchor.registry.projects[0]!.bootstrap_id!, device_id: deviceId,
    event_id: crypto.randomUUID(), operation: 'update', revision: 2, generation: 2,
    parent_event_ids: [genesis.event_id] }
  metadata.metadata!.status = 'активен'
  const sealed = await sealProjectMetadataEvent(amk, metadata), sequence = state.rows.length + 1
  state.rows.push(row({ event_id: metadata.header.event_id, project_id: project, entity_id: project,
    entity_type: 'project_metadata', operation: 'upsert', revision: 2,
    updated_at: metadata.header.updated_at, deleted_at: null }, encoded(sealed), sequence))
  if (deviceId !== source) {
    const wire = JSON.parse(state.rows.at(-1)!.wire_item)
    wire.event.device_id = deviceId
    state.rows.at(-1)!.wire_item = JSON.stringify(wire)
  }
  state.manifest!.receive_cursor = sequence
  state.manifest!.anchor.registry.current_cursor = sequence
  vi.mocked(encryptedSyncV3Api.pullRaw).mockImplementation(async () => JSON.stringify({ protocol_version: 3,
    encrypted_sync_version: 3, items: [], next_cursor: sequence, has_more: false }))
}

beforeEach(async () => {
  vi.restoreAllMocks(); active = true; amk = await generateAccountMasterKey()
  auth = new NormalUserAuthRuntime({ login: async () => ({ access_token: 'test', refresh_token: 'test', access_expires_in: 600 }),
    me: async () => ({ id: scope.canonicalUserId, username: 'writer', email: 'writer@example.test', email_verified: true,
      role: 'user', status: 'active', created_at: 'now' }), refresh: vi.fn(), logout: async () => undefined })
  await auth.login('writer', 'password')
  bindings = { ensureForCurrentUser: vi.fn(async () => ({ context: auth.requireContext() })) } as unknown as AuthoritativeAccountBinding
  const lease = { localAccountId: scope.accountId, canonicalUserId: scope.canonicalUserId, authEpoch: auth.requireContext().authEpoch,
    isCurrent: () => active, use: async <T>(work: (key: typeof amk) => Promise<T>) => work(amk) }
  keys = { leaseForAccount: () => lease } as unknown as RuntimeKeyContext
  const note = structuredClone(noteFixture.plaintext.note) as NoteSyncRecord, noteId = 'initial-note', at = note.updated_at
  const noteEvent = { event_id: crypto.randomUUID(), project_id: project, entity_id: noteId, entity_type: 'note' as const,
    operation: 'upsert' as const, revision: 1, updated_at: at, deleted_at: null }
  const noteSealed = await sealNoteSyncEvent(amk, scope.canonicalUserId, noteEvent, null,
    { ...note, id: noteId, project_id: project, metadata: {} })
  const metadata = structuredClone(metadataFixture[0]!.event) as ProjectMetadataEvent
  metadata.header = { ...metadata.header, account_id: scope.canonicalUserId, project_id: project, entity_id: project,
    bootstrap_id: '55555555-5555-4555-8555-555555555555', device_id: source, event_id: crypto.randomUUID() }
  metadata.metadata!.status = 'активен'
  const metadataSealed = await sealProjectMetadataEvent(amk, metadata)
  const metaEvent = { event_id: metadata.header.event_id, project_id: project, entity_id: project, entity_type: 'project_metadata',
    operation: 'upsert', revision: 1, updated_at: metadata.header.updated_at, deleted_at: null }
  const anchor = replayAnchor({ current_cursor: 2, projects: [{ project_id: project, bootstrap_id: metadata.header.bootstrap_id,
    origin_device_id: source, state: 'active', initial_event_count: 1, initial_max_server_sequence: 1 }] },
  { supported_transport_version: 2, writer_transport_version: 3, cutover_epoch: 2 })
  const rows = [row(noteEvent, encoded(noteSealed.object), 1), row(metaEvent, encoded(metadataSealed), 2)]
  state = { manifest: { record_version: 4, contract_version: 4, metadata_cursor: 0, applied_cursor: 0, source_seeds: [],
    account_id: scope.accountId, canonical_user_id: scope.canonicalUserId, device_id: scope.deviceId,
    generation: crypto.randomUUID(), anchor, receive_cursor: 2, ciphertext_bytes: 0, phase: 'received', revision: 1 },
  rows, bindings: [], owners: [], sources: [] }
  vi.spyOn(cloudProjectsApi, 'listBootstraps').mockImplementation(async () => structuredClone(anchor.registry))
  vi.spyOn(encryptedSyncV2Api, 'capabilities').mockImplementation(async () => structuredClone(anchor.transport))
  vi.spyOn(encryptedSyncV3Api, 'pullRaw').mockImplementation(async () => JSON.stringify({ protocol_version: 3,
    encrypted_sync_version: 3, items: [], next_cursor: 2, has_more: false }))
  commit = vi.fn<Commit>(async (expected, sourceRow, reference, server) => {
    await server(); expect(expected).toEqual(state)
    state.sources!.push(sourceRow)
    const manifest = state.manifest as ReplayManifest
    state.manifest = { ...manifest, source_seeds: [...manifest.source_seeds!, reference], revision: manifest.revision + 1 }
  })
})

it('authenticates true Note-before-Metadata history, seals one source and reopens without a second write', async () => {
  expect(await run()).toMatchObject({ status: 'seeded', projectOpened: false, writerAuthorized: false })
  expect(state.sources).toHaveLength(1)
  expect(JSON.stringify(state.sources)).not.toContain('Cover vector')
  expect(state.rows.every(record => record.state === 'received')).toBe(true)
  expect(state.manifest).toMatchObject({ applied_cursor: 0, metadata_cursor: 0 })
  expect(await run()).toMatchObject({ status: 'already_seeded', projectOpened: false })
  expect(commit).toHaveBeenCalledTimes(1)
})
it('rechecks the source anchor through explicitly selected recovery delivery', async () => {
  const recovery = vi.spyOn(encryptedSyncV3Api, 'recoveryRaw').mockResolvedValue(JSON.stringify({
    protocol_version: 3, encrypted_sync_version: 3, items: [], next_cursor: 2, has_more: false,
  }))
  expect(await seedReplayNoteSource({ read: async () => structuredClone(state), readSource: vi.fn(), commit }, scope, project,
    new AbortController().signal, () => active, auth, bindings, keys, 'recovery')).toMatchObject({ status: 'seeded' })
  expect(recovery).toHaveBeenCalled()
  expect(encryptedSyncV3Api.pullRaw).not.toHaveBeenCalled()
})
it.each(['count', 'max', 'origin', 'bootstrap', 'genesis', 'note', 'identity', 'family'] as const)('rejects %s with no source or proof', async kind => {
  const manifest = state.manifest!, remote = manifest.anchor.registry.projects[0]!
  if (kind === 'count') remote.initial_event_count = 0
  if (kind === 'max') remote.initial_max_server_sequence = 2
  if (kind === 'origin') remote.origin_device_id = crypto.randomUUID()
  if (kind === 'bootstrap') remote.bootstrap_id = crypto.randomUUID()
  if (kind === 'genesis') { const wire = JSON.parse(state.rows[1]!.wire_item); wire.object.ciphertext = (wire.object.ciphertext[0] === 'A' ? 'B' : 'A') + wire.object.ciphertext.slice(1); state.rows[1]!.wire_item = JSON.stringify(wire) }
  if (kind === 'note') { const wire = JSON.parse(state.rows[0]!.wire_item); wire.object.ciphertext = (wire.object.ciphertext[0] === 'A' ? 'B' : 'A') + wire.object.ciphertext.slice(1); state.rows[0]!.wire_item = JSON.stringify(wire) }
  if (kind === 'identity') { const wire = JSON.parse(state.rows[0]!.wire_item); wire.event.entity_id = 'foreign-note'; state.rows[0]!.wire_item = JSON.stringify(wire) }
  if (kind === 'family') { const wire = JSON.parse(state.rows[1]!.wire_item); wire.event.entity_type = 'unknown_family'; state.rows[1]!.wire_item = JSON.stringify(wire) }
  vi.mocked(cloudProjectsApi.listBootstraps).mockImplementation(async () => structuredClone(manifest.anchor.registry))
  await expect(run()).rejects.toThrow()
  expect(commit).not.toHaveBeenCalled(); expect(state.sources).toEqual([])
  expect(state.manifest!.applied_cursor).toBe(0)
})
it('lost commit response is reconciled from the server on retry without another write', async () => {
  const original = commit.getMockImplementation()!
  commit.mockImplementationOnce(async (...args: Parameters<typeof original>) => { await original(...args); throw Error('lost_response') })
  await expect(run()).rejects.toThrow('lost_response')
  expect(await run()).toMatchObject({ status: 'already_seeded' })
  expect(commit).toHaveBeenCalledTimes(1)
})
it('rejects an incomplete retained feed before source creation', async () => {
  state.rows.pop()
  await expect(run()).rejects.toThrow()
  expect(commit).not.toHaveBeenCalled()
  expect(state.sources).toEqual([])
})
it('rejects a genuine but inactive Metadata genesis', async () => {
  const metadata = structuredClone(metadataFixture[0]!.event) as ProjectMetadataEvent
  metadata.header = { ...metadata.header, account_id: scope.canonicalUserId, project_id: project, entity_id: project,
    bootstrap_id: state.manifest!.anchor.registry.projects[0]!.bootstrap_id!, device_id: source,
    event_id: state.rows[1]!.event_id }
  metadata.metadata!.status = 'завершен'
  const sealed = await sealProjectMetadataEvent(amk, metadata)
  const wire = JSON.parse(state.rows[1]!.wire_item)
  wire.object = encoded(sealed)
  state.rows[1]!.wire_item = JSON.stringify(wire)
  await expect(run()).rejects.toThrow()
  expect(commit).not.toHaveBeenCalled()
})
it('rejects AMK lease loss during the fresh server check without committing', async () => {
  vi.mocked(cloudProjectsApi.listBootstraps).mockImplementation(async () => {
    active = false
    return structuredClone(state.manifest!.anchor.registry)
  })
  await expect(run()).rejects.toThrow()
  expect(commit).not.toHaveBeenCalled()
  expect(state.sources).toEqual([])
})
it('rejects logout during the fresh server check without committing', async () => {
  vi.mocked(cloudProjectsApi.listBootstraps).mockImplementation(async () => {
    await auth.logout()
    return structuredClone(state.manifest!.anchor.registry)
  })
  await expect(run()).rejects.toThrow()
  expect(commit).not.toHaveBeenCalled()
  expect(state.sources).toEqual([])
})
it.each(['fork', 'foreign_source', 'delete'] as const)('rejects unsupported %s Metadata history before a seed', async kind => {
  await appendMetadataUpdate(kind === 'foreign_source' ? crypto.randomUUID() : source)
  if (kind === 'fork') await appendMetadataUpdate()
  if (kind === 'delete') {
    const wire = JSON.parse(state.rows.at(-1)!.wire_item)
    wire.event.operation = 'delete'
    state.rows.at(-1)!.wire_item = JSON.stringify(wire)
  }
  await expect(run()).rejects.toThrow()
  expect(commit).not.toHaveBeenCalled()
  expect(state.sources).toEqual([])
  expect(state.manifest!.applied_cursor).toBe(0)
})
it('rejects additional Note source material outside the initial bootstrap cohort', async () => {
  const note = structuredClone(noteFixture.plaintext.note) as NoteSyncRecord
  const event = { event_id: crypto.randomUUID(), project_id: project, entity_id: 'later-note', entity_type: 'note' as const,
    operation: 'upsert' as const, revision: 1, updated_at: note.updated_at, deleted_at: null }
  const sealed = await sealNoteSyncEvent(amk, scope.canonicalUserId, event, null,
    { ...note, id: event.entity_id, project_id: project, metadata: {} })
  state.rows.push(row(event, encoded(sealed.object), 3))
  state.manifest!.receive_cursor = 3
  state.manifest!.anchor.registry.current_cursor = 3
  await expect(run()).rejects.toThrow()
  expect(commit).not.toHaveBeenCalled()
  expect(state.sources).toEqual([])
})
