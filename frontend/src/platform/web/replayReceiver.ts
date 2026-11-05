import { cloudProjectsApi } from '@/api/cloudProjects'
import { encryptedSyncV2Api } from '@/api/encryptedSyncV2'
import { encryptedSyncV3Api, type V3Descriptor } from '@/api/encryptedSyncV3'
import { ApiError, ApiResponseTooLargeError } from '@/api/client'
import type { NormalUserAuthRuntime } from '@/auth/userAuth'
import type { AuthoritativeKeyContextLease } from '@/auth/keyContext'
import { BrowserPersistenceError } from './persistence'
import { snapshotBrowserSyncScope, type BrowserSyncScope } from './syncDurability'
import { BrowserReplayError, REPLAY_LIMITS, prepareReplayPage, replayAnchor, replaySame } from './replayRecords'
import { openBrowserReplayStorage, type ReplaySnapshot, type ReplayStorage } from './replayStorage'
import { openBrowserCanonicalObject } from './canonicalReaders'
import { decodeBase64Url } from '@/api/base64url'
import { canonicalizeSyncTimestamp } from '@/cloud/syncTimestamp'
import type { ContentNoteEvent } from '@/cloud/contentNoteCodec'

export interface BrowserAccountReceiveResult {
  readonly kind: 'received_ciphertext'; readonly cursor: number; readonly events: number; readonly generation: string
  readonly ready: false; readonly declarationAllowed: false; readonly writerAuthorized: false
  readonly projectOpened: false; readonly ownerProofs: 0
}

interface ReceiveInput {
  auth: NormalUserAuthRuntime; lease: AuthoritativeKeyContextLease; scope: BrowserSyncScope
  signal: AbortSignal; current: () => boolean; openStorage?: typeof openBrowserReplayStorage
  delivery?: 'ordinary' | 'recovery'
}

