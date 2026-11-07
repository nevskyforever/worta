// @vitest-environment node
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { NormalUserAuthRuntime } from '@/auth/userAuth'
import { encryptedSyncV2Api } from '@/api/encryptedSyncV2'
import { encryptedSyncV3Api, type V3PullItem } from '@/api/encryptedSyncV3'
import { cloudProjectsApi } from '@/api/cloudProjects'
import { ApiError } from '@/api/client'
import { generateAccountMasterKey, type AccountMasterKey } from '@/crypto'
import { sealProjectMetadataEvent, type ProjectMetadataEvent } from '@/cloud/projectMetadataCodec'
import vectors from '@/cloud/__fixtures__/projectMetadataV2.json'
import { readBrowserCloudProject } from './cloudRead'
const base = vectors[0]!.event as ProjectMetadataEvent
let amk: AccountMasterKey, active: boolean
let auth: NormalUserAuthRuntime
let rows: V3PullItem[]
let high = 1
async function item(event: ProjectMetadataEvent, seq: number): Promise<V3PullItem> {
  return { event: { event_id: event.header.event_id, device_id: event.header.device_id, project_id: event.header.project_id,
    entity_id: event.header.entity_id, entity_type: 'project_metadata', operation: event.header.operation === 'delete' ? 'delete' : 'upsert',
    revision: event.header.revision, updated_at: event.header.updated_at, deleted_at: event.deleted_at, server_sequence: seq }, object: await sealProjectMetadataEvent(amk, event) }
}
const read = () => readBrowserCloudProject({ auth, deviceId: base.header.device_id, projectId: 'project', signal: new AbortController().signal,
  current: () => active, lease: { localAccountId: 'local', canonicalUserId: base.header.account_id, authEpoch: auth.requireContext().authEpoch,
    keyEpoch: 1, keyContextId: 'key', isCurrent: () => active, use: async fn => fn(amk) } })
