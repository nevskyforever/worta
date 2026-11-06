import type { BrowserProjectGame } from './gameApply'
import type { ProjectGameEvent } from '@/cloud/gameCodec'
import type { ProjectGameState } from '@/cloud/gameProjectTransition'
import { presentProgress, type BrowserProgressView, type ProgressSettings } from './progressView'
import type { BrowserProjectMap } from './mapApply'
import type { PortableDocument } from '@/cloud/documentCodec'
import { readReplayStages, type ReplayStageView } from './replayStageView'
import { replaySame } from './replayRecords'
import type { ReplayMetadataResult, ReplayStructuralResult, ReplayInitialNoteResult, ReplayOrderedContentResult } from './replayMetadata'
import type { ReplaySourceSeedResult } from './replaySourceSeed'
import { accountCryptoApi, type AccountCryptoTransport } from '@/api/accountCrypto'
import { cloudProjectsApi, type CloudProjectsResponse } from '@/api/cloudProjects'
import { legalDocumentsApi, type LegalDocument, type LegalDocumentConfirmation,
  type LegalDocumentStatus } from '@/api/legalDocuments'
import { AuthoritativeAccountBinding } from '@/auth/accountBinding'
import { KeyNotProvisionedError, RuntimeKeyContext, type AuthoritativeKeyContextLease } from '@/auth/keyContext'
import { NormalUserAuthRuntime, StaleAuthContextError, type AuthContextSnapshot } from '@/auth/userAuth'
import { currentPlatform } from '@/platform/runtime'
import type { SupportedLanguage } from '@/types/api'
import { BrowserPersistenceError, openBrowserPersistence } from './persistence'
import type { BrowserLocalAuthorityRuntime } from './localAuthority'
import type { NativeLocalProject, NativeLegacyNote } from './localAuthorityRecords'
import type { BrowserLocalStagesRuntime } from './localStages'
import type { BrowserLocalDocumentsRuntime } from './localDocuments'
import type { BrowserSyncScope } from './syncDurability'
import { readBrowserCloudProject, BrowserCloudReadError, type BrowserCloudProjectRead } from './cloudRead'
import { withReceivedBrowserAccountHistory, receiveBrowserAccountHistory, type BrowserAccountReceiveResult } from './replayReceiver'

type BrowserStorage = Awaited<ReturnType<typeof openBrowserPersistence>>
type BrowserIdentity = { readonly local_account_id: string; readonly device_id: string }

export interface BrowserLocalNoteRepository {
  /** Account/device scope derived from the authenticated, unlocked Web session. */
  readonly scope: BrowserSyncScope
  /** Existing C21 encrypted local source. It has no cloud transport or Note writer. */
  readonly source: Pick<BrowserLocalAuthorityRuntime, 'listProjects' | 'readSnapshot' | 'withDecryptedSnapshot' | 'assertSnapshot' | 'initializeProject' | 'createProject' | 'createNote' | 'updateNote' | 'tombstoneNote' | 'replaceProject' | 'deleteProject'>
  readonly stages: Pick<BrowserLocalStagesRuntime, 'list' | 'create' | 'rename' | 'remove' | 'reorder'>
  readonly documents: Pick<BrowserLocalDocumentsRuntime, 'read' | 'open' | 'save'>
}

export interface BrowserAccountSessionOptions {
  readonly auth?: NormalUserAuthRuntime
  readonly cryptoTransport?: AccountCryptoTransport
  readonly openStorage?: typeof openBrowserPersistence
  readonly legalTransport?: typeof legalDocumentsApi
}

/** Test-Web account/key composition. It owns no Note transport or reader authority. */
export class BrowserCloudAccountSession {
  private readonly auth: NormalUserAuthRuntime
  private readonly cryptoTransport: AccountCryptoTransport
  private readonly openStorage: typeof openBrowserPersistence
  private readonly legalTransport: typeof legalDocumentsApi
  private readonly unsubscribe: () => void
  private storage: BrowserStorage | null = null
  private bindings: AuthoritativeAccountBinding | null = null
  private keys: RuntimeKeyContext | null = null
  private identity: BrowserIdentity | null = null
  private controller: AbortController | null = null
  private opening: Promise<void> | null = null
  private generation = 0
  private everUnlocked = false
  private disposed = false
  private legalStatus: LegalDocumentStatus | null = null
  private readonly revocations = new EventTarget()
  private readonly onPageHide = () => { void this.logout().catch(() => undefined) }

