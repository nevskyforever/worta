import {validateBrowserV9State} from './indexedDbV9Migration'
import {validateBrowserV8State} from './contentAuthorityMigration'
import { BrowserContentNoteApply } from './contentNoteApply'
import {BrowserStageOrderApply} from './stageOrderApply'
import { BrowserStageApply } from './stageApply'
import { BrowserReaderCapability } from './readerCapability'
import { BrowserDeviceRegistration } from './deviceRegistration'
import { BrowserProvenPrefix } from './provenPrefix'
import { BrowserNoteIntents } from './noteIntents'
import { validateBrowserV7State } from './noteIntentMigration'
import { BrowserLegacyNoteHistory } from './noteHistory'
import { validateBrowserV6State } from './noteHistoryMigration'
import { BrowserLegacyNoteApply } from './legacyNoteApply'
import { BrowserAccountRegistry } from './accountRegistry'
import { BrowserBootstrapCompletion } from './bootstrapCompletion'
import { BrowserInitialNoteCapture } from './initialNoteCapture'
import { BrowserLocalAuthority } from './localAuthority'
import { BrowserLocalStages } from './localStages'
import { BrowserLocalDocuments } from './localDocuments'
import { validateBrowserV5State } from './localAuthorityMigration'
import { BrowserImportedMetadataApply } from './importedMetadataApply'
import { BrowserSelfMetadataCreate } from './selfMetadataCreate'
import { BrowserProjectBindings, validateBrowserProjectBinding } from './projectBindings'
import type { CloudIdentity, CloudIdentityRepository } from '@/infrastructure/sqlite/cloudIdentityRepository'
import type { CloudAccountBindingRepository, EnsureCloudAccountBindingResult } from '@/infrastructure/sqlite/cloudAccountBindingRepository'
import { BrowserSyncDurability, validateBrowserV2SyncState, validateBrowserV3SyncState, type BrowserSyncScope } from './syncDurability'

export const WEB_DATABASE_NAME = 'worta-web'
export const WEB_DATABASE_VERSION = 12
const WRITER_LOCK = `${WEB_DATABASE_NAME}:writer`
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/
const STORES = ['identities', 'accountBindings']
export type BrowserPersistenceCode = 'stage_order_membership_mismatch'|'stage_order_foreign_stage'|'stage_dependency_missing'|'stage_membership_head_changed'|'stage_dependency_proof_limit'|'stage_tombstone_child_manifest_incomplete'|'stage_order_conflict'| 'unavailable' | 'quota_exceeded' | 'unsupported_version' | 'upgrade_blocked' | 'writer_busy' | 'storage_failed' | 'corrupt_record' | 'account_mismatch' | 'closed' | 'conflicting_record' | 'stale_cursor' | 'missing_history' | 'stale_lineage' | 'invalid_lineage_transition' | 'cover_material_unavailable' | 'metadata_rejected' | 'metadata_conflict' | 'stale_authority' | 'ineligible_lineage' | 'self_echo_unavailable' | 'local_incompatible' | 'invalid_local_source' | 'unsupported_local_source' | 'stale_source' | 'missing_local_source' | 'deleted_local_source' | 'source_limit' | 'bootstrap_incomplete' | 'bootstrap_response_rejected' | 'bootstrap_completion_failed' | 'registry_rejected' | 'registry_conflict' | 'registry_network_failed' | 'note_rejected' | 'note_orphan' | 'note_conflict_unproven' | 'candidate_history_unavailable' | 'note_key_unavailable'

