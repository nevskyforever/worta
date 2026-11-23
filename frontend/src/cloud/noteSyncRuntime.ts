import {ProgressSyncRuntime,type ProgressAuthorityView,type ProgressDecision} from './progressSyncRuntime'
import {GameSyncRuntime,type GameAuthorityView,type GameDecision,type GameCompensation} from './gameSyncRuntime'
import {DocumentSyncRuntime,type DocumentAuthorityView,type DocumentDecision} from './documentSyncRuntime'
import {MapSyncRuntime,type MapAuthorityView,type MapDecision} from './mapSyncRuntime'
import { ContentNoteRuntime, type ContentNoteConflict, type ContentNoteMigrationView } from './contentNoteRuntime'
import { AccountCatalogRuntime } from './accountCatalogRuntime'
import type { CatalogDecision,CatalogView } from '@/infrastructure/sqlite/accountCatalogRepository'
import { AuthoritativeAccountBinding } from '@/auth/accountBinding'
import { KeyNotProvisionedError, RuntimeKeyContext, type AuthoritativeKeyContextLease } from '@/auth/keyContext'
import { NormalUserAuthRuntime, StaleAuthContextError, type AuthContextSnapshot } from '@/auth/userAuth'
import { SQLiteCloudAccountBindingRepository, type CloudAccountBindingRepository } from '@/infrastructure/sqlite/cloudAccountBindingRepository'
import { SQLiteCloudIdentityRepository, type CloudIdentity, type CloudIdentityRepository } from '@/infrastructure/sqlite/cloudIdentityRepository'
import { SQLiteCloudProjectBootstrapRepository } from '@/infrastructure/sqlite/cloudProjectBootstrapRepository'
import { SQLiteNoteSyncAckRepository } from '@/infrastructure/sqlite/noteSyncAckRepository'
import { SQLiteNoteSyncInboxRepository } from '@/infrastructure/sqlite/noteSyncInboxRepository'
import { SQLiteNoteSyncIntentRepository } from '@/infrastructure/sqlite/noteSyncIntentRepository'
import { SQLiteNoteSyncOutboxRepository } from '@/infrastructure/sqlite/noteSyncOutboxRepository'
import { SQLiteNoteSyncRemoteApplyRepository } from '@/infrastructure/sqlite/noteSyncRemoteApplyRepository'
import { SQLiteNoteSyncResolutionUploadRepository } from '@/infrastructure/sqlite/noteSyncResolutionUploadRepository'
import { NoteSyncDeviceAckAdapter } from './noteSyncDeviceAck'
import { accountCryptoApi, type CurrentUserCryptoRecord } from '@/api/accountCrypto'
import { PendingAccountCryptoProvisioning } from './accountCryptoProvisioning'
import { NoteSyncInboxRemoteApplier } from './noteSyncInboxApply'
import { DurableNoteSyncInbox } from './noteSyncInbox'
import { NoteSyncOrchestrator, type NoteSyncOrchestratorOptions, type NoteSyncOrchestratorResult } from './noteSyncOrchestrator'
import { NoteSyncPuller } from './noteSyncPull'
import { NoteSyncUploader } from './noteSyncUpload'
import { NoteSyncV2Uploader } from './noteSyncV2Upload'
import { NoteSyncResolutionUploader } from './noteSyncResolutionUpload'
import { NoteSyncResolutionInboxApplier } from './noteSyncResolutionInboxApply'
import { DurableNoteSyncV2Inbox, NoteSyncV2AckAdapter } from './noteSyncV2Transport'
import { NoteSyncV2Cycle, type NoteSyncV2CycleResult } from './noteSyncV2Cycle'
import { NoteSyncV3Cycle } from './noteSyncV3Cycle'
import { StageStructuralRuntime } from './stageStructuralRuntime'
import type { StructuralView, StructuralDecision } from '@/infrastructure/sqlite/stageStructuralRepository'
import { ProjectMetadataMigrationRuntime } from './projectMetadataMigrationRuntime'
import type { MetadataMigrationStatus, MetadataAuthorityView, MetadataDecisionKind } from '@/infrastructure/sqlite/projectMetadataMigrationRepository'
import type { ProjectMetadata } from './projectMetadataCodec'
import { encryptedSyncV2Api } from '@/api/encryptedSyncV2'
import { NoteSyncTransportRouter, type NoteSyncProductionResult } from './noteSyncTransportRouter'
import { sealPendingNoteSyncIntents } from './noteSyncIntent'
import {
  CloudProjectBootstrapCoordinator,
  type CloudProjectBootstrapReporter,
  type CloudProjectBootstrapProgress,
  type CloudRegistryReconciliation,
} from './projectBootstrap'