  constructor(options: BrowserAccountSessionOptions = {}) {
    this.auth = options.auth ?? new NormalUserAuthRuntime()
    this.cryptoTransport = options.cryptoTransport ?? accountCryptoApi
    this.openStorage = options.openStorage ?? openBrowserPersistence
    this.legalTransport = options.legalTransport ?? legalDocumentsApi
    this.unsubscribe = this.auth.onInvalidated(() => this.revoke(true))
    if (typeof window !== 'undefined') window.addEventListener('pagehide', this.onPageHide)
  }

  get status(): 'logged_out' | 'legal_required' | 'authenticated' | 'unlocked' | 'locked' {
    if (this.auth.state !== 'authenticated') return 'logged_out'
    if (this.legalStatus?.missing_required_document_ids.length) return 'legal_required'
    if (this.identity && this.keys?.leaseForAccount(this.identity.local_account_id)?.isCurrent()) return 'unlocked'
    return this.everUnlocked ? 'locked' : 'authenticated'
  }

  get currentIdentity(): BrowserIdentity | null {
    return this.auth.state === 'authenticated' && this.identity ? { ...this.identity } : null
  }

  get username(): string | null {
    return this.auth.state === 'authenticated' ? this.auth.requireContext().username : null
  }

  get legalDocuments(): readonly LegalDocument[] {
    return this.status === 'legal_required' ? [...(this.legalStatus?.documents ?? [])] : []
  }

  onRevoked(listener: () => void): () => void {
    this.revocations.addEventListener('revoked', listener)
    return () => this.revocations.removeEventListener('revoked', listener)
  }

  /** Fetches server-owned IDs only; local developer projects are never a fallback. */
  async listCloudProjects(signal?: AbortSignal): Promise<CloudProjectsResponse> {
    this.assertWeb()
    if (this.status === 'logged_out' || this.status === 'legal_required') throw new StaleAuthContextError()
    const { value, context } = await this.auth.authorized(token => cloudProjectsApi.list(token, signal))
    if (!this.auth.isCurrent(context)) throw new StaleAuthContextError()
    if (!value || !Array.isArray(value.cloud_project_ids) || !Number.isSafeInteger(value.cloud_project_count)
      || !Number.isSafeInteger(value.max_cloud_projects) || value.cloud_project_count !== value.cloud_project_ids.length
      || value.cloud_project_count < 0 || value.max_cloud_projects < value.cloud_project_count
      || value.cloud_project_ids.some(id => typeof id !== 'string' || !id.trim() || new TextEncoder().encode(id).length > 512)
      || new Set(value.cloud_project_ids).size !== value.cloud_project_ids.length) {
      throw new Error('cloud_project_catalog_response_invalid')
    }
    return { cloud_project_ids: [...value.cloud_project_ids], cloud_project_count: value.cloud_project_count,
      max_cloud_projects: value.max_cloud_projects }
  }

  /** Server read only. The result belongs to this active key/auth session. */
  async readCloudProject(projectId: string, signal: AbortSignal): Promise<BrowserCloudProjectRead> {
    this.assertWeb()
    const identity = this.identity, context = this.auth.requireContext(), generation = this.generation
    const lease = identity && this.keys?.leaseForAccount(identity.local_account_id)
    if (this.status !== 'unlocked' || !identity || !lease || !this.controller) throw new BrowserCloudReadError('key_locked')
    return readBrowserCloudProject({ auth: this.auth, lease, deviceId: identity.device_id, projectId,
      signal: AbortSignal.any([signal, this.controller.signal]),
      current: () => !this.disposed && this.generation === generation && this.auth.isCurrent(context) })
  }