const SCHEMA: ReadonlyArray<readonly [string, string | string[], ReadonlyArray<readonly [string, string | string[], boolean]>]> = [
  ['identities', 'canonical_user_id', [['local_account_id', 'local_account_id', true], ['device_id', 'device_id', true]]],
  ['accountBindings', 'local_account_id', [['canonical_user_id', 'canonical_user_id', true]]],
  ['outbox', ['account_id', 'event_id'], [['ordinal', ['account_id', 'device_id', 'local_ordinal'], true], ['state', ['account_id', 'state', 'local_ordinal'], false]]],
  ['inbox', ['account_id', 'server_sequence'], [['event', ['account_id', 'event_id'], true], ['state', ['account_id', 'state', 'server_sequence'], false]]],
  ['cursors', ['account_id', 'device_id'], []],
  ['receipts', ['account_id', 'event_id'], [['server_sequence', ['account_id', 'server_sequence'], true]]],
  ['projectBindings', ['account_id', 'project_id'], [['bootstrap', ['account_id', 'bootstrap_id'], true]]],
  ['ownerHeads', ['account_id','owner_kind','owner_id'], []],
  ['localProjects', ['account_id','project_id'], []],
  ['localNotes', ['account_id','project_id','note_id'], [['identity', ['account_id','note_id'], true]]],
  ['legacyNoteHistory', ['account_id','project_id','note_id','record_kind','record_id'], [['event', ['account_id','event_id'], true]]],
  ['legacyNoteIntents', ['account_id','event_id'], [['note', ['account_id','project_id','note_id'], true], ['ordinal', ['account_id','device_id','local_ordinal'], true]]],
  ['localStages', ['account_id','project_id','stage_id'], []],
  ['localDocuments', ['account_id','project_id','scope_key'], []],
]

function validateSchema(database: IDBDatabase, transaction: IDBTransaction, version: number): void {
  const schema = version === 1 ? SCHEMA.slice(0, 2) : version === 2 ? SCHEMA.slice(0, 5) : version === 3 ? SCHEMA.slice(0, 6) : version === 4 ? SCHEMA.slice(0, 7) : version === 5 ? SCHEMA.slice(0, 8) : version === 6 ? SCHEMA.slice(0,10) : version === 7 ? SCHEMA.slice(0,11) : version <= 10 ? SCHEMA.slice(0,12) : version === 11 ? SCHEMA.slice(0,13) : SCHEMA
  if (Array.from(database.objectStoreNames).sort().join(',') !== schema.map(([name]) => name).sort().join(',')) throw new BrowserPersistenceError('unsupported_version')
  for (const [name, key, indexes] of schema) {
    const store = transaction.objectStore(name)
    if (JSON.stringify(store.keyPath) !== JSON.stringify(key) || store.autoIncrement || store.indexNames.length !== indexes.length
      || indexes.some(([index, path, unique]) => !store.indexNames.contains(index) || store.index(index).unique !== unique || store.index(index).multiEntry || JSON.stringify(store.index(index).keyPath) !== JSON.stringify(path))) throw new BrowserPersistenceError('corrupt_record')
  }
}

/** Bounded diagnostics only: never retain arbitrary storage error messages/data. */
export class BrowserPersistenceError extends Error {
  readonly name = 'BrowserPersistenceError'
  constructor(readonly code: BrowserPersistenceCode) { super(`web_persistence_${code}`) }
}

export function persistenceError(error: unknown): BrowserPersistenceError {
  if (error instanceof BrowserPersistenceError) return error
  const name = error instanceof DOMException ? error.name : ''
  if (name === 'QuotaExceededError') return new BrowserPersistenceError('quota_exceeded')
  if (name === 'VersionError') return new BrowserPersistenceError('unsupported_version')
  if (['SecurityError', 'NotAllowedError', 'NotSupportedError'].includes(name)) return new BrowserPersistenceError('unavailable')
  return new BrowserPersistenceError('storage_failed')
}

interface IdentityRecord extends CloudIdentity { canonical_user_id: string; record_version: 1 }
interface BindingRecord { local_account_id: string; canonical_user_id: string; record_version: 1 }
function validUser(value: string): void {
  if (typeof value !== 'string' || !UUID.test(value)) throw new BrowserPersistenceError('account_mismatch')
}
function validateRecords(identity: unknown, binding: unknown, user: string): IdentityRecord {
  const i = identity as IdentityRecord | undefined, b = binding as BindingRecord | undefined
  if (!i || !b || Object.keys(i).sort().join(',') !== 'canonical_user_id,device_id,local_account_id,record_version'
    || Object.keys(b).sort().join(',') !== 'canonical_user_id,local_account_id,record_version'
    || i.record_version !== 1 || b.record_version !== 1
    || !UUID.test(i.canonical_user_id) || i.canonical_user_id !== user || b.canonical_user_id !== user
    || i.local_account_id !== b.local_account_id || !UUID.test(i.local_account_id) || !UUID.test(i.device_id)) {
    throw new BrowserPersistenceError('corrupt_record')
  }
  return i
}

