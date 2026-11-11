import type { AuthoritativeAccountBinding } from '@/auth/accountBinding'
import type { RuntimeKeyContext } from '@/auth/keyContext'
import type { NormalUserAuthRuntime } from '@/auth/userAuth'
import { cloudProjectsApi } from '@/api/cloudProjects'
import { encodeV3MetadataPush, encryptedSyncV3Api, type V3Descriptor, type V3MetadataPushItem } from '@/api/encryptedSyncV3'
import { createCloudProjectMetadata } from './cloudProjectMetadataCreate'
import { BrowserPersistenceError } from './persistence'
import { encodeBase64Url, decodeBase64Url } from '@/api/base64url'
import { openProjectMetadataEvent } from '@/cloud/projectMetadataCodec'
import type { BrowserProjectBindings } from './projectBindings'
import type { BrowserSelfMetadataCreate } from './selfMetadataCreate'
import { snapshotBrowserSyncScope, type BrowserSyncDurability } from './syncDurability'

export interface CloudProjectCreateScope {
  readonly accountId: string
  readonly canonicalUserId: string
  readonly deviceId: string
}

export interface CloudProjectCreateDependencies {
  readonly auth: NormalUserAuthRuntime
  readonly bindings: AuthoritativeAccountBinding
  readonly keys: RuntimeKeyContext
  readonly storage: {
    readonly sync: BrowserSyncDurability
    readonly projects: BrowserProjectBindings
    readonly selfMetadataCreate: BrowserSelfMetadataCreate
    readonly device: {
      bind(auth: NormalUserAuthRuntime, bindings: AuthoritativeAccountBinding): {
        registerOnce(input: CloudProjectCreateScope): Promise<{ deviceId: string; serverAckCursor: number; ready: false }>
      }
    }
  }
}

interface PendingCloudProject {
  readonly projectId: string
  readonly bootstrapId: string
  readonly eventId: string
  readonly updatedAt: string
  readonly nonce: string
  readonly ciphertext: string
}

const pendingKey = (accountId: string) => `worta:pending-cloud-project-create:${accountId}`

function readPending(accountId: string): PendingCloudProject | null {
  try {
    const raw = window.localStorage.getItem(pendingKey(accountId))
    if (raw === null) return null
    const value: unknown = JSON.parse(raw)
    if (!value || typeof value !== 'object') throw new TypeError('pending_metadata_create')
    const p = value as PendingCloudProject
    if (typeof p.projectId !== 'string' || typeof p.bootstrapId !== 'string' || typeof p.eventId !== 'string'
      || typeof p.updatedAt !== 'string' || typeof p.nonce !== 'string' || typeof p.ciphertext !== 'string') throw new TypeError('pending_metadata_create')
    return p
  } catch {
    throw new BrowserPersistenceError('corrupt_record')
  }
}

function writePending(accountId: string, pending: PendingCloudProject): void {
  window.localStorage.setItem(pendingKey(accountId), JSON.stringify(pending))
}

const inFlight = new Set<string>()

/** Internal writer slice. Success means Metadata applied; bootstrap is still pending. */
export async function stageCloudProjectMetadata(
  input: CloudProjectCreateScope,
  name: string,
  dependencies: CloudProjectCreateDependencies,
  signal?: AbortSignal,
): Promise<{ projectId: string; serverSequence: number }> {
  const scope = snapshotBrowserSyncScope(input)
  if (inFlight.has(scope.accountId)) throw new BrowserPersistenceError('writer_busy')
  inFlight.add(scope.accountId)
  try {
    return await stage(scope, name, dependencies, signal)
  } finally {
    inFlight.delete(scope.accountId)
  }
}