  /** Explicit ciphertext receipt only; no owner apply, project opening or background work. */
  async receiveCloudAccountHistory(signal: AbortSignal): Promise<BrowserAccountReceiveResult> {
    this.assertWeb()
    const identity = this.identity, context = this.auth.requireContext(), generation = this.generation
    const lease = identity && this.keys?.leaseForAccount(identity.local_account_id)
    if (this.status !== 'unlocked' || !identity || !lease || !this.controller) throw new BrowserCloudReadError('key_locked')
    return receiveBrowserAccountHistory({ auth: this.auth, lease,
      scope: { accountId: identity.local_account_id, deviceId: identity.device_id, canonicalUserId: context.userId },
      signal: AbortSignal.any([signal, this.controller.signal]),
      current: () => !this.disposed && this.generation === generation && this.auth.isCurrent(context) })
  }

  /** Online, bounded read-only recovery. No writer capability or ACK is granted. */
  async readRecoveryProject(projectId: string, signal: AbortSignal): Promise<{ project: NativeLocalProject; notes: NativeLegacyNote[]; stages: ReplayStageView[]; documents: PortableDocument[]; maps: BrowserProjectMap[]; progress: BrowserProgressView[]; games: {event:ProjectGameEvent;state:ProjectGameState;history:ProjectGameEvent[]}[] }> {
    this.assertWeb()
    const identity = this.identity, context = this.auth.requireContext(), generation = this.generation
    const keys = this.keys, bindings = this.bindings, lease = identity && keys?.leaseForAccount(identity.local_account_id)
    if (this.status !== 'unlocked' || !identity || !lease || !keys || !bindings || !this.controller) throw new BrowserCloudReadError('key_locked')
    const scope = { accountId: identity.local_account_id, deviceId: identity.device_id, canonicalUserId: context.userId }
    return withReceivedBrowserAccountHistory({ auth: this.auth, lease, scope, delivery: 'recovery',
      signal: AbortSignal.any([signal, this.controller.signal]),
      current: () => !this.disposed && this.generation === generation && this.auth.isCurrent(context),
    }, async storage => {
      await storage.importRecoveryProject(projectId, this.auth, bindings, keys, 'recovery')
      for (let i = 0; i <= 200; i++) {
        const result = await storage.recoverV10(this.auth, bindings, keys, 'recovery')
        if (result.blocker && result.blocker.kind !== 'budget_exhausted') {
          const blocked = (await storage.read()).rows.find(row => row.server_sequence === result.blocker!.sequence)
          if (blocked && JSON.parse(blocked.wire_item).event.entity_type === 'document') throw new BrowserCloudReadError('document_recovery_unavailable')
          throw new BrowserCloudReadError('unsupported_history')
        }
        if (!result.more) break
        if (i === 200) throw new BrowserCloudReadError('history_limit')
      }
      const snapshot = await storage.read()
      if (!snapshot.manifest || snapshot.manifest.applied_cursor !== snapshot.manifest.receive_cursor)
        throw new BrowserCloudReadError('unsupported_history')
      const local = storage.contentAuthorOwner(this.auth, bindings, keys).local
      const source = await local.readSnapshot(scope, projectId)
      const current = () => !this.disposed && this.generation === generation && this.auth.isCurrent(context) && lease.isCurrent()
      const stages = await lease.use(amk => readReplayStages(scope, snapshot, projectId, amk, current))
      const progressChains = (await storage.authenticateProgress(this.auth, bindings, keys)).filter(value => value.project_id === projectId)
      const games = (await storage.authenticateGames(this.auth,bindings,keys)).filter(value => value.event.header.scope === 'project' && value.event.header.project_id === projectId) as (BrowserProjectGame & {event:ProjectGameEvent;state:ProjectGameState;history:ProjectGameEvent[]})[]
      const maps = (await storage.authenticateMaps(this.auth, bindings, keys)).filter(map => map.project_id === projectId)
      const documents = (await storage.authenticateDocuments(this.auth, bindings, keys)).filter(document => document.project_id === projectId)
      const result = await local.withDecryptedSnapshot(scope, source.token, async value => ({
        progress: progressChains.filter(chain => chain.stage_id !== null || !stages.length || stages.every(stage => progressChains.some(progress => progress.stage_id === stage.id))).map(chain => {
          const settings = chain.stage_id === null ? value.project : stages.find(stage => stage.id === chain.stage_id)?.settings
          if (!settings) throw new BrowserCloudReadError('unsupported_history')
          // Native Progress projection uses Stage totals for a staged project.
          // Do not invent totals for Stages whose Progress has not been imported.
          const day = new Date().toLocaleDateString('en-CA')
          const total = chain.stage_id === null && stages.length ? stages.reduce((sum, stage) => {
            const progress = progressChains.find(progress => progress.stage_id === stage.id)!
            return sum + presentProgress(progress, stage.settings!, day).total
          }, 0) : undefined
          return presentProgress(chain, settings as unknown as ProgressSettings, day, total)
        }),
        project: structuredClone(value.project), notes: value.notes.filter(note => note.state === 'live').map(note => structuredClone(note.note)), stages, documents, maps, games,
      }))
      if (games.some(value => value.event.header.stage_id !== null && !stages.some(stage=>stage.id===value.event.header.stage_id))) throw new BrowserCloudReadError('unsupported_history')
      if (maps.some(map => map.stage_id !== null && !stages.some(stage => stage.id === map.stage_id))) throw new BrowserCloudReadError('unsupported_history')
      if (result.notes.some(note => note.stage_id !== null && !stages.some(stage => stage.id === note.stage_id))
        || result.documents.some(document => document.stage_id !== null && !stages.some(stage => stage.id === document.stage_id))) throw new BrowserCloudReadError('unsupported_history')
      if (!current() || !replaySame(snapshot, await storage.read())) throw new BrowserCloudReadError('unsupported_history')
      return result
    })
  }