const CANONICAL_UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/

interface NoteSyncRunner {
  runOnce(localAccountId: string, deviceId: string, options?: NoteSyncOrchestratorOptions): Promise<NoteSyncOrchestratorResult>
}

interface NoteSyncV2Runner {
  runOnce(localAccountId: string, deviceId: string, options?: NoteSyncOrchestratorOptions): Promise<NoteSyncV2CycleResult>
}

interface ProductionRunner {
  runOnce(localAccountId: string, deviceId: string, options?: NoteSyncOrchestratorOptions): Promise<NoteSyncProductionResult>
  uploadOnce(localAccountId: string): Promise<void>
}

interface RuntimeKeyManager {
  unlockWithPassphrase(localAccountId: string, passphrase: string): Promise<AuthoritativeKeyContextLease>
  leaseForAccount(localAccountId: string): AuthoritativeKeyContextLease | null
  lock(): Promise<void>
  dispose(): Promise<void>
}

interface ProjectBootstrapGate {
  reconcile(identity: { localAccountId: string, deviceId: string }): Promise<CloudRegistryReconciliation>
  preflightLocalProject(projectId: string): Promise<Array<{ note_id: string, code: string }>>
  runReadyCycle(identity: { localAccountId: string, deviceId: string }): Promise<NoteSyncProductionResult>
  bootstrapLocalProject(identity: { localAccountId: string, deviceId: string }, projectId: string, report?: CloudProjectBootstrapReporter): Promise<CloudProjectBootstrapProgress>
  importRemoteProject(identity: { localAccountId: string, deviceId: string }, projectId: string, displayName: string, report?: CloudProjectBootstrapReporter, metadataImport?: import('./projectMetadataMigrationRuntime').MetadataImportSnapshot | null): Promise<CloudProjectBootstrapProgress>
  setPaused(identity: { localAccountId: string, deviceId: string }, projectId: string, paused: boolean): Promise<CloudRegistryReconciliation>
}

export interface NoteSyncRuntimeDependencies {
  readonly auth?: NormalUserAuthRuntime
  readonly identityRepository?: CloudIdentityRepository
  readonly bindingRepository?: CloudAccountBindingRepository
  readonly bindings?: AuthoritativeAccountBinding
  readonly keys?: RuntimeKeyManager
  readonly orchestrator?: NoteSyncRunner
  readonly v2Cycle?: NoteSyncV2Runner
  readonly router?: ProductionRunner
  readonly bootstrap?: ProjectBootstrapGate
}

export interface NoteSyncRuntimeLoginResult {
  readonly context: AuthContextSnapshot
  readonly identity: CloudIdentity
}

export interface NoteSyncRuntimeUnlockResult {
  readonly identity: CloudIdentity
  readonly registry: CloudRegistryReconciliation
}

export class CloudIdentityUnavailableError extends Error {
  readonly name = 'CloudIdentityUnavailableError'
}

export class NoteSyncRuntimeDisposedError extends Error {
  readonly name = 'NoteSyncRuntimeDisposedError'
}

/**
 * Headless normal-user session composition. It is deliberately not connected
 * to desktop startup, UI, scheduler, or legacy document synchronization.
 */
export class NoteSyncRuntime {
  private readonly auth: NormalUserAuthRuntime
  private readonly identityRepository: CloudIdentityRepository
  private readonly bindings: AuthoritativeAccountBinding
  private readonly keys: RuntimeKeyManager
  private readonly router: ProductionRunner
  private readonly bootstrap: ProjectBootstrapGate
  private readonly contentNotes: ContentNoteRuntime
  private readonly progress: ProgressSyncRuntime
  private readonly game: GameSyncRuntime
  private readonly documents: DocumentSyncRuntime
  private readonly maps: MapSyncRuntime
  private readonly structural: StageStructuralRuntime
  private readonly catalog: AccountCatalogRuntime
  private readonly metadata: ProjectMetadataMigrationRuntime
  private flight: { readonly key: string, readonly promise: Promise<NoteSyncProductionResult> } | null = null
  private disposed = false