/** Keeps the isolated writer lease through receipt reconciliation and its bounded owner consumer. */
export async function withReceivedBrowserAccountHistory<T>(input: ReceiveInput, consume: (storage: ReplayStorage, snapshot: ReplaySnapshot) => Promise<T>): Promise<T> {
  const { auth, lease } = input, scope = snapshotBrowserSyncScope(input.scope), context = auth.requireContext()
  const controller = new AbortController(), signal = AbortSignal.any([input.signal, controller.signal])
  const unsubscribe = auth.onInvalidated(() => controller.abort())
  const fail = (code: string): never => { throw new BrowserReplayError(code) }
  const check = () => {
    if (signal.aborted || !input.current() || !auth.isCurrent(context) || !lease.isCurrent()
      || lease.localAccountId !== scope.accountId || lease.canonicalUserId !== scope.canonicalUserId
      || context.userId !== scope.canonicalUserId || lease.authEpoch !== context.authEpoch) fail('session_closed')
  }
  const request = async <T>(operation: (token: string) => Promise<T>): Promise<T> => {
    check()
    const response = await auth.authorized(token => { check(); return operation(token) })
    check(); return response.value
  }
  let storage: Awaited<ReturnType<typeof openBrowserReplayStorage>> | undefined
  try {
    check()
    const mode = await request(token => encryptedSyncV2Api.capabilities(token, signal))
    const registry = await request(token => cloudProjectsApi.listBootstraps(token, signal))
    const anchor = replayAnchor(registry, mode)
    storage = await (input.openStorage ?? openBrowserReplayStorage)(scope, signal, () => {
      try { check(); return true } catch { return false }
    })
    check()
    // When an existing recovery cache has a different server head, compare its
    // complete retained wire before attempting any transition. This is evidence
    // only: it does not authorize changing the import binding or the anchor.
    if (input.delivery === 'recovery') {
      const previous = await storage.read(); check()
      const prior = previous.manifest
      if (prior && !replaySame(prior.anchor, anchor)) {
        if (prior.phase !== 'received' || anchor.registry.current_cursor < prior.receive_cursor
          || !replaySame(prior.anchor.transport, anchor.transport)
          || !replaySame(prior.anchor.registry.projects, anchor.registry.projects)) fail('server_changed')
        let offset = 0
        while (offset < prior.receive_cursor) {
          const limit = Math.min(REPLAY_LIMITS.page, prior.receive_cursor - offset)
          const raw = await request(token => encryptedSyncV3Api.recoveryRaw(token, offset, limit,
            anchor.registry.current_cursor, anchor.transport.cutover_epoch, signal))
          const { page } = prepareReplayPage(raw, offset, scope, anchor)
          if (!page.items.length || page.next_cursor > prior.receive_cursor) fail('history_incomplete')
          for (const item of page.items) {
            const retained = previous.rows[item.event.server_sequence! - 1]
            if (!retained || !replaySame(JSON.parse(retained.wire_item), item)) fail('retained_wire_mismatch')
          }
          offset = page.next_cursor
        }
        const afterRegistry = await request(token => cloudProjectsApi.listBootstraps(token, signal))
        const afterMode = await request(token => encryptedSyncV2Api.capabilities(token, signal))
        if (!replaySame(replayAnchor(afterRegistry, afterMode), anchor) || !replaySame(await storage.read(), previous)) fail('server_changed')
        // Limit this transition to one bounded page of authenticated peer Content
        // updates. Owner application still produces the authoritative proofs.
        if (anchor.registry.current_cursor - prior.receive_cursor > REPLAY_LIMITS.page) fail('history_limit')
        const raw = await request(token => encryptedSyncV3Api.recoveryRaw(token, prior.receive_cursor, REPLAY_LIMITS.page,
          anchor.registry.current_cursor, anchor.transport.cutover_epoch, signal))
        const { page } = prepareReplayPage(raw, prior.receive_cursor, scope, anchor)
        if (page.has_more || !page.items.length) fail('history_incomplete')
        await lease.use(async amk => {
          const heads = new Map<string, { event_id: string; revision: number }>()
          for (const row of previous.rows) {
            const event = JSON.parse(row.wire_item).event
            if (event.entity_type === 'note' && row.state === 'content_note_proven') heads.set(JSON.stringify([event.project_id, event.entity_id]), event)
          }
          for (const item of page.items) {
            const event = item.event as V3Descriptor & { scope?: string }
            if ('scope' in event || event.entity_type !== 'note' || event.operation !== 'event' || event.device_id === scope.deviceId) fail('unsupported_history_family')
            const key = JSON.stringify([event.project_id, event.entity_id]), head = heads.get(key)
            const nonce = decodeBase64Url((item.object as { nonce: string }).nonce), ciphertext = decodeBase64Url((item.object as { ciphertext: string }).ciphertext)
            try {
              const object = item.object as { crypto_version: 1; aad_version: 1 }
              const decoded = await openBrowserCanonicalObject(amk, { accountId: scope.canonicalUserId, projectId: event.project_id,
                entityId: event.entity_id, entityType: 'note', eventId: event.event_id }, { ...object, nonce, ciphertext }) as ContentNoteEvent
              const header = decoded.event.header
              if (!head || decoded.device_id !== event.device_id || decoded.event.version !== 1 || decoded.event.mutation !== 'update'
                || header.parent_event_id !== head.event_id || header.revision !== head.revision + 1 || header.revision !== event.revision
                || canonicalizeSyncTimestamp(header.updated_at) !== canonicalizeSyncTimestamp(event.updated_at)) fail('invalid_history')
              heads.set(key, event)
            } finally { nonce.fill(0); ciphertext.fill(0) }
          }
        }); check()
        const server = async () => {
          const freshRegistry = await request(token => cloudProjectsApi.listBootstraps(token, signal))
          const freshMode = await request(token => encryptedSyncV2Api.capabilities(token, signal))
          if (!replaySame(replayAnchor(freshRegistry, freshMode), anchor)) fail('server_changed')
          const fresh = await request(token => encryptedSyncV3Api.recoveryRaw(token, prior.receive_cursor, REPLAY_LIMITS.page,
            anchor.registry.current_cursor, anchor.transport.cutover_epoch, signal))
          if (!replaySame(JSON.parse(fresh), JSON.parse(raw))) fail('server_changed')
          const tail = await request(token => encryptedSyncV3Api.recoveryRaw(token, anchor.registry.current_cursor, 1,
            anchor.registry.current_cursor, anchor.transport.cutover_epoch, signal))
          const checked = prepareReplayPage(tail, anchor.registry.current_cursor, scope, anchor)
          if (checked.page.items.length || checked.page.has_more) fail('server_changed')
        }
        await storage.stageRecoveryExtension(previous, anchor, raw, server); check()
      }
    }
    let snapshot = await storage.start(anchor)
    const retained = snapshot.manifest!.receive_cursor
    const ids = new Set<string>()
    let since = 0, more = true
    while (more) {
      // Fresh zero-origin reconciliation of ALL retained wire. This is readonly
      // server comparison, not J's older-page replay or an applied certificate.
      const limit = Math.min(REPLAY_LIMITS.page, since < retained ? retained - since : REPLAY_LIMITS.page)
      const raw = await request(token => input.delivery === 'recovery'
        ? encryptedSyncV3Api.recoveryRaw(token, since, limit, anchor.registry.current_cursor, anchor.transport.cutover_epoch, signal)
        : encryptedSyncV3Api.pullRaw(token, scope.deviceId, since, limit, signal))
      const { page } = prepareReplayPage(raw, since, scope, anchor)
      if (page.items.length > limit) fail('invalid_page')
      for (const item of page.items) {
        if (ids.has(item.event.event_id)) fail('history_duplicate')
        ids.add(item.event.event_id)
      }
      if (since < retained) {
        if (page.next_cursor > retained) fail('history_incomplete')
        for (const item of page.items) {
          const row = snapshot.rows[item.event.server_sequence! - 1]
          if (!row || !replaySame(JSON.parse(row.wire_item), item)) fail('retained_wire_mismatch')
        }
      } else if (page.items.length) {
        check(); snapshot = await storage.commitPage(snapshot, since, raw); check()
      }
      since = page.next_cursor; more = page.has_more
    }
    if (since !== anchor.registry.current_cursor || snapshot.manifest!.receive_cursor !== since) fail('history_incomplete')
    const afterRegistry = await request(token => cloudProjectsApi.listBootstraps(token, signal))
    const afterMode = await request(token => encryptedSyncV2Api.capabilities(token, signal))
    if (!replaySame(replayAnchor(afterRegistry, afterMode), anchor)) fail('server_changed')
    check(); snapshot = await storage.finish(snapshot); check()
    const result = await consume(storage, snapshot); check(); return result
  } catch (error) {
    check()
    if (error instanceof BrowserReplayError || error instanceof BrowserPersistenceError) throw error
    if (error instanceof ApiResponseTooLargeError) fail('history_limit')
    if (error instanceof ApiError) {
      if (error.status === 0 && error.code === 'network_error') fail('connection_failed')
      const gates = ['sync_device_not_registered', 'sync_transport_mode_incompatible', 'compression_reader_required',
        'content_note_reader_required', 'map_reader_required', 'document_reader_required', 'progress_reader_required']
      fail(gates.includes(error.code) ? error.code : error.status === 401 || error.status === 403 ? 'authorization_required' : 'server_rejected')
    }
    return fail(error instanceof SyntaxError || error instanceof TypeError ? 'invalid_history' : 'connection_failed')
  } finally { unsubscribe(); controller.abort(); await storage?.close() }
}

/** Receipt-only result never labels existing owner proofs as zero. */
export function receiveBrowserAccountHistory(input: ReceiveInput): Promise<BrowserAccountReceiveResult> {
  return withReceivedBrowserAccountHistory(input, async (_storage, snapshot) => {
    if (snapshot.manifest!.applied_cursor) throw new BrowserReplayError('metadata_owner_state_present')
    return { kind: 'received_ciphertext', cursor: snapshot.manifest!.receive_cursor, events: snapshot.rows.length, generation: snapshot.manifest!.generation,
      ready: false, declarationAllowed: false, writerAuthorized: false, projectOpened: false, ownerProofs: 0 }
  })
}