/** Infrastructure helper: only synchronous IDB scheduling, never async crypto/HTTP. */
export function runBrowserTransaction<T>(
  database: IDBDatabase,
  stores: string[],
  mode: 'readonly' | 'readwrite',
  schedule: (transaction: IDBTransaction, result: (value: T) => void, fail: (error: unknown) => void) => void,
  track?: (transaction: IDBTransaction, completion: Promise<T>) => void,
): Promise<T> {
  let transaction: IDBTransaction
  try {
    transaction = database.transaction(stores, mode, { durability: 'strict' })
    if (mode === 'readwrite' && transaction.durability !== 'strict') {
      transaction.abort()
      throw new BrowserPersistenceError('unavailable')
    }
  } catch (error) { return Promise.reject(persistenceError(error)) }
  let value: T, hasResult = false, failure: BrowserPersistenceError | undefined
  const completion = new Promise<T>((resolve, reject) => {
    const fail = (error: unknown) => {
      failure ??= persistenceError(error)
      try { transaction.abort() } catch { /* Already completed/aborted; never claim success. */ }
    }
    transaction.oncomplete = () => {
      if (failure) reject(failure)
      else if (!hasResult) reject(new BrowserPersistenceError('storage_failed'))
      else resolve(value)
    }
    transaction.onabort = () => reject(failure ?? persistenceError(transaction.error))
    // Do not preventDefault: a failed request must abort the whole transaction.
    transaction.onerror = event => { failure ??= persistenceError((event.target as IDBRequest).error ?? transaction.error) }
    try {
      const returned: unknown = schedule(transaction, result => { value = result; hasResult = true }, fail)
      if (returned !== undefined) {
        fail(new BrowserPersistenceError('storage_failed'))
        void Promise.resolve(returned).catch(() => undefined)
      }
    } catch (error) { fail(error) }
  })
  track?.(transaction, completion)
  return completion
}

/** Validate the seventh legacy store without inferring Metadata authority. */
export function validateBrowserV4ProjectState(scopes: BrowserSyncScope[], rows: unknown[]): void {
  const projects = new Set<string>(), bootstraps = new Set<string>()
  for (const value of rows) {
    const scope = scopes.find(s => s.accountId === (value as { account_id?: string })?.account_id)
    if (!scope) throw new BrowserPersistenceError('corrupt_record')
    const row = validateBrowserProjectBinding(value, scope)
    if (row.mode === 'upload_existing' && ['captured','completing'].includes(row.phase)) throw new BrowserPersistenceError('corrupt_record') // v4/v5 have no capture producer.
    const project = JSON.stringify([row.account_id, row.project_id]), bootstrap = JSON.stringify([row.account_id, row.bootstrap_id])
    if (projects.has(project) || bootstraps.has(bootstrap)) throw new BrowserPersistenceError('corrupt_record')
    projects.add(project); bootstraps.add(bootstrap)
  }
}