  constructor(dependencies: NoteSyncRuntimeDependencies = {}) {
    this.auth = dependencies.auth ?? new NormalUserAuthRuntime()
    this.identityRepository = dependencies.identityRepository ?? new SQLiteCloudIdentityRepository()
    const bindingRepository = dependencies.bindingRepository ?? new SQLiteCloudAccountBindingRepository()
    this.bindings = dependencies.bindings ?? new AuthoritativeAccountBinding(this.auth, bindingRepository)
    this.keys = dependencies.keys ?? new RuntimeKeyContext(this.auth, this.bindings)
    const composition = this.composeSync(dependencies)
    this.router = composition.router
    this.bootstrap = dependencies.bootstrap ?? composition.bootstrap
    this.metadata = composition.metadata
    this.progress=composition.progress
    this.game=composition.game
    this.documents=composition.documents
    this.maps = composition.maps
    this.structural = composition.structural
    this.contentNotes = composition.contentNotes
    this.catalog = composition.catalog
  }

  async login(username: string, password: string): Promise<NoteSyncRuntimeLoginResult> {
    this.assertNotDisposed()
    const context = await this.auth.login(username, password)
    const identity = await this.provisionFor(context)
    return { context, identity }
  }

  /** Reads the authenticated user's immutable E2EE bootstrap record. */
  async cryptoRecord(): Promise<CurrentUserCryptoRecord> {
    this.assertNotDisposed()
    return (await this.auth.authorized(accessToken => accountCryptoApi.get(accessToken))).value
  }

  /** Starts a transient first-provisioning context without exposing auth tokens. */
  async beginCryptoProvisioning(encryptionPassword: string): Promise<PendingAccountCryptoProvisioning> {
    this.assertNotDisposed()
    return PendingAccountCryptoProvisioning.begin(this.auth, encryptionPassword)
  }

  async submitCryptoProvisioning(pending: PendingAccountCryptoProvisioning): Promise<CurrentUserCryptoRecord> {
    this.assertNotDisposed()
    return pending.submit(this.auth)
  }

  async reconcileCryptoProvisioning(pending: PendingAccountCryptoProvisioning): Promise<CurrentUserCryptoRecord | null> {
    this.assertNotDisposed()
    return pending.reconcile(this.auth)
  }

  /** Unlocks an existing wrapped AMK without starting pull, upload, or ACK. */
  async unlock(passphrase: string): Promise<NoteSyncRuntimeUnlockResult> {
    this.assertNotDisposed()
    const context = this.auth.requireContext()
    const identity = await this.readFor(context)
    const lease = await this.keys.unlockWithPassphrase(identity.local_account_id, passphrase)
    this.assertLease(context, identity, lease)
    return { identity, registry: await this.bootstrap.reconcile(this.bootstrapIdentity(identity)) }
  }

  async retry(options?: NoteSyncOrchestratorOptions): Promise<NoteSyncProductionResult> {
    this.assertNotDisposed()
    const context = this.auth.requireContext()
    const identity = await this.readFor(context)
    return this.runFor(context, identity, options)
  }

  async reconcileProjects(): Promise<CloudRegistryReconciliation> {
    this.assertNotDisposed()
    const context = this.auth.requireContext()
    const identity = await this.readFor(context)
    this.assertUnlocked(context, identity)
    return this.bootstrap.reconcile(this.bootstrapIdentity(identity))
  }

  async preflightLocalProject(projectId: string): Promise<Array<{ note_id: string, code: string }>> {
    this.assertNotDisposed()
    const context = this.auth.requireContext()
    const identity = await this.readFor(context)
    this.assertUnlocked(context, identity)
    const issues = await this.bootstrap.preflightLocalProject(projectId)
    this.assertCurrent(context)
    return issues
  }

  async bootstrapLocalProject(projectId: string, report?: CloudProjectBootstrapReporter): Promise<CloudProjectBootstrapProgress> {
    this.assertNotDisposed()
    const context = this.auth.requireContext()
    const identity = await this.readFor(context)
    this.assertUnlocked(context, identity)
    return this.bootstrap.bootstrapLocalProject(this.bootstrapIdentity(identity), projectId, report)
  }