beforeEach(async () => {
  amk = await generateAccountMasterKey(); active = true; high = 1
  auth = new NormalUserAuthRuntime({ login: async () => ({ access_token: 'test', refresh_token: 'test', access_expires_in: 60 }),
    me: async () => ({ id: base.header.account_id, username: 'writer', email: 'test@example.test', email_verified: true, role: 'user', status: 'active', created_at: 'now' }),
    refresh: vi.fn(), logout: async () => undefined })
  await auth.login('writer', 'test')
  rows = [await item(structuredClone(base), 1)]
  vi.spyOn(encryptedSyncV2Api, 'capabilities').mockImplementation(async () => ({ supported_transport_version: 2, writer_transport_version: 3, cutover_epoch: 2 }))
  vi.spyOn(cloudProjectsApi, 'listBootstraps').mockImplementation(async () => ({ current_cursor: high, projects: [{ project_id: 'project', bootstrap_id: base.header.bootstrap_id,
    origin_device_id: base.header.device_id, state: 'active', initial_event_count: 0, initial_max_server_sequence: 0 }] }))
  vi.spyOn(encryptedSyncV3Api, 'pull').mockImplementation(async () => ({ protocol_version: 3, encrypted_sync_version: 3, items: structuredClone(rows), next_cursor: high, has_more: false }))
})
describe('read-only server Metadata preview', () => {
  it('authenticates real AEAD and reuses the shared history reducer without writes/declarations', async () => {
    const fetch = vi.spyOn(globalThis, 'fetch')
    expect(await read()).toMatchObject({ kind: 'metadata_only', projectId: 'project', metadata: base.metadata, cursor: 1, ready: false, writerAuthorized: false })
    expect(fetch).not.toHaveBeenCalled()
  })
  it('reads authenticated project metadata including a non-null cover reference', async () => {
    rows = [await item(vectors[2]!.event as ProjectMetadataEvent, 1)]
    high = 1
    const result = await read()
    expect(result.metadata.name).toBe('Cover vector')
    expect(result.metadata.cover_reference).toMatchObject({ blob_id: '123e4567-e89b-42d3-a456-426614174004', mime_type: 'image/jpeg' })
  })
  it('validates a two-page account sequence and preserves the exact update', async () => {
    const update = structuredClone(base); update.header = { ...update.header, event_id: crypto.randomUUID(), operation: 'update', parent_event_ids: [base.header.event_id], revision: 2, generation: 2 }; update.metadata!.name = 'Updated'
    rows.push(await item(update, 2)); high = 2
    vi.mocked(encryptedSyncV3Api.pull).mockImplementation(async (_token, _device, since) => ({ protocol_version: 3, encrypted_sync_version: 3, items: structuredClone([rows[since]!]), next_cursor: since + 1, has_more: since === 0 }))
    expect((await read()).metadata.name).toBe('Updated')
  })
  it.each(['gap', 'duplicate', 'ciphertext', 'foreign_account', 'missing_parent', 'conflict', 'unsupported', 'registry_drift', 'locked', 'mode'] as const)('rejects %s without returning a partial project', async kind => {
    if (kind === 'gap') rows[0]!.event.server_sequence = 2
    if (kind === 'duplicate') { rows.push(structuredClone(rows[0]!)); rows[1]!.event.server_sequence = 2; high = 2 }
    if (kind === 'ciphertext') rows[0]!.object.ciphertext[0]! ^= 1
    if (kind === 'foreign_account') { const event = structuredClone(base); event.header.account_id = crypto.randomUUID(); rows = [await item(event, 1)] }
    if (kind === 'missing_parent') { const event = structuredClone(base); event.header = { ...event.header, revision: 2, generation: 2, operation: 'update', parent_event_ids: [crypto.randomUUID()] }; rows = [await item(event, 1)] }
    if (kind === 'conflict') { const event = structuredClone(base); event.header.event_id = crypto.randomUUID(); rows.push(await item(event, 2)); high = 2 }
    if (kind === 'unsupported') rows[0]!.event.entity_type = 'stage'
    if (kind === 'registry_drift') vi.mocked(cloudProjectsApi.listBootstraps).mockImplementation(async () => ({ current_cursor: high++, projects: [{ project_id: 'project', bootstrap_id: base.header.bootstrap_id, origin_device_id: base.header.device_id, state: 'active', initial_event_count: 0, initial_max_server_sequence: 0 }] }))
    if (kind === 'locked') vi.mocked(encryptedSyncV3Api.pull).mockImplementation(async () => { active = false; return { protocol_version: 3, encrypted_sync_version: 3, items: rows, next_cursor: 1, has_more: false } })
    if (kind === 'mode') vi.mocked(encryptedSyncV2Api.capabilities).mockResolvedValue({ supported_transport_version: 2, writer_transport_version: 2, cutover_epoch: 1 })
    const codes = { gap: 'history_gap_or_duplicate', duplicate: 'history_gap_or_duplicate', ciphertext: 'invalid_encrypted_history', foreign_account: 'invalid_encrypted_history', missing_parent: 'metadata_dependency_or_deleted', conflict: 'metadata_conflict', unsupported: 'unsupported_history_family', cover: 'cover_material_unavailable', registry_drift: 'server_changed', locked: 'session_closed', mode: 'transport_v3_required' }
    await expect(read()).rejects.toMatchObject({ code: codes[kind] })
  })
  it.each([new TypeError('Failed to fetch'), new ApiError(0, 'network_error', 'offline')])('reports a network failure without returning the last read', async failure => {
    await read()
    vi.mocked(encryptedSyncV3Api.pull).mockRejectedValue(failure)
    await expect(read()).rejects.toMatchObject({ code: 'connection_failed' })
  })
  it.each(['compression_reader_required', 'content_note_reader_required', 'sync_device_not_registered'])('preserves the concrete server gate %s without declaring support', async code => {
    vi.mocked(encryptedSyncV3Api.pull).mockRejectedValue(new ApiError(409, code, 'test'))
    await expect(read()).rejects.toMatchObject({ code })
  })
})