  /** Bounded peer Metadata only. Fresh server reconciliation is mandatory on every call. */
  async applyCloudAccountMetadata(signal: AbortSignal): Promise<ReplayMetadataResult> {
    this.assertWeb()
    const identity = this.identity, context = this.auth.requireContext(), generation = this.generation
    const keys = this.keys, bindings = this.bindings, lease = identity && keys?.leaseForAccount(identity.local_account_id)
    if (this.status !== 'unlocked' || !identity || !lease || !keys || !bindings || !this.controller) throw new BrowserCloudReadError('key_locked')
    return withReceivedBrowserAccountHistory({ auth: this.auth, lease,
      scope: { accountId: identity.local_account_id, deviceId: identity.device_id, canonicalUserId: context.userId },
      signal: AbortSignal.any([signal, this.controller.signal]),
      current: () => !this.disposed && this.generation === generation && this.auth.isCurrent(context)
    }, storage => storage.applyMetadata(this.auth, bindings, keys))
  }

  /** Bounded account-wide Metadata/Stage/StageOrder composition. Fresh server reconciliation is mandatory on every call. */
  async applyCloudAccountStructure(signal: AbortSignal): Promise<ReplayStructuralResult> {
    this.assertWeb()
    const identity = this.identity, context = this.auth.requireContext(), generation = this.generation
    const keys = this.keys, bindings = this.bindings, lease = identity && keys?.leaseForAccount(identity.local_account_id)
    if (this.status !== 'unlocked' || !identity || !lease || !keys || !bindings || !this.controller) throw new BrowserCloudReadError('key_locked')
    return withReceivedBrowserAccountHistory({ auth: this.auth, lease,
      scope: { accountId: identity.local_account_id, deviceId: identity.device_id, canonicalUserId: context.userId },
      signal: AbortSignal.any([signal, this.controller.signal]),
      current: () => !this.disposed && this.generation === generation && this.auth.isCurrent(context)
    }, storage => storage.applyStructure(this.auth, bindings, keys))
  }

  /** Initial server Legacy Note cohort plus existing structural owners; still no project opening. */
  async applyCloudInitialNotes(signal: AbortSignal): Promise<ReplayInitialNoteResult> {
    this.assertWeb()
    const identity = this.identity, context = this.auth.requireContext(), generation = this.generation
    const keys = this.keys, bindings = this.bindings, lease = identity && keys?.leaseForAccount(identity.local_account_id)
    if (this.status !== 'unlocked' || !identity || !lease || !keys || !bindings || !this.controller) throw new BrowserCloudReadError('key_locked')
    return withReceivedBrowserAccountHistory({ auth: this.auth, lease,
      scope: { accountId: identity.local_account_id, deviceId: identity.device_id, canonicalUserId: context.userId },
      signal: AbortSignal.any([signal, this.controller.signal]),
      current: () => !this.disposed && this.generation === generation && this.auth.isCurrent(context)
    }, storage => storage.applyInitialNotes(this.auth, bindings, keys))
  }