  async importRemoteProject(projectId: string, displayName: string, report?: CloudProjectBootstrapReporter): Promise<CloudProjectBootstrapProgress> {
    this.assertNotDisposed()
    const context = this.auth.requireContext()
    const identity = await this.readFor(context)
    this.assertUnlocked(context, identity)
    const snapshot = await this.metadata.importSnapshot(identity.local_account_id, identity.device_id, projectId)
    this.assertUnlocked(context, identity)
    if (snapshot && (!snapshot.head || !snapshot.metadata)) throw new TypeError('metadata_import_conflict')
    const name = snapshot?.metadata?.name ?? displayName
    if (!name.trim()) throw new TypeError('legacy_import_name_required')
    const result = await this.bootstrap.importRemoteProject(this.bootstrapIdentity(identity), projectId, name, report, snapshot)
    this.assertUnlocked(context, identity)
    if (snapshot?.head) {
      const view = await this.metadata.authority(identity.local_account_id, identity.device_id, projectId)
      if (view.head_event_id === snapshot.head && view.state === 'local_matches_authenticated' && view.local) {
        await this.metadata.adopt(identity.local_account_id, identity.device_id, projectId, snapshot.head, view.local)
      }
    }
    return result
  }

  async setProjectPaused(projectId: string, paused: boolean): Promise<CloudRegistryReconciliation> {
    this.assertNotDisposed()
    const context = this.auth.requireContext()
    const identity = await this.readFor(context)
    this.assertUnlocked(context, identity)
    return this.bootstrap.setPaused(this.bootstrapIdentity(identity), projectId, paused)
  }

  async prepareMetadataTransport(): Promise<void> {
    const context = this.auth.requireContext()
    const identity = await this.readFor(context)
    this.assertUnlocked(context, identity)
    await this.metadata.prepareTransport(identity.local_account_id, identity.device_id)
  }

  /** Explicit internal reader declaration. Every registered device must do this before mode-3 cutover. */
  async declareMetadataReaderReady(): Promise<void> {
    const context = this.auth.requireContext()
    const identity = await this.readFor(context)
    this.assertUnlocked(context, identity)
    await this.metadata.declareReaderReady(identity.local_account_id, identity.device_id)
  }

  /** Explicit irreversible account transition; no login/unlock/retry path invokes it. */
  async cutoverMetadataTransport(): Promise<void> {
    const context = this.auth.requireContext()
    const identity = await this.readFor(context)
    this.assertUnlocked(context, identity)
    await this.metadata.cutover(identity.local_account_id, identity.device_id)
  }

