import { cloudProjectsApi } from '@/api/cloudProjects'
import { encryptedSyncV2Api } from '@/api/encryptedSyncV2'
import { encryptedSyncV3Api, isAccountItem, parseV3Pull } from '@/api/encryptedSyncV3'
import { CryptoError } from '@/crypto'
import { ApiError, ApiResponseTooLargeError } from '@/api/client'
import type { NormalUserAuthRuntime } from '@/auth/userAuth'
import type { AuthoritativeKeyContextLease } from '@/auth/keyContext'
import type { ProjectMetadata, ProjectMetadataEvent } from '@/cloud/projectMetadataCodec'
import { metadataImportSnapshot } from '@/cloud/metadataImportSnapshot'
import { canonicalizeSyncTimestamp } from '@/cloud/syncTimestamp'
import { validateAccountRegistry } from './accountRegistry'
import { openBrowserCanonicalObject } from './canonicalReaders'

export class BrowserCloudReadError extends Error {
  constructor(readonly code: string) { super(`browser_cloud_read_${code}`) }
}
export interface BrowserCloudProjectRead {
  readonly kind: 'metadata_only'
  readonly projectId: string
  readonly metadata: ProjectMetadata
  readonly cursor: number
  readonly empty?: true
  readonly ready: false
  readonly declarationAllowed: false
  readonly writerAuthorized: false
}
function fail(code: string): never { throw new BrowserCloudReadError(code) }

/** Explicit read-only Metadata view, not sync apply/replay or capability evidence.
 * Reads the entire bounded account feed from zero. Unsupported families stop the
 * whole request; nothing is skipped, persisted, ACKed or declared. */