function openDatabase(factory: IDBFactory, signal: AbortSignal): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    let request: IDBOpenDBRequest, failed = false
    const abortUpgrade = () => {
      try { request?.transaction?.abort() } catch { /* Already completed/aborted; opening still fails closed. */ }
    }
    const cancel = () => {
      abortUpgrade()
      fail(new BrowserPersistenceError('closed'))
    }
    const fail = (error: unknown) => {
      failed = true
      signal.removeEventListener('abort', cancel)
      reject(persistenceError(error))
    }
    if (signal.aborted) { cancel(); return }
    signal.addEventListener('abort', cancel, { once: true })
    try { request = factory.open(WEB_DATABASE_NAME, WEB_DATABASE_VERSION) }
    catch (error) { fail(error); return }
    request.onblocked = () => fail(new BrowserPersistenceError('upgrade_blocked'))
    request.onerror = () => fail(request.error)
    request.onupgradeneeded = event => {
      if (failed || ![0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11].includes(event.oldVersion)) {
        abortUpgrade()
        fail(new BrowserPersistenceError('unsupported_version'))
        return
      }
      try {
        const transaction = request.transaction!
        if (event.oldVersion > 0) {
          validateSchema(request.result, transaction, event.oldVersion)
          const names = event.oldVersion === 1 ? STORES : SCHEMA.slice(0, event.oldVersion === 2 ? 5 : event.oldVersion === 3 ? 6 : event.oldVersion === 4 ? 7 : event.oldVersion === 5 ? 8 : event.oldVersion === 6 ? 10 : event.oldVersion === 7 ? 11 : event.oldVersion === 11 ? 13 : 12).map(([name]) => name)
          const requests = names.map(name => transaction.objectStore(name).getAll())
          let received = 0
          const validate = () => {
            if (++received !== requests.length) return
            try {
              const identities = requests[0]!.result as IdentityRecord[], bindings = requests[1]!.result as BindingRecord[]
              if (identities.length !== bindings.length) throw new BrowserPersistenceError('corrupt_record')
              for (const identity of identities) validateRecords(identity, bindings.find(b => b.canonical_user_id === identity.canonical_user_id), identity.canonical_user_id)
              if (event.oldVersion === 2) validateBrowserV2SyncState(identities.map(identity => ({ canonicalUserId: identity.canonical_user_id, accountId: identity.local_account_id, deviceId: identity.device_id })), requests[2]!.result, requests[3]!.result, requests[4]!.result)
              if (event.oldVersion === 3 || event.oldVersion === 4) validateBrowserV3SyncState(identities.map(identity => ({ canonicalUserId: identity.canonical_user_id, accountId: identity.local_account_id, deviceId: identity.device_id })), requests[2]!.result, requests[3]!.result, requests[4]!.result, requests[5]!.result)
              if (event.oldVersion === 9) validateBrowserV9State(identities.map(identity => ({ canonicalUserId: identity.canonical_user_id, accountId: identity.local_account_id, deviceId: identity.device_id })), requests.slice(2).map(r => r.result))
              if (event.oldVersion === 8) validateBrowserV8State(identities.map(identity => ({ canonicalUserId: identity.canonical_user_id, accountId: identity.local_account_id, deviceId: identity.device_id })), requests.slice(2).map(r => r.result))
              if (event.oldVersion === 7) validateBrowserV7State(identities.map(identity => ({ canonicalUserId: identity.canonical_user_id, accountId: identity.local_account_id, deviceId: identity.device_id })), requests.slice(2).map(r => r.result))
              if (event.oldVersion === 6) validateBrowserV6State(identities.map(identity => ({ canonicalUserId: identity.canonical_user_id, accountId: identity.local_account_id, deviceId: identity.device_id })), requests.slice(2).map(r => r.result))
              if (event.oldVersion === 5) validateBrowserV5State(identities.map(identity => ({ canonicalUserId: identity.canonical_user_id, accountId: identity.local_account_id, deviceId: identity.device_id })), requests.slice(2).map(r => r.result))
              if (event.oldVersion === 4) validateBrowserV4ProjectState(identities.map(identity => ({ canonicalUserId: identity.canonical_user_id, accountId: identity.local_account_id, deviceId: identity.device_id })), requests[6]!.result)
            } catch (error) { abortUpgrade(); fail(error) }
          }
          for (const read of requests) read.onsuccess = validate
        }
        for (const [name, keyPath, indexes] of SCHEMA.slice(event.oldVersion === 0 ? 0 : event.oldVersion === 1 ? 2 : event.oldVersion === 2 ? 5 : event.oldVersion === 3 ? 6 : event.oldVersion === 4 ? 7 : event.oldVersion === 5 ? 8 : event.oldVersion === 6 ? 10 : event.oldVersion === 7 ? 11 : event.oldVersion === 11 ? 13 : 12)) {
          const store = request.result.createObjectStore(name, { keyPath })
          for (const [index, path, unique] of indexes) store.createIndex(index, path, { unique })
        }
      } catch (error) { abortUpgrade(); fail(error) }
    }
    request.onsuccess = () => {
      const database = request.result
      if (failed) { database.close(); return }
      try {
        if (database.version !== WEB_DATABASE_VERSION) {
          throw new BrowserPersistenceError('unsupported_version')
        }
        validateSchema(database, database.transaction(SCHEMA.map(([name]) => name)), WEB_DATABASE_VERSION)
        signal.removeEventListener('abort', cancel)
        resolve(database)
      } catch (error) { database.close(); fail(error) }
    }
  })
}