  /** Explicit per-project candidate and genesis decision; no automatic publication. */
  async projectProgressAuthority(projectId:string):Promise<ProgressAuthorityView>{const context=this.auth.requireContext();const identity=await this.readFor(context);this.assertUnlocked(context,identity);return this.progress.viewProgress(identity.local_account_id,identity.device_id,projectId)}
  async gameAuthority():Promise<GameAuthorityView>{const context=this.auth.requireContext();const identity=await this.readFor(context);this.assertUnlocked(context,identity);return this.game.viewGame(identity.local_account_id,identity.device_id)}
  async beginGameMigration():Promise<GameAuthorityView>{const context=this.auth.requireContext();const identity=await this.readFor(context);this.assertUnlocked(context,identity);return this.game.beginGame(identity.local_account_id,identity.device_id)}
  async rebuildGameProjection(ownerKey:string):Promise<void>{const context=this.auth.requireContext();const identity=await this.readFor(context);this.assertUnlocked(context,identity);await this.game.rebuildGame(identity.local_account_id,identity.device_id,ownerKey)}
  async chooseGameHistory(decision:GameDecision):Promise<void>{const context=this.auth.requireContext();const identity=await this.readFor(context);this.assertUnlocked(context,identity);await this.game.chooseGame(identity.local_account_id,identity.device_id,decision)}
  async compensateGameReward(decision:GameCompensation):Promise<void>{const context=this.auth.requireContext();const identity=await this.readFor(context);this.assertUnlocked(context,identity);await this.game.compensateGame(identity.local_account_id,identity.device_id,decision)}
  async beginProgressMigration(projectId:string):Promise<ProgressAuthorityView>{const context=this.auth.requireContext();const identity=await this.readFor(context);this.assertUnlocked(context,identity);return this.progress.beginProgress(identity.local_account_id,identity.device_id,projectId)}
  async chooseProgressHistory(decision:ProgressDecision):Promise<string>{const context=this.auth.requireContext();const identity=await this.readFor(context);this.assertUnlocked(context,identity);return this.progress.chooseProgress(identity.local_account_id,identity.device_id,decision)}
  async moveDocument(projectId:string,id:string,stage:string|null,expected:unknown):Promise<void>{const context=this.auth.requireContext();const identity=await this.readFor(context);this.assertUnlocked(context,identity);await this.documents.moveDocument(identity.local_account_id,identity.device_id,projectId,id,stage,expected)}
  async deleteDocument(projectId:string,id:string,expected:unknown):Promise<void>{const context=this.auth.requireContext();const identity=await this.readFor(context);this.assertUnlocked(context,identity);await this.documents.deleteDocument(identity.local_account_id,identity.device_id,projectId,id,expected)}
  async projectDocumentAuthority(projectId:string):Promise<DocumentAuthorityView>{const context=this.auth.requireContext();const identity=await this.readFor(context);this.assertUnlocked(context,identity);return this.documents.viewDocuments(identity.local_account_id,identity.device_id,projectId)}
  async beginDocumentMigration(projectId:string):Promise<DocumentAuthorityView>{const context=this.auth.requireContext();const identity=await this.readFor(context);this.assertUnlocked(context,identity);return this.documents.beginDocuments(identity.local_account_id,identity.device_id,projectId)}
  async chooseDocumentVersion(decision:DocumentDecision):Promise<void>{const context=this.auth.requireContext();const identity=await this.readFor(context);this.assertUnlocked(context,identity);await this.documents.chooseDocument(identity.local_account_id,identity.device_id,decision)}
  async projectMapAuthority(projectId:string):Promise<MapAuthorityView>{const context=this.auth.requireContext();const identity=await this.readFor(context);this.assertUnlocked(context,identity);return this.maps.viewMaps(identity.local_account_id,identity.device_id,projectId)}
  async beginMapMigration(projectId:string):Promise<MapAuthorityView>{const context=this.auth.requireContext();const identity=await this.readFor(context);this.assertUnlocked(context,identity);return this.maps.beginMaps(identity.local_account_id,identity.device_id,projectId)}
  async chooseMapVersion(decision:MapDecision,keepLocal?:boolean):Promise<void>{const context=this.auth.requireContext();const identity=await this.readFor(context);this.assertUnlocked(context,identity);await this.maps.chooseMap(identity.local_account_id,identity.device_id,decision,keepLocal)}
  async projectNoteAuthority(projectId:string):Promise<ContentNoteMigrationView>{const context=this.auth.requireContext();const identity=await this.readFor(context);this.assertUnlocked(context,identity);return this.contentNotes.viewNotes(identity.local_account_id,identity.device_id,projectId)}
  async beginNoteMigration(projectId:string):Promise<ContentNoteMigrationView>{const context=this.auth.requireContext();const identity=await this.readFor(context);this.assertUnlocked(context,identity);return this.contentNotes.beginNotes(identity.local_account_id,identity.device_id,projectId)}

  async noteConflicts(projectId:string):Promise<ContentNoteConflict[]>{const context=this.auth.requireContext();const identity=await this.readFor(context);this.assertUnlocked(context,identity);return this.contentNotes.conflicts(identity.local_account_id,identity.device_id,projectId)}
  async chooseNoteVersion(projectId:string,decision:ContentNoteConflict,selected:string):Promise<void>{const context=this.auth.requireContext();const identity=await this.readFor(context);this.assertUnlocked(context,identity);await this.contentNotes.choose(identity.local_account_id,identity.device_id,projectId,decision,selected)}

  async projectStructuralAuthority(projectId: string): Promise<StructuralView> {
    const context = this.auth.requireContext()
    const identity = await this.readFor(context)
    this.assertUnlocked(context, identity)
    return this.structural.view(identity.local_account_id, identity.device_id, projectId)
  }
  async beginStageMigration(projectId: string): Promise<StructuralView> {
    const context = this.auth.requireContext()
    const identity = await this.readFor(context)
    this.assertUnlocked(context, identity)
    return this.structural.beginStructure(identity.local_account_id, identity.device_id, projectId)
  }
  async decideStructure(projectId: string, decision: StructuralDecision): Promise<string> {
    const context = this.auth.requireContext()
    const identity = await this.readFor(context)
    this.assertUnlocked(context, identity)
    return this.structural.decideStructure(identity.local_account_id, identity.device_id, projectId, decision)
  }