  /** One bounded terminal peer Content update through the ordinary protected receiver. */
  async applyCloudOrderedContentUpdate(signal: AbortSignal): Promise<ReplayOrderedContentResult> {
    this.assertWeb()
    const identity = this.identity, context = this.auth.requireContext(), generation = this.generation
    const keys = this.keys, bindings = this.bindings, lease = identity && keys?.leaseForAccount(identity.local_account_id)
    if (this.status !== 'unlocked' || !identity || !lease || !keys || !bindings || !this.controller) throw new BrowserCloudReadError('key_locked')
    return withReceivedBrowserAccountHistory({ auth: this.auth, lease,
      scope: { accountId: identity.local_account_id, deviceId: identity.device_id, canonicalUserId: context.userId },
      signal: AbortSignal.any([signal, this.controller.signal]),
      current: () => !this.disposed && this.generation === generation && this.auth.isCurrent(context)
    }, storage => storage.applyOrderedContentUpdate(this.auth, bindings, keys))
  }

  /** Private cache-only Note source preparation. It never applies Notes or opens a project. */
  async seedCloudNoteSource(projectId: string, signal: AbortSignal): Promise<ReplaySourceSeedResult> {
    this.assertWeb()
    const identity = this.identity, context = this.auth.requireContext(), generation = this.generation
    const keys = this.keys, bindings = this.bindings, lease = identity && keys?.leaseForAccount(identity.local_account_id)
    if (this.status !== 'unlocked' || !identity || !lease || !keys || !bindings || !this.controller) throw new BrowserCloudReadError('key_locked')
    return withReceivedBrowserAccountHistory({ auth: this.auth, lease,
      scope: { accountId: identity.local_account_id, deviceId: identity.device_id, canonicalUserId: context.userId },
      signal: AbortSignal.any([signal, this.controller.signal]),
      current: () => !this.disposed && this.generation === generation && this.auth.isCurrent(context)
    }, storage => storage.seedNoteSource(projectId, this.auth, bindings, keys))
  }

  /** Explicit user action: identity registration only, never reader readiness. */
  async registerCloudReadDevice(): Promise<void> {
    this.assertWeb()
    const context = this.auth.requireContext(), identity = this.identity
    if (this.status !== 'unlocked' || !identity || !this.storage || !this.bindings) throw new BrowserCloudReadError('key_locked')
    await this.storage.device.bind(this.auth, this.bindings).registerOnce({ canonicalUserId: context.userId,
      accountId: identity.local_account_id, deviceId: identity.device_id })
  }

  private assertWeb(): void {
    if (this.disposed || currentPlatform() !== 'web' || !import.meta.env.DEV) {
      throw new BrowserPersistenceError('unavailable')
    }
  }

  private async revoke(clearIdentity: boolean): Promise<void> {
    this.generation += 1
    this.controller?.abort()
    this.controller = null
    const keys = this.keys, storage = this.storage
    this.keys = null
    this.bindings = null
    this.storage = null
    if (clearIdentity) { this.identity = null; this.everUnlocked = false; this.legalStatus = null }
    this.revocations.dispatchEvent(new Event('revoked'))
    await Promise.all([keys?.dispose(), storage?.close()])
  }

  private async openFor(context: AuthContextSnapshot): Promise<void> {
    if (this.storage) return
    if (this.opening) {
      await this.opening
      if (!this.auth.isCurrent(context)) throw new StaleAuthContextError()
      return
    }
    const opening = this.openFreshFor(context)
    this.opening = opening
    try { await opening } finally { if (this.opening === opening) this.opening = null }
  }

  private async openFreshFor(context: AuthContextSnapshot): Promise<void> {
    const generation = this.generation
    const controller = new AbortController()
    this.controller = controller
    let storage: BrowserStorage | null = null
    try {
      storage = await this.openStorage(context.userId, controller.signal)
      const identity = await storage.read(context.userId)
      if (!identity || !this.auth.isCurrent(context) || this.generation !== generation || controller.signal.aborted) {
        throw new StaleAuthContextError()
      }
      if (this.identity && (this.identity.local_account_id !== identity.local_account_id || this.identity.device_id !== identity.device_id)) {
        throw new BrowserPersistenceError('account_mismatch')
      }
      const bindings = new AuthoritativeAccountBinding(this.auth, storage)
      this.storage = storage
      this.bindings = bindings
      this.identity = { local_account_id: identity.local_account_id, device_id: identity.device_id }
      this.keys = new RuntimeKeyContext(this.auth, bindings, this.cryptoTransport)
    } catch (error) {
      await storage?.close()
      if (this.controller === controller) this.controller = null
      throw error
    }
  }