/** Dormant browser identity/binding and encrypted durability session. No full sync composition. */
class BrowserPersistence implements CloudIdentityRepository, CloudAccountBindingRepository {
  readonly sync: BrowserSyncDurability
  readonly projects: BrowserProjectBindings
  readonly stageOrder: BrowserStageOrderApply
  readonly stages: BrowserStageApply
  readonly metadata: BrowserImportedMetadataApply
  readonly selfMetadataCreate: BrowserSelfMetadataCreate
  readonly localSource: BrowserLocalAuthority
  readonly localStages: BrowserLocalStages
  readonly localDocuments: BrowserLocalDocuments
  readonly initialNotes: BrowserInitialNoteCapture
  readonly completion: BrowserBootstrapCompletion
  readonly noteIntents: BrowserNoteIntents
  readonly noteHistory: BrowserLegacyNoteHistory
  readonly contentNotes: BrowserContentNoteApply
  readonly notes: BrowserLegacyNoteApply
  readonly readers: BrowserReaderCapability
  readonly device: BrowserDeviceRegistration
  readonly proven: BrowserProvenPrefix
  readonly registry: BrowserAccountRegistry
  private active = true
  private readonly pending = new Map<IDBTransaction, Promise<unknown>>()
  private closeFlight: Promise<void> | undefined
  private readonly abort = () => { void this.close() }

  constructor(
    private readonly database: IDBDatabase, private readonly user: string,
    private readonly releaseWriter: () => void, private readonly writerFlight: () => Promise<unknown>,
    private readonly signal: AbortSignal, private readonly cleanup: () => void,
  ) {
    this.sync = new BrowserSyncDurability((scope, stores, mode, schedule) => this.syncTransaction(scope, stores, mode, schedule))
    this.projects = new BrowserProjectBindings((scope, stores, mode, schedule) => this.syncTransaction(scope, stores, mode, schedule))
    this.localSource = new BrowserLocalAuthority((scope, stores, mode, schedule) => this.syncTransaction(scope, stores, mode, schedule))
    this.localStages = new BrowserLocalStages((scope, stores, mode, schedule) => this.syncTransaction(scope, stores, mode, schedule))
    this.localDocuments = new BrowserLocalDocuments((scope, stores, mode, schedule) => this.syncTransaction(scope, stores, mode, schedule))
    this.initialNotes = new BrowserInitialNoteCapture((scope, stores, mode, schedule) => this.syncTransaction(scope, stores, mode, schedule), this.localSource, this.projects)
    this.completion = new BrowserBootstrapCompletion((scope, stores, mode, schedule) => this.syncTransaction(scope, stores, mode, schedule), () => this.active)
    this.noteIntents = new BrowserNoteIntents((scope, stores, mode, schedule) => this.syncTransaction(scope, stores, mode, schedule), this.localSource, () => this.active)
    this.noteHistory = new BrowserLegacyNoteHistory((scope, stores, mode, schedule) => this.syncTransaction(scope, stores, mode, schedule), () => this.active)
    this.notes = new BrowserLegacyNoteApply((scope, stores, mode, schedule) => this.syncTransaction(scope, stores, mode, schedule), this.localSource, () => this.active)
    this.contentNotes = new BrowserContentNoteApply(this.notes)
    this.readers = new BrowserReaderCapability((scope, stores, mode, schedule) => this.syncTransaction(scope, stores, mode, schedule), () => this.active)
    this.device = new BrowserDeviceRegistration((scope, stores, mode, schedule) => this.syncTransaction(scope, stores, mode, schedule), () => this.active)
    this.proven = new BrowserProvenPrefix((scope, stores, mode, schedule) => this.syncTransaction(scope, stores, mode, schedule), () => this.active)
    this.registry = new BrowserAccountRegistry((scope, stores, mode, schedule) => this.syncTransaction(scope, stores, mode, schedule), () => this.active)
    this.stageOrder = new BrowserStageOrderApply((scope,stores,mode,schedule)=>this.syncTransaction(scope,stores,mode,schedule),()=>this.active)
    this.stages = new BrowserStageApply((scope, stores, mode, schedule) => this.syncTransaction(scope, stores, mode, schedule), () => this.active)
    this.metadata = new BrowserImportedMetadataApply((scope, stores, mode, schedule) => this.syncTransaction(scope, stores, mode, schedule), this.sync)
    this.selfMetadataCreate = new BrowserSelfMetadataCreate((scope, stores, mode, schedule) => this.syncTransaction(scope, stores, mode, schedule), this.sync)
    database.onversionchange = () => { void this.close() }
    database.onclose = () => { void this.close() }
    signal.addEventListener('abort', this.abort, { once: true })
  }