  async beginProjectMetadataMigration(projectId: string): Promise<MetadataMigrationStatus> {
    const context = this.auth.requireContext()
    const identity = await this.readFor(context)
    this.assertUnlocked(context, identity)
    return this.metadata.begin(identity.local_account_id, identity.device_id, projectId)
  }

  async projectMetadataMigrationStatus(projectId: string): Promise<MetadataMigrationStatus> {
    const context = this.auth.requireContext()
    const identity = await this.readFor(context)
    this.assertUnlocked(context, identity)
    return this.metadata.status(identity.local_account_id, identity.device_id, projectId)
  }

  async projectCoverAuthority(projectId:string) {
    const context=this.auth.requireContext(),identity=await this.readFor(context)
    this.assertUnlocked(context,identity)
    return this.metadata.coverAuthority(identity.local_account_id,identity.device_id,projectId)
  }
  async projectCoverPreview(projectId:string,reference:import('./projectCoverReference').ProjectCoverReference){
    const context=this.auth.requireContext(),identity=await this.readFor(context);this.assertUnlocked(context,identity)
    return this.metadata.coverPreview(identity.local_account_id,identity.device_id,projectId,reference)
  }
  async beginCoverMigration(projectId:string):Promise<void> {
    const context=this.auth.requireContext(),identity=await this.readFor(context)
    this.assertUnlocked(context,identity)
    await this.metadata.captureCover(identity.local_account_id,identity.device_id,projectId)
  }

  async projectMetadataAuthority(projectId: string): Promise<MetadataAuthorityView> {
    const context = this.auth.requireContext()
    const identity = await this.readFor(context)
    this.assertUnlocked(context, identity)
    return this.metadata.authority(identity.local_account_id, identity.device_id, projectId)
  }

  async metadataTransportMode(): Promise<1 | 2 | 3> {
    const context = this.auth.requireContext()
    const identity = await this.readFor(context)
    this.assertUnlocked(context, identity)
    return this.metadata.mode(identity.local_account_id, identity.device_id)
  }

  async adoptProjectMetadata(projectId: string, expectedHead: string, expectedLocal: ProjectMetadata): Promise<MetadataAuthorityView> {
    const context = this.auth.requireContext()
    const identity = await this.readFor(context)
    this.assertUnlocked(context, identity)
    return this.metadata.adopt(identity.local_account_id, identity.device_id, projectId, expectedHead, expectedLocal)
  }

  async decideProjectMetadata(projectId: string, kind: MetadataDecisionKind, selectedEventId: string | null,
    proposed: ProjectMetadata | null, expectedLocal: ProjectMetadata, expectedTips: string[]): Promise<string> {
    const context = this.auth.requireContext()
    const identity = await this.readFor(context)
    this.assertUnlocked(context, identity)
    return this.metadata.decide(identity.local_account_id, identity.device_id, projectId,
      kind, selectedEventId, proposed, expectedLocal, expectedTips)
  }

  async catalogAuthority():Promise<CatalogView>{const context=this.auth.requireContext(),identity=await this.readFor(context);this.assertUnlocked(context,identity);return this.catalog.inspectCatalog(identity.local_account_id,identity.device_id)}
  async beginCatalogMigration():Promise<CatalogView>{const context=this.auth.requireContext(),identity=await this.readFor(context);this.assertUnlocked(context,identity);return this.catalog.beginCatalog(identity.local_account_id,identity.device_id)}
  async decideCatalog(decision:CatalogDecision):Promise<string>{const context=this.auth.requireContext(),identity=await this.readFor(context);this.assertUnlocked(context,identity);return this.catalog.decideCatalog(identity.local_account_id,identity.device_id,decision)}

  async lock(): Promise<void> {
    this.assertNotDisposed()
    await this.keys.lock()
  }

  async logout(): Promise<void> {
    this.assertNotDisposed()
    // auth invalidation invokes RuntimeKeyContext.lock() and drains existing uses.
    await this.auth.logout()
  }

  async dispose(): Promise<void> {
    if (this.disposed) return
    this.disposed = true
    await this.keys.dispose()
  }