  private async finishLogin(context: AuthContextSnapshot): Promise<BrowserIdentity> {
    const record = await this.auth.authorized(token => this.cryptoTransport.get(token))
    if (!record.value.provisioned) throw new KeyNotProvisionedError()
    await this.openFor(context)
    if (!this.identity || !this.auth.isCurrent(context)) throw new StaleAuthContextError()
    return { ...this.identity }
  }

  async login(username: string, password: string, language: SupportedLanguage = 'ru'): Promise<BrowserIdentity | null> {
    this.assertWeb()
    const context = await this.auth.login(username, password)
    try {
      const legal = await this.auth.authorized(token => this.legalTransport.status(token, language))
      this.legalStatus = legal.value
      if (legal.value.missing_required_document_ids.length) return null
      return await this.finishLogin(context)
    } catch (error) {
      if (this.auth.isCurrent(context)) await this.auth.logout().catch(() => undefined)
      throw error
    }
  }

  async acceptLegalDocuments(language: SupportedLanguage,
    documents: LegalDocumentConfirmation[]): Promise<BrowserIdentity> {
    this.assertWeb()
    const context = this.auth.requireContext()
    let result
    try {
      result = await this.auth.authorized(token => this.legalTransport.accept(token, language, documents))
    } catch (error) {
      if (error instanceof Error && 'code' in error && error.code === 'legal_document_version_changed') {
        const refreshed = await this.auth.authorized(token => this.legalTransport.status(token, language))
        this.legalStatus = refreshed.value
      }
      throw error
    }
    this.legalStatus = result.value
    if (result.value.missing_required_document_ids.length) throw new Error('Legal acceptance remains incomplete.')
    return await this.finishLogin(context)
  }

  async unlock(passphrase: string): Promise<AuthoritativeKeyContextLease> {
    this.assertWeb()
    const context = this.auth.requireContext()
    await this.openFor(context)
    const keys = this.keys, identity = this.identity, generation = this.generation
    if (!keys || !identity) throw new BrowserPersistenceError('closed')
    const lease = await keys.unlockWithPassphrase(identity.local_account_id, passphrase)
    if (!this.auth.isCurrent(context) || generation !== this.generation || !lease.isCurrent()) {
      await keys.lock()
      throw new StaleAuthContextError()
    }
    this.everUnlocked = true
    return lease
  }

  /** Internal local-only composition. The returned source becomes unusable on lock/logout. */
  localNoteRepository(): BrowserLocalNoteRepository {
    this.assertWeb()
    const context = this.auth.requireContext(), identity = this.identity
    const storage = this.storage, bindings = this.bindings, keys = this.keys
    const lease = identity && keys?.leaseForAccount(identity.local_account_id)
    if (!identity || !storage || !bindings || !keys || !lease?.isCurrent()) throw new BrowserPersistenceError('closed')
    const scope = Object.freeze({ canonicalUserId: context.userId, accountId: identity.local_account_id, deviceId: identity.device_id })
    const source = storage.localSource.bind(this.auth, bindings, keys)
    const stages = storage.localStages.bind(this.auth, bindings, keys, source)
    return Object.freeze({ scope, source, stages, documents: storage.localDocuments.bind(this.auth, bindings, keys, source, stages) })
  }

  async lock(): Promise<void> {
    await this.revoke(false)
    if (this.auth.state === 'authenticated') this.everUnlocked = true
  }

  async logout(): Promise<void> {
    await this.auth.logout()
  }

  async dispose(): Promise<void> {
    if (this.disposed) return
    this.disposed = true
    if (typeof window !== 'undefined') window.removeEventListener('pagehide', this.onPageHide)
    try { await this.logout() } finally { this.unsubscribe(); await this.revoke(true) }
  }
}