async function stage(scope: CloudProjectCreateScope, name: string, dependencies: CloudProjectCreateDependencies, signal?: AbortSignal) {
  const { auth, bindings, keys, storage } = dependencies
  const { context } = await bindings.ensureForCurrentUser(scope.accountId)
  const lease = keys.leaseForAccount(scope.accountId)
  if (!lease || lease.localAccountId !== scope.accountId || context.userId !== scope.canonicalUserId
    || lease.canonicalUserId !== context.userId || lease.authEpoch !== context.authEpoch) {
    throw new BrowserPersistenceError('account_mismatch')
  }
  const check = () => {
    if (signal?.aborted || !lease.isCurrent() || !auth.isCurrent(context)) throw new BrowserPersistenceError('closed')
  }
  const request = async <T>(send: (token: string) => Promise<T>): Promise<T> => {
    const response = await auth.authorized(token => { check(); return send(token) })
    check()
    return response.value
  }

  check()
  let pending = readPending(scope.accountId)
  if (!pending) {
    const created = await lease.use(amk => createCloudProjectMetadata({
      accountId: scope.canonicalUserId, canonicalUserId: scope.canonicalUserId, deviceId: scope.deviceId,
    }, name, amk))
    try {
      const { project_id, bootstrap_id, event_id, updated_at } = created.event.header
      pending = { projectId: project_id, bootstrapId: bootstrap_id, eventId: event_id, updatedAt: updated_at,
        nonce: encodeBase64Url(created.object.nonce), ciphertext: encodeBase64Url(created.object.ciphertext) }
      check()
      // Retain the operation identity before any network request or IDB commit.
      writePending(scope.accountId, pending)
    } finally { created.object.nonce.fill(0); created.object.ciphertext.fill(0) }
  }
  const { projectId, bootstrapId, eventId, updatedAt } = pending
  const item: V3MetadataPushItem = {
    event: { event_id: eventId, project_id: projectId, entity_id: projectId, entity_type: 'project_metadata',
      operation: 'upsert', revision: 1, updated_at: updatedAt, deleted_at: null },
    object: { crypto_version: 1, aad_version: 1, nonce: decodeBase64Url(pending.nonce), ciphertext: decodeBase64Url(pending.ciphertext) },
  }
  try {
    const event = await lease.use(amk => openProjectMetadataEvent(amk, {
      account_id: scope.canonicalUserId, project_id: projectId, entity_id: projectId, event_id: eventId,
    }, item.object))
    check()
    const h = event.header
    if (event.version !== 1 || h.operation !== 'create' || h.bootstrap_id !== bootstrapId || h.device_id !== scope.deviceId
      || h.updated_at !== updatedAt || event.metadata?.name !== name.trim()) throw new BrowserPersistenceError('metadata_rejected')

    const [queued] = await storage.sync.enqueue(scope, [encodeV3MetadataPush(scope.deviceId, [item])])
    if (!queued) throw new BrowserPersistenceError('corrupt_record')
    check()
    const lineage = await storage.projects.prepareMetadataCreate(scope, projectId, bootstrapId)
    check()
    const device = await storage.device.bind(auth, bindings).registerOnce(scope)
    check()
    if (device.deviceId !== scope.deviceId) throw new BrowserPersistenceError('account_mismatch')
    if (lineage.phase === 'prepared') {
      const registration = await request(token => cloudProjectsApi.registerBootstrap(token, projectId, {
        bootstrap_id: bootstrapId, device_id: scope.deviceId,
      }, signal))
      const project = registration.project
      if (project?.project_id !== projectId || project.bootstrap_id !== bootstrapId || project.origin_device_id !== scope.deviceId
        || project.state !== 'initializing' || project.initial_event_count !== null || project.initial_max_server_sequence !== null) {
        throw new BrowserPersistenceError('registry_rejected')
      }
      await storage.projects.confirmRegistration(scope, lineage, {
        remote_state: 'initializing', remote_high_water: registration.current_cursor,
      })
      check()
    }

    let receipt = await storage.sync.readUploadReceipt(scope, eventId)
    if (!receipt) {
      const push = await request(token => encryptedSyncV3Api.pushMetadata(token, scope.deviceId, [item], signal))
      await storage.sync.commitUploadAcceptance(scope, queued.item, push)
      check()
      receipt = await storage.sync.readUploadReceipt(scope, eventId)
    }
    if (!receipt) throw new BrowserPersistenceError('missing_history')
    const sequence = receipt.server_sequence
    let cursor = (await storage.sync.readCheckpoint(scope)).pull_cursor
    for (let page = 0; cursor < sequence && page < 16; page++) {
      const since = cursor
      const raw = await request(token => encryptedSyncV3Api.pullRaw(token, scope.deviceId, since, 100, signal))
      const committed = await storage.sync.commitInboundPage(scope, since, raw)
      check()
      cursor = committed.committed_cursor
      if (!committed.has_more) break
    }
    if (cursor < sequence) throw new BrowserPersistenceError('missing_history')
    const inbox = await storage.sync.readInboxEvent(scope, sequence)
    check()
    if (!inbox || inbox.event_id !== eventId) throw new BrowserPersistenceError('missing_history')
    const reference = JSON.parse(inbox.wire_item).event as V3Descriptor
    const plan = await storage.selfMetadataCreate.prepare(scope, reference, auth, bindings, keys, signal)
    check()
    await storage.selfMetadataCreate.commit(scope, plan)
    check()
    // Keep the retry identity until a future, proof-backed bootstrap completion.
    return { projectId, serverSequence: sequence }
  } finally { item.object.nonce.fill(0); item.object.ciphertext.fill(0) }
}