  private composeSync(dependencies: NoteSyncRuntimeDependencies): { router: ProductionRunner, bootstrap: ProjectBootstrapGate, metadata: ProjectMetadataMigrationRuntime, structural: StageStructuralRuntime, catalog: AccountCatalogRuntime, contentNotes: ContentNoteRuntime, maps: MapSyncRuntime, documents: DocumentSyncRuntime, progress: ProgressSyncRuntime, game: GameSyncRuntime } {
    const intents = new SQLiteNoteSyncIntentRepository()
    const outbox = new SQLiteNoteSyncOutboxRepository()
    const inboxRepository = new SQLiteNoteSyncInboxRepository()
    const puller = new NoteSyncPuller(this.auth, this.bindings)
    const inbox = new DurableNoteSyncInbox(this.auth, this.bindings, puller, inboxRepository)
    const remoteApply = new SQLiteNoteSyncRemoteApplyRepository()
    const applier = new NoteSyncInboxRemoteApplier(
      this.auth, this.bindings, this.keys as RuntimeKeyContext, inboxRepository, remoteApply,
    )
    const uploader = new NoteSyncUploader(this.auth, this.bindings, outbox)
    const deviceAck = new NoteSyncDeviceAckAdapter(this.auth, this.bindings, new SQLiteNoteSyncAckRepository())
    const resolutionApplier = new NoteSyncResolutionInboxApplier(
      this.auth, this.bindings, this.keys as RuntimeKeyContext, inboxRepository, remoteApply,
    )
    const productionOrchestrator = new NoteSyncOrchestrator(
      this.auth, this.keys as RuntimeKeyContext, intents, uploader, inbox, applier, deviceAck, resolutionApplier,
    )
    const orchestrator = dependencies.orchestrator ?? productionOrchestrator
    const v2Uploader = new NoteSyncV2Uploader(this.auth, this.bindings, this.identityRepository, outbox)
    const metadata = new ProjectMetadataMigrationRuntime(this.auth, this.bindings, this.identityRepository, this.keys as RuntimeKeyContext)
    const contentNotes = new ContentNoteRuntime(this.auth,this.bindings,this.identityRepository,this.keys as RuntimeKeyContext)
    const progress = new ProgressSyncRuntime(this.auth,this.bindings,this.identityRepository,this.keys as RuntimeKeyContext)
    const game = new GameSyncRuntime(this.auth,this.bindings,this.identityRepository,this.keys as RuntimeKeyContext)
    const documents = new DocumentSyncRuntime(this.auth,this.bindings,this.identityRepository,this.keys as RuntimeKeyContext)
    const maps = new MapSyncRuntime(this.auth,this.bindings,this.identityRepository,this.keys as RuntimeKeyContext)
    const structural = new StageStructuralRuntime(this.auth, this.bindings, this.identityRepository, this.keys as RuntimeKeyContext)
    const resolutionUploader = new NoteSyncResolutionUploader(this.auth, this.bindings, this.identityRepository, new SQLiteNoteSyncResolutionUploadRepository())
    const v2Cycle = dependencies.v2Cycle ?? new NoteSyncV2Cycle(
      this.auth, this.bindings, this.identityRepository, this.keys as RuntimeKeyContext, deviceAck,
      intents, v2Uploader,
      resolutionUploader,
      new DurableNoteSyncV2Inbox(this.auth, this.bindings, this.identityRepository, inboxRepository),
      productionOrchestrator,
      new NoteSyncV2AckAdapter(this.auth, this.bindings, this.identityRepository, new SQLiteNoteSyncAckRepository()),
    )
    const catalog=new AccountCatalogRuntime(this.auth,this.bindings,this.identityRepository,this.keys as RuntimeKeyContext)
    const v3Cycle = new NoteSyncV3Cycle(this.auth, this.bindings, this.identityRepository, this.keys as RuntimeKeyContext,
      deviceAck, intents, v2Uploader, resolutionUploader, productionOrchestrator, metadata, structural, catalog, contentNotes, maps, documents, progress, game)
    const router = dependencies.router ?? new NoteSyncTransportRouter(this.auth, orchestrator, v2Cycle, uploader, v2Uploader, encryptedSyncV2Api, v3Cycle)
    const bootstrap = new CloudProjectBootstrapCoordinator(
      this.auth,
      new SQLiteCloudProjectBootstrapRepository(),
      {
        registerDevice: (localAccountId, deviceId) => deviceAck.registerOnce(localAccountId, deviceId),
        sealOnce: () => sealPendingNoteSyncIntents(intents, this.keys as RuntimeKeyContext),
        uploadOnce: localAccountId => router.uploadOnce(localAccountId),
        runOnce: (localAccountId, deviceId) => router.runOnce(localAccountId, deviceId),
      },
    )
    return { router, bootstrap, metadata, structural, catalog, contentNotes, maps, documents, progress, game }
  }

