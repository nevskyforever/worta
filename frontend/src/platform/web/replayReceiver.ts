import { validateBrowserRetainedHistory, type BrowserReceivedInboxRecord } from './syncDurability'
import type { ProjectMetadataEvent } from '@/cloud/projectMetadataCodec'
import { validateBrowserMetadataHead } from './metadataApplyRecords'
import { prepareMapEvent } from './mapPrepare'
import { authenticateReplayMaps, mapPreparationInventory } from './mapApply'
import { readReplayStages } from './replayStageView'
import { validateStageHead } from './stageRecords'
import { validateStageOrderHead } from './stageOrderRecords'
import type { StructuralEvent } from '@/cloud/stageCodec'
import { cloudProjectsApi } from '@/api/cloudProjects'
import { encryptedSyncV2Api } from '@/api/encryptedSyncV2'
import { encryptedSyncV3Api, type V3Descriptor } from '@/api/encryptedSyncV3'
import { ApiError, ApiResponseTooLargeError } from '@/api/client'
import type { NormalUserAuthRuntime } from '@/auth/userAuth'
import type { AuthoritativeKeyContextLease } from '@/auth/keyContext'
import { BrowserPersistenceError } from './persistence'
import { snapshotBrowserSyncScope, type BrowserSyncScope } from './syncDurability'
import { BrowserReplayError, REPLAY_LIMITS, prepareReplayPage, recoveryRegistryExtends, replayAnchor, replaySame } from './replayRecords'
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
  // This is receipt validation, never an owner outcome. Dependencies/proofs are
  // checked by the existing owners only after all pages have been received.
  const authenticateCanonicalWires = (items: ReturnType<typeof prepareReplayPage>['page']['items']) => lease.use(async amk => {
    for (const item of items) if (['document', 'map', 'progress', 'project_game', 'account_game'].includes((item.event as V3Descriptor).entity_type)) {
      const event = item.event as V3Descriptor
      const object = item.object as { crypto_version: 1; aad_version: 1; nonce: string; ciphertext: string }
      const nonce = decodeBase64Url(object.nonce), ciphertext = decodeBase64Url(object.ciphertext)
      try {
        await openBrowserCanonicalObject(amk, { accountId: scope.canonicalUserId, projectId: String(event.entity_type) === 'account_game' ? null : event.project_id,
          entityId: event.entity_id, entityType: event.entity_type, eventId: event.event_id }, { ...object, nonce, ciphertext })
      } finally { nonce.fill(0); ciphertext.fill(0) }
      check()
    }
  })
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
        if (anchor.registry.current_cursor < prior.anchor.registry.current_cursor
          || !recoveryRegistryExtends(prior.anchor, anchor)) fail('server_changed')
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
        // Admit bounded peer Content continuations or one verified StageOrder update.
        // Owner application still produces the authoritative proofs.
        const raw = await request(token => encryptedSyncV3Api.recoveryRaw(token, prior.receive_cursor, REPLAY_LIMITS.page,
          anchor.registry.current_cursor, anchor.transport.cutover_epoch, signal))
        const { page } = prepareReplayPage(raw, prior.receive_cursor, scope, anchor)
        if (!page.items.length && (prior.phase !== 'received' || page.next_cursor !== anchor.registry.current_cursor
          || anchor.registry.current_cursor !== prior.anchor.registry.current_cursor)) fail('history_incomplete')
        const added = anchor.registry.projects.filter(project => project.initial_event_count === 1
          && !prior.anchor.registry.projects.some(value => value.project_id === project.project_id))
        const newMetadata = new Map<string, ProjectMetadataEvent['header']>()
        if (added.length) await lease.use(async amk => {
          for (const project of added) {
            const item = page.items.find(value => value.event.server_sequence === project.initial_max_server_sequence)
            if (!item) return fail('invalid_history')
            const wire = item
            const event = wire.event as V3Descriptor & { scope?: string }
            if (event.entity_type !== 'project_metadata' || 'scope' in event || event.project_id !== project.project_id
              || event.device_id !== project.origin_device_id || event.operation !== 'upsert'
              || event.revision !== 1 || event.deleted_at !== null) fail('invalid_history')
            const object = wire.object as { crypto_version: 1; aad_version: 1; nonce: string; ciphertext: string }
            const nonce = decodeBase64Url(object.nonce), ciphertext = decodeBase64Url(object.ciphertext)
            try {
              const decoded = await openBrowserCanonicalObject(amk, { accountId: scope.canonicalUserId,
                projectId: project.project_id, entityId: project.project_id, entityType: 'project_metadata', eventId: event.event_id },
              { ...object, nonce, ciphertext }) as ProjectMetadataEvent
              const h = decoded.header
              if (h.bootstrap_id !== project.bootstrap_id || h.device_id !== event.device_id || h.operation !== 'create'
                || h.revision !== 1 || h.generation !== 1 || h.updated_at !== canonicalizeSyncTimestamp(event.updated_at)
                || h.parent_event_ids.length || !decoded.metadata) fail('invalid_history')
              newMetadata.set(event.project_id, h)
            } finally { nonce.fill(0); ciphertext.fill(0) }
          }
        })
        if (prior.phase === 'receiving' || prior.applied_cursor !== prior.receive_cursor) {
          // Re-pin an interrupted receive/apply only after comparing ALL retained
          // wire above. Keep the prior proofs/import and replay the unproven rows;
          // a pending parent must not be mistaken for an applied dependency.
          await authenticateCanonicalWires(page.items)
        } else await lease.use(async amk => {
          const heads = new Map<string, { event_id: string; revision: number }>()
          for (const row of previous.rows) {
            const event = JSON.parse(row.wire_item).event
            if (event.entity_type === 'note' && row.state === 'content_note_proven') heads.set(JSON.stringify([event.project_id, event.entity_id]), event)
          }
          const pairedOrders = new Set<string>()
          const stageExtensions = new Map<string, { order: StructuralEvent['header']; members: Record<string, string[]> }>()
          for (const [index, item] of page.items.entries()) {
            const event = item.event as V3Descriptor & { scope?: string }
            if (pairedOrders.has(event.event_id)) continue
            if ('scope' in event && String(event.entity_type) !== 'account_game' || event.device_id === scope.deviceId && !['progress', 'document', 'account_game'].includes(event.entity_type)) fail('unsupported_history_family')
            if (event.device_id === scope.deviceId) {
              const echo: BrowserReceivedInboxRecord = { record_version: 1, account_id: scope.accountId,
                device_id: scope.deviceId, event_id: event.event_id, server_sequence: event.server_sequence,
                protocol_version: 3, state: 'received', wire_item: JSON.stringify(item) }
              const find = (value: unknown) => (value as {event_id: string}).event_id === event.event_id
              const evidence = validateBrowserRetainedHistory(scope, event, previous.author?.out.find(find), previous.author?.receipts.find(find), echo)
              if (!evidence.outbox || !evidence.receipt) fail('invalid_history')
            }
            if (event.entity_type === 'project_metadata' && ['upsert', 'delete'].includes(event.operation)) {
              if (added.some(project => project.project_id === event.project_id
                && project.initial_max_server_sequence === event.server_sequence)) continue
              const rawHead = previous.owners.find(owner => owner.owner_kind === 'project_metadata' && owner.owner_id === event.project_id)
              const head = rawHead && validateBrowserMetadataHead(rawHead, scope)
              if (head && head.tip_event_ids.length !== 1) fail('invalid_history')
              const extension = newMetadata.get(event.project_id)
              const tip = extension ?? head?.history.find(proof => proof.event_id === head.tip_event_ids[0])
              const remote = anchor.registry.projects.find(project => project.project_id === event.project_id)
              const object = item.object as { crypto_version: 1; aad_version: 1; nonce: string; ciphertext: string }
              const nonce = decodeBase64Url(object.nonce), ciphertext = decodeBase64Url(object.ciphertext)
              try {
                const decoded = await openBrowserCanonicalObject(amk, {
                  accountId: scope.canonicalUserId, projectId: event.project_id, entityId: event.entity_id,
                  entityType: 'project_metadata', eventId: event.event_id,
                }, { ...object, nonce, ciphertext }) as ProjectMetadataEvent
                const h = decoded.header
                if (h.device_id !== event.device_id || h.revision !== event.revision
                  || h.updated_at !== canonicalizeSyncTimestamp(event.updated_at)
                  || decoded.deleted_at !== (event.deleted_at === null ? null : canonicalizeSyncTimestamp(event.deleted_at))) fail('invalid_history')
                if (tip) {
                  if (h.bootstrap_id !== (extension?.bootstrap_id ?? head?.bootstrap_id)
                    || h.operation !== (event.operation === 'delete' ? 'delete' : 'update')
                    || !replaySame(h.parent_event_ids, [tip.event_id])
                    || h.revision !== tip.revision + 1 || h.generation <= tip.generation) fail('invalid_history')
                } else if (remote?.state !== 'active' || remote.initial_event_count !== 0
                  || remote.origin_device_id !== h.device_id || remote.bootstrap_id !== h.bootstrap_id
                  || event.operation !== 'upsert' || h.operation !== 'create' || h.revision !== 1
                  || h.generation !== 1 || h.parent_event_ids.length || !decoded.metadata) {
                  fail('invalid_history')
                }
                newMetadata.set(event.project_id, h)
              } finally { nonce.fill(0); ciphertext.fill(0) }
              continue // Receipt only. The Metadata owner still authenticates/commits its outcome.
            }
            if (['project_game','account_game'].includes(event.entity_type) && ['event','upsert'].includes(event.operation)) {
              await authenticateCanonicalWires([item]);continue
            }
            if (event.entity_type === 'progress' && event.operation === 'event') {
              await authenticateCanonicalWires([item])
              continue
            }
            if (event.entity_type === 'map' && event.operation === 'event') {
              const current = () => !signal.aborted && input.current() && auth.isCurrent(context) && lease.isCurrent()
              await authenticateReplayMaps(scope, previous, amk, current)
              await prepareMapEvent(scope, { server_sequence: event.server_sequence!, wire_item: JSON.stringify(item) }, mapPreparationInventory(previous), amk, current)
              continue
            }
            if (event.entity_type === 'document' && event.operation === 'event') {
              await authenticateCanonicalWires([item])
              continue
            }
            const following = page.items[index + 1]
            if (event.entity_type === 'stage' && event.operation === 'upsert' && event.revision === 1 && following) {
              const next = following.event as V3Descriptor
              if (next.entity_type !== 'stage_order' || next.project_id !== event.project_id || next.operation !== 'upsert'
                || next.device_id !== event.device_id || next.device_id === scope.deviceId) fail('invalid_history')
              const current = () => !signal.aborted && input.current() && auth.isCurrent(context) && lease.isCurrent()
              const extension = stageExtensions.get(event.project_id)
              if (!extension) await readReplayStages(scope, previous, event.project_id, amk, current)
              const rawOrder = previous.owners.find(owner => owner.owner_kind === 'stage_order' && owner.project_id === event.project_id)
              const orderHead = rawOrder && validateStageOrderHead(rawOrder, scope)
              const tip = extension?.order ?? orderHead?.history.find(proof => proof.event_id === orderHead.tip_event_ids[0])
              const rawMetadata = previous.owners.find(owner => owner.owner_kind === 'project_metadata' && owner.owner_id === event.project_id)
              const metadata = rawMetadata && validateBrowserMetadataHead(rawMetadata, scope)
              const metadataExtension = newMetadata.get(event.project_id)
              const bootstrap = metadata?.bootstrap_id ?? metadataExtension?.bootstrap_id
              if (!bootstrap || (orderHead && (!tip || orderHead.tip_event_ids.length !== 1))) fail('invalid_history')
              const open = async (value: typeof item, type: 'stage' | 'stage_order') => {
                const nonce = decodeBase64Url((value.object as { nonce: string }).nonce), ciphertext = decodeBase64Url((value.object as { ciphertext: string }).ciphertext)
                try { return await openBrowserCanonicalObject(amk, { accountId: scope.canonicalUserId, projectId: event.project_id, entityId: (value.event as V3Descriptor).entity_id, entityType: type, eventId: value.event.event_id }, { ...(value.object as { crypto_version: 1; aad_version: 1 }), nonce, ciphertext }) as StructuralEvent }
                finally { nonce.fill(0); ciphertext.fill(0) }
              }
              const created = await open(item, 'stage'), ordered = await open(following, 'stage_order'), h = created.header, o = ordered.header
              const expected = extension?.members ?? Object.fromEntries(previous.owners.filter(owner => owner.owner_kind === 'stage' && owner.project_id === event.project_id).map(owner => {
                const head = validateStageHead(owner, scope)
                return [head.stage_id, head.tip_event_ids]
              }))
              if (Object.hasOwn(expected, event.entity_id)) fail('invalid_history')
              expected[event.entity_id] = [event.event_id]
              const metadataId = h.metadata_event_id
              if ((!metadata?.history.some(value => value.event_id === metadataId && value.operation !== 'delete')
                  && (metadataExtension?.event_id !== metadataId || metadataExtension.operation === 'delete'))
                || created.version !== 1 || h.operation !== 'create' || h.device_id !== event.device_id
                || h.bootstrap_id !== bootstrap || h.revision !== 1 || !created.stage
                || ordered.version !== 1 || o.operation !== (tip ? 'update' : 'create') || o.device_id !== next.device_id
                || o.bootstrap_id !== bootstrap || o.metadata_event_id !== metadataId || o.revision !== (tip?.revision ?? 0) + 1
                || o.revision !== next.revision || o.generation !== (tip?.generation ?? 0) + 1 || !replaySame(o.parent_event_ids, tip ? [tip.event_id] : [])
                || h.updated_at !== canonicalizeSyncTimestamp(event.updated_at) || o.updated_at !== canonicalizeSyncTimestamp(next.updated_at)
                || !replaySame(ordered.stage_heads, expected)) fail('invalid_history')
              stageExtensions.set(event.project_id, { order: o, members: expected })
              pairedOrders.add(next.event_id)
              continue
            }
            if (event.entity_type === 'stage' && event.operation === 'upsert' && page.items.length === 1) {
              await readReplayStages(scope, previous, event.project_id, amk, () => !signal.aborted && input.current() && auth.isCurrent(context) && lease.isCurrent())
              const rawHead = previous.owners.find(owner => owner.owner_kind === 'stage' && owner.project_id === event.project_id && owner.stage_id === event.entity_id)
              if (!rawHead) fail('unsupported_history_family')
              const head = validateStageHead(rawHead, scope), priorProof = head.history.find(proof => proof.event_id === head.tip_event_ids[0])
              const nonce = decodeBase64Url((item.object as { nonce: string }).nonce), ciphertext = decodeBase64Url((item.object as { ciphertext: string }).ciphertext)
              try {
                const decoded = await openBrowserCanonicalObject(amk, { accountId: scope.canonicalUserId, projectId: event.project_id,
                  entityId: event.entity_id, entityType: 'stage', eventId: event.event_id }, { ...(item.object as { crypto_version: 1; aad_version: 1 }), nonce, ciphertext }) as StructuralEvent
                const h = decoded.header
                if (!priorProof || h.account_id !== scope.canonicalUserId || h.project_id !== event.project_id || h.device_id !== event.device_id
                  || h.entity_id !== event.entity_id || h.entity_type !== 'stage' || h.operation !== 'update' || h.event_id !== event.event_id
                  || h.bootstrap_id !== head.bootstrap_id || h.metadata_event_id !== priorProof.metadata_event_id || h.revision !== event.revision
                  || h.revision !== priorProof.revision + 1 || h.generation !== priorProof.generation + 1 || !replaySame(h.parent_event_ids, head.tip_event_ids)
                  || canonicalizeSyncTimestamp(h.updated_at) !== canonicalizeSyncTimestamp(event.updated_at) || !decoded.stage) fail('invalid_history')
              } finally { nonce.fill(0); ciphertext.fill(0) }
              continue
            }
            if (event.entity_type === 'stage_order' && event.operation === 'upsert' && page.items.length === 1) {
              await readReplayStages(scope, previous, event.project_id, amk, () => !signal.aborted && input.current() && auth.isCurrent(context) && lease.isCurrent())
              const rawHead = previous.owners.find(owner => owner.owner_kind === 'stage_order' && owner.project_id === event.project_id)
              const head = rawHead && validateStageOrderHead(rawHead, scope)
              const priorProof = head?.history.find(proof => proof.event_id === head.tip_event_ids[0])
              const rawMetadata = previous.owners.find(owner => owner.owner_kind === 'project_metadata' && owner.owner_id === event.project_id)
              const metadata = rawMetadata && validateBrowserMetadataHead(rawMetadata, scope)
              if (!metadata || metadata.tip_event_ids.length !== 1 || (head && !priorProof)) fail('invalid_history')
              const nonce = decodeBase64Url((item.object as { nonce: string }).nonce), ciphertext = decodeBase64Url((item.object as { ciphertext: string }).ciphertext)
              try {
                const decoded = await openBrowserCanonicalObject(amk, { accountId: scope.canonicalUserId, projectId: event.project_id,
                  entityId: event.entity_id, entityType: 'stage_order', eventId: event.event_id }, { ...(item.object as { crypto_version: 1; aad_version: 1 }), nonce, ciphertext }) as StructuralEvent
                const h = decoded.header, stageHeads = previous.owners.filter(owner => owner.owner_kind === 'stage' && owner.project_id === event.project_id).map(owner => validateStageHead(owner, scope))
                if (h.device_id !== event.device_id || h.operation !== (priorProof ? 'update' : 'create')
                  || h.bootstrap_id !== metadata!.bootstrap_id || !metadata!.history.some(proof => proof.event_id === h.metadata_event_id && proof.operation !== 'delete')
                  || h.revision !== event.revision || h.revision !== (priorProof?.revision ?? 0) + 1
                  || h.generation !== (priorProof?.generation ?? 0) + 1 || !replaySame(h.parent_event_ids, head?.tip_event_ids ?? [])
                  || (!priorProof && stageHeads.length !== 0)
                  || h.updated_at !== canonicalizeSyncTimestamp(event.updated_at)
                  || !decoded.stage_ids || !decoded.stage_heads || decoded.stage_ids.length !== stageHeads.length
                  || stageHeads.some(stage => !decoded.stage_ids!.includes(stage.stage_id) || !replaySame(decoded.stage_heads![stage.stage_id], stage.tip_event_ids))) fail('invalid_history')
              } finally { nonce.fill(0); ciphertext.fill(0) }
              continue
            }
            if (event.entity_type !== 'note' || event.operation !== 'event') fail('unsupported_history_family')
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
        // Reject damaged Document/Map ciphertext before retaining a new page. Owners
        // still authenticate dependencies and issue proofs only after reception.
        if (input.delivery === 'recovery') await authenticateCanonicalWires(page.items)
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