export async function readBrowserCloudProject(input: {
  auth: NormalUserAuthRuntime; lease: AuthoritativeKeyContextLease; deviceId: string
  projectId: string; signal: AbortSignal; current: () => boolean
  delivery?: 'recovery'
}): Promise<BrowserCloudProjectRead> {
  const { auth, lease, deviceId, projectId, signal, current } = input
  const context = auth.requireContext()
  const check = () => {
    if (signal.aborted || !current() || !auth.isCurrent(context) || !lease.isCurrent()
      || lease.canonicalUserId !== context.userId || lease.authEpoch !== context.authEpoch) fail('session_closed')
  }
  const request = async <T>(operation: (token: string) => Promise<T>): Promise<T> => {
    check()
    try {
      const response = await auth.authorized(token => { check(); return operation(token) })
      check()
      return response.value
    } catch (error) {
      check()
      if (error instanceof ApiError || error instanceof BrowserCloudReadError) throw error
      if (error instanceof ApiResponseTooLargeError) fail('history_limit')
      if (error instanceof SyntaxError || error instanceof TypeError && ['invalid_encrypted_sync_v3', 'Invalid encrypted sync v2 payload.'].includes(error.message)) fail('invalid_encrypted_history')
      fail('connection_failed')
    }
  }
  try {
    check()
    const mode = await request(token => encryptedSyncV2Api.capabilities(token, signal))
    if (mode.writer_transport_version !== 3) fail('transport_v3_required')
    const readRegistry = async () => {
      const value = await request(token => cloudProjectsApi.listBootstraps(token, signal))
      try { return validateAccountRegistry(value) } catch { return fail('invalid_server_registry') }
    }
    const registry = await readRegistry()
    const selected = registry.projects.find(project => project.project_id === projectId)
    if (!selected) fail('project_not_authorized')
    if (selected.state !== 'active') fail('project_bootstrap_not_active')
    // Bound memory and full-account history work before fetching ciphertext.
    if (registry.current_cursor > 200) fail('history_limit')
    const histories = new Map<string, ProjectMetadataEvent[]>()
    const ids = new Set<string>()
    let cursor = 0, bytes = 0, selectedSequence = 0
    await lease.use(async amk => {
      let more = true
      while (more) {
        const page = input.delivery === 'recovery'
          ? parseV3Pull(JSON.parse(await request(token => encryptedSyncV3Api.recoveryRaw(token, cursor, 100,
            registry.current_cursor, mode.cutover_epoch, signal))), cursor, 100)
          : await request(token => encryptedSyncV3Api.pull(token, deviceId, cursor, 100, signal))
        try {
          for (const item of page.items) {
            if (item.event.server_sequence !== cursor + 1 || ids.has(item.event.event_id)) fail('history_gap_or_duplicate')
            if (item.event.server_sequence > registry.current_cursor) fail('server_changed')
            bytes += item.object.ciphertext.length
            if (bytes > 32 * 1024 * 1024) fail('history_limit')
            // Fail the account read before selecting any project's projection.
            if (isAccountItem(item)) fail('unsupported_history_family')
            const e = item.event
            if (e.entity_type !== 'project_metadata') fail('unsupported_history_family')
            const binding = registry.projects.find(project => project.project_id === e.project_id)
            if (!binding || binding.state !== 'active') fail('project_bootstrap_not_active')
            const event = await openBrowserCanonicalObject(amk, { accountId: context.userId,
              projectId: e.project_id, entityId: e.entity_id, entityType: e.entity_type, eventId: e.event_id }, item.object) as ProjectMetadataEvent
            check()
            const h = event.header
            if (h.bootstrap_id !== binding.bootstrap_id || h.device_id !== e.device_id || h.revision !== e.revision
              || h.updated_at !== canonicalizeSyncTimestamp(e.updated_at)
              || event.deleted_at !== (e.deleted_at === null ? null : canonicalizeSyncTimestamp(e.deleted_at))
              || e.operation !== (h.operation === 'delete' ? 'delete' : 'upsert')) fail('descriptor_mismatch')
            // Cover metadata (reference) is part of authenticated project metadata and is readable here.
            // Encrypted cover blob material/authentication remains a separate concern.
            const history = histories.get(e.project_id) ?? []
            history.push(event); histories.set(e.project_id, history)
            if (e.project_id === projectId) selectedSequence = e.server_sequence
            ids.add(e.event_id); cursor = e.server_sequence
          }
          if (page.next_cursor !== cursor || page.has_more && cursor >= registry.current_cursor) fail('server_changed')
          more = page.has_more
        } finally {
          for (const item of page.items) { item.object.nonce.fill(0); item.object.ciphertext.fill(0) }
        }
      }
    })
    check()
    if (cursor !== registry.current_cursor) fail('history_incomplete')
    const snapshots = new Map<string, ReturnType<typeof metadataImportSnapshot>>()
    for (const [id, history] of histories) {
      const bootstrap = registry.projects.find(project => project.project_id === id)!
      try { snapshots.set(id, metadataImportSnapshot(history, bootstrap.bootstrap_id!, id)) }
      catch { fail('metadata_dependency_or_deleted') }
      if (!snapshots.get(id)?.head) fail('metadata_conflict')
    }
    const snapshot = snapshots.get(projectId)
    if (!snapshot?.metadata) fail('metadata_missing')
    const after = await readRegistry()
    const afterMode = await request(token => encryptedSyncV2Api.capabilities(token, signal))
    if (JSON.stringify(after) !== JSON.stringify(registry) || afterMode.writer_transport_version !== mode.writer_transport_version
      || afterMode.cutover_epoch !== mode.cutover_epoch) fail('server_changed')
    check()
    // Only the complete, stable account feed can prove that this bootstrap
    // consists of a single Metadata Create and contains no project content.
    const history = histories.get(projectId)!
    const header = history[0]?.header
    const empty = input.delivery === 'recovery' && history.length === 1 && header?.operation === 'create'
      && header.device_id === selected.origin_device_id && header.revision === 1 && header.generation === 1
      && header.parent_event_ids.length === 0 && selected.initial_event_count === 1
      && selected.initial_max_server_sequence === selectedSequence
    return { kind: 'metadata_only', projectId, metadata: structuredClone(snapshot.metadata), cursor,
      ...(empty ? { empty: true as const } : {}),
      ready: false, declarationAllowed: false, writerAuthorized: false }
  } catch (error) {
    check()
    if (error instanceof BrowserCloudReadError) throw error
    if (error instanceof ApiError) {
      if (error.status === 0 && error.code === 'network_error') fail('connection_failed')
      const gates = ['sync_device_not_registered', 'sync_transport_mode_incompatible', 'compression_reader_required',
        'content_note_reader_required', 'map_reader_required', 'document_reader_required', 'progress_reader_required']
      fail(gates.includes(error.code) ? error.code : error.status === 401 || error.status === 403 ? 'authorization_required' : 'server_rejected')
    }
    fail(error instanceof TypeError || error instanceof CryptoError ? 'invalid_encrypted_history' : 'connection_failed')
  }
}