  private async provisionFor(context: AuthContextSnapshot): Promise<CloudIdentity> {
    const identity = await this.identityRepository.provision(context.userId)
    this.assertCurrent(context)
    this.assertIdentity(identity)
    const binding = await this.bindings.ensureForCurrentUser(identity.local_account_id)
    if (binding.context.userId !== context.userId || binding.context.authEpoch !== context.authEpoch) throw new StaleAuthContextError()
    this.assertCurrent(context)
    return identity
  }

  private async readFor(context: AuthContextSnapshot): Promise<CloudIdentity> {
    const identity = await this.identityRepository.read(context.userId)
    this.assertCurrent(context)
    if (identity === null) throw new CloudIdentityUnavailableError('No durable cloud identity is provisioned for this user.')
    this.assertIdentity(identity)
    const binding = await this.bindings.ensureForCurrentUser(identity.local_account_id)
    if (binding.context.userId !== context.userId || binding.context.authEpoch !== context.authEpoch) throw new StaleAuthContextError()
    this.assertCurrent(context)
    return identity
  }

  private async runFor(context: AuthContextSnapshot, identity: CloudIdentity, options?: NoteSyncOrchestratorOptions): Promise<NoteSyncProductionResult> {
    const lease = this.keys.leaseForAccount(identity.local_account_id)
    if (lease === null) throw new KeyNotProvisionedError()
    this.assertLease(context, identity, lease)
    const key = `${context.userId}\u0000${context.authEpoch}\u0000${identity.local_account_id}\u0000${identity.device_id}\u0000${lease.keyContextId}\u0000${lease.keyEpoch}`
    if (this.flight?.key === key) return this.flight.promise
    const promise = options === undefined
      ? this.bootstrap.runReadyCycle(this.bootstrapIdentity(identity))
      : this.runGatedWithOptions(identity, options)
    this.flight = { key, promise }
    try {
      return await promise
    } finally {
      if (this.flight?.promise === promise) this.flight = null
    }
  }

  private async runGatedWithOptions(identity: CloudIdentity, options: NoteSyncOrchestratorOptions): Promise<NoteSyncProductionResult> {
    const bootstrapIdentity = this.bootstrapIdentity(identity)
    const registry = await this.bootstrap.reconcile(bootstrapIdentity)
    if (!registry.readyForNormalCycle) return this.bootstrap.runReadyCycle(bootstrapIdentity)
    return this.router.runOnce(identity.local_account_id, identity.device_id, options)
  }

  private assertUnlocked(context: AuthContextSnapshot, identity: CloudIdentity): void {
    const lease = this.keys.leaseForAccount(identity.local_account_id)
    if (lease === null) throw new KeyNotProvisionedError()
    this.assertLease(context, identity, lease)
  }

  private bootstrapIdentity(identity: CloudIdentity): { localAccountId: string, deviceId: string } {
    return { localAccountId: identity.local_account_id, deviceId: identity.device_id }
  }

  private assertLease(context: AuthContextSnapshot, identity: CloudIdentity, lease: AuthoritativeKeyContextLease): void {
    this.assertCurrent(context)
    if (!lease.isCurrent() || lease.localAccountId !== identity.local_account_id
      || lease.canonicalUserId !== context.userId || lease.authEpoch !== context.authEpoch) {
      throw new StaleAuthContextError()
    }
  }

  private assertCurrent(context: AuthContextSnapshot): void {
    if (!this.auth.isCurrent(context)) throw new StaleAuthContextError()
  }

  private assertIdentity(identity: CloudIdentity): void {
    if (typeof identity.local_account_id !== 'string' || identity.local_account_id.length < 1
      || identity.local_account_id.length > 512 || !CANONICAL_UUID.test(identity.device_id)) {
      throw new CloudIdentityUnavailableError('Durable cloud identity is malformed.')
    }
  }

  private assertNotDisposed(): void {
    if (this.disposed) throw new NoteSyncRuntimeDisposedError()
  }
}