  private check(user = this.user): void {
    if (!this.active) throw new BrowserPersistenceError('closed')
    if (user !== this.user) throw new BrowserPersistenceError('account_mismatch')
  }
  private transaction<T>(mode: 'readonly' | 'readwrite', schedule: Parameters<typeof runBrowserTransaction<T>>[3], stores = STORES): Promise<T> {
    this.check()
    return runBrowserTransaction(this.database, stores, mode, schedule, (transaction, completion) => {
      this.pending.set(transaction, completion)
      void completion.then(() => this.pending.delete(transaction), () => this.pending.delete(transaction))
    }).then(result => { this.check(); return result }, error => { this.check(); throw error })
  }
  private syncTransaction<T>(scope: BrowserSyncScope, stores: string[], mode: 'readonly' | 'readwrite', schedule: Parameters<typeof runBrowserTransaction<T>>[3]): Promise<T> {
    this.check(scope.canonicalUserId)
    return this.transaction(mode, (transaction, result, fail) => {
      const identity = transaction.objectStore('identities').get(this.user)
      const binding = transaction.objectStore('accountBindings').index('canonical_user_id').get(this.user)
      let received = 0
      const start = () => {
        if (++received !== 2) return
        try {
          this.check(scope.canonicalUserId)
          const value = validateRecords(identity.result, binding.result, this.user)
          if (value.local_account_id !== scope.accountId || value.device_id !== scope.deviceId) throw new BrowserPersistenceError('account_mismatch')
          schedule(transaction, result, fail)
        } catch (error) { fail(error) }
      }
      identity.onsuccess = start
      binding.onsuccess = start
    }, [...STORES, ...stores])
  }
  private identity(create: boolean): Promise<CloudIdentity> {
    return this.transaction<CloudIdentity>(create ? 'readwrite' : 'readonly', (transaction, result, fail) => {
      const identities = transaction.objectStore('identities'), bindings = transaction.objectStore('accountBindings')
      const identityRequest = identities.get(this.user)
      const bindingRequest = bindings.index('canonical_user_id').get(this.user)
      let received = 0
      const finish = () => {
        if (++received !== 2) return
        try {
          this.check()
          if (identityRequest.result === undefined && bindingRequest.result === undefined && create) {
            const identity: IdentityRecord = { canonical_user_id: this.user, local_account_id: crypto.randomUUID(), device_id: crypto.randomUUID(), record_version: 1 }
            identities.add(identity)
            bindings.add({ canonical_user_id: this.user, local_account_id: identity.local_account_id, record_version: 1 } satisfies BindingRecord)
            result({ local_account_id: identity.local_account_id, device_id: identity.device_id })
          } else {
            const identity = validateRecords(identityRequest.result, bindingRequest.result, this.user)
            result({ local_account_id: identity.local_account_id, device_id: identity.device_id })
          }
        } catch (error) { fail(error) }
      }
      identityRequest.onsuccess = finish
      bindingRequest.onsuccess = finish
    })
  }
  async provision(canonicalUserId: string): Promise<CloudIdentity> {
    this.check(canonicalUserId)
    return this.identity(true)
  }
  async read(canonicalUserId: string): Promise<CloudIdentity | null> {
    this.check(canonicalUserId)
    return this.identity(false)
  }
  async ensure(localAccountId: string, canonicalUserId: string): Promise<EnsureCloudAccountBindingResult> {
    this.check(canonicalUserId)
    const identity = await this.read(canonicalUserId)
    if (identity?.local_account_id !== localAccountId) throw new BrowserPersistenceError('account_mismatch')
    return 'validated'
  }
  /** Revoke immediately; abort/drain before releasing writer; never delete data. */
  close(): Promise<void> {
    if (this.closeFlight) return this.closeFlight
    this.active = false
    this.signal.removeEventListener('abort', this.abort)
    this.cleanup()
    const pending = Array.from(this.pending.values())
    for (const transaction of this.pending.keys()) {
      try { transaction.abort() } catch { /* Completion may already be queued. */ }
    }
    this.database.close()
    this.closeFlight = Promise.allSettled(pending).then(async () => {
      this.releaseWriter()
      await this.writerFlight().catch(() => undefined)
    })
    return this.closeFlight
  }
  resetForLogout(): Promise<void> { return this.close() }
}

/** Caller supplies a canonical authenticated user ID; this is not authentication. */
export async function openBrowserPersistence(canonicalUserId: string, signal?: AbortSignal): Promise<BrowserPersistence> {
  validUser(canonicalUserId)
  if (signal?.aborted) throw new BrowserPersistenceError('closed')
  let factory: IDBFactory, locks: LockManager
  try {
    if (typeof window === 'undefined' || !globalThis.isSecureContext || !globalThis.indexedDB || !navigator.locks || !crypto.randomUUID) throw new BrowserPersistenceError('unavailable')
    factory = globalThis.indexedDB
    locks = navigator.locks
  } catch (error) { throw error instanceof BrowserPersistenceError ? error : new BrowserPersistenceError('unavailable') }
  const cancellation = new AbortController()
  const cancel = () => cancellation.abort()
  const cleanup = () => {
    signal?.removeEventListener('abort', cancel)
    window.removeEventListener('pagehide', cancel)
  }
  signal?.addEventListener('abort', cancel, { once: true })
  window.addEventListener('pagehide', cancel, { once: true })
  let resolveReady!: (value: BrowserPersistence) => void, rejectReady!: (error: BrowserPersistenceError) => void
  const ready = new Promise<BrowserPersistence>((resolve, reject) => { resolveReady = resolve; rejectReady = reject })
  let session: BrowserPersistence | undefined
  let flight: Promise<unknown>
  try { flight = locks.request(WRITER_LOCK, { mode: 'exclusive', ifAvailable: true }, async lock => {
    if (cancellation.signal.aborted) throw new BrowserPersistenceError('closed')
    if (!lock) throw new BrowserPersistenceError('writer_busy')
    const database = await openDatabase(factory, cancellation.signal)
    if (cancellation.signal.aborted) { database.close(); throw new BrowserPersistenceError('closed') }
    let release!: () => void
    const held = new Promise<void>(resolve => { release = resolve })
    session = new BrowserPersistence(database, canonicalUserId, release, () => flight, cancellation.signal, cleanup)
    try {
      await session.provision(canonicalUserId)
      resolveReady(session)
      await held
    } catch (error) {
      // On open failure no caller owns the session; revoke before releasing.
      void session.close()
      throw error
    }
  }) } catch (error) { cleanup(); throw persistenceError(error) }
  void flight.catch(error => {
    cleanup()
    rejectReady(persistenceError(error))
    if (session) void session.close()
  })
  return ready
}
