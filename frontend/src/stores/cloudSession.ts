import type { CoverAuthorityStatus } from '@/cloud/projectMetadataMigrationRuntime'
import type {ProgressAuthorityView,ProgressDecision} from '@/cloud/progressSyncRuntime'
import type {GameAuthorityView,GameDecision,GameCompensation} from '@/cloud/gameSyncRuntime'
import type {DocumentAuthorityView,DocumentDecision} from '@/cloud/documentSyncRuntime'
import type {MapAuthorityView,MapDecision} from '@/cloud/mapSyncRuntime'
import type { ContentNoteConflict, ContentNoteMigrationView } from '@/cloud/contentNoteRuntime'
import type { CatalogDecision,CatalogView } from '@/infrastructure/sqlite/accountCatalogRepository'
import { diagnostics } from '@/diagnostics/service'
import { safeError } from '@/diagnostics/events'
import { presentStatus, technicalCode } from '@/diagnostics/presentation'
import type { StructuralDecision, StructuralView } from '@/infrastructure/sqlite/stageStructuralRepository'
import { MetadataImportContinuationError } from '@/cloud/projectMetadataMigrationRuntime'
import { announceDataChange } from '@/services/dataChanges'
import { onLocalNoteWrite } from '@/services/localNoteWrites'
import { defineStore } from 'pinia'
import { computed, ref } from 'vue'

import type { CurrentUserCryptoRecord } from '@/api/accountCrypto'
import { ApiError } from '@/api/client'
import { projectsApi } from '@/api/projects'
import { StaleAuthContextError } from '@/auth/userAuth'
import {
  AccountCryptoAlreadyProvisionedError,
  AccountCryptoProvisioningConflictError,
  PendingAccountCryptoProvisioning,
} from '@/cloud/accountCryptoProvisioning'
import {
  CloudProjectBootstrapBlockedError,
  type CloudProjectBootstrapProgress,
  type CloudProjectBootstrapReporter,
  type CloudProjectBootstrapStage,
  type CloudRegistryReconciliation,
} from '@/cloud/projectBootstrap'
import { NoteSyncRuntime, type NoteSyncRuntimeUnlockResult } from '@/cloud/noteSyncRuntime'
import { isSyncNetworkFailure } from '@/cloud/syncNetworkFailure'
import type { NoteSyncProductionResult } from '@/cloud/noteSyncTransportRouter'
import type { MetadataAuthorityView, MetadataDecisionKind, MetadataMigrationStatus } from '@/infrastructure/sqlite/projectMetadataMigrationRepository'
import type { ProjectMetadata } from '@/cloud/projectMetadataCodec'
import { canEnableCloudProjectSync } from '@/cloud/capabilities'
import type { CloudProjectBootstrapRecord } from '@/infrastructure/sqlite/cloudProjectBootstrapRepository'
import { currentPlatform } from '@/platform/runtime'
import { desktopCloudEnabled } from '@/platform/desktopCloud'

export type CloudSessionStatus =
  | 'unavailable'
  | 'logged_out'
  | 'provisioning'
  | 'key_locked'
  | 'ready'
  | 'syncing'
  | 'completed'
  | 'retryable_error'
  | 'blocked'
  | 'remaining_work'

export type CloudProjectUiStatus =
  | 'local_only'
  | 'unsupported'
  | 'import_available'
  | 'registering'
  | 'preparing_initial_notes'
  | 'uploading_initial_notes'
  | 'completing_registration'
  | 'pulling_remote_notes'
  | 'initial_sync_completed'
  | 'remaining_work'
  | 'paused'
  | 'blocked'

export interface CloudProjectView {
  readonly projectId: string
  readonly name: string | null
  readonly origin: 'local' | 'remote'
  readonly status: CloudProjectUiStatus
  readonly connectionAvailable: boolean
  readonly mode: 'upload_existing' | 'import_remote' | null
  readonly reasonCode?: string
  readonly reason: string | null
}

interface LocalProjectSummary {
  readonly id: string
  readonly name: string
}

export interface CloudSessionRuntime {
  gameAuthority?():Promise<GameAuthorityView>
  beginGameMigration?():Promise<GameAuthorityView>
  chooseGameHistory?(decision:GameDecision):Promise<void>
  compensateGameReward?(decision:GameCompensation):Promise<void>
  rebuildGameProjection?(ownerKey:string):Promise<void>
  projectProgressAuthority?(projectId:string):Promise<ProgressAuthorityView>
  beginProgressMigration?(projectId:string):Promise<ProgressAuthorityView>
  chooseProgressHistory?(decision:ProgressDecision):Promise<void>

  login(username: string, password: string): Promise<{ context: { username: string } }>
  cryptoRecord(): Promise<CurrentUserCryptoRecord>
  beginCryptoProvisioning(encryptionPassword: string): Promise<PendingAccountCryptoProvisioning>
  submitCryptoProvisioning(pending: PendingAccountCryptoProvisioning): Promise<CurrentUserCryptoRecord>
  reconcileCryptoProvisioning(pending: PendingAccountCryptoProvisioning): Promise<CurrentUserCryptoRecord | null>
  unlock(passphrase: string): Promise<NoteSyncRuntimeUnlockResult>
  reconcileProjects(): Promise<CloudRegistryReconciliation>
  preflightLocalProject(projectId: string): Promise<Array<{ note_id: string, code: string }>>
  bootstrapLocalProject(projectId: string, report?: CloudProjectBootstrapReporter): Promise<CloudProjectBootstrapProgress>
  importRemoteProject(projectId: string, displayName: string, report?: CloudProjectBootstrapReporter): Promise<CloudProjectBootstrapProgress>
  setProjectPaused(projectId: string, paused: boolean): Promise<CloudRegistryReconciliation>
  retry(): Promise<NoteSyncProductionResult>
  catalogAuthority?():Promise<CatalogView>
  beginCatalogMigration?():Promise<CatalogView>
  decideCatalog?(decision:CatalogDecision):Promise<string>
  projectMapAuthority?(projectId:string):Promise<MapAuthorityView>
  beginMapMigration?(projectId:string):Promise<MapAuthorityView>
  projectDocumentAuthority?(projectId:string):Promise<DocumentAuthorityView>
  beginDocumentMigration?(projectId:string):Promise<DocumentAuthorityView>
  chooseDocumentVersion?(decision:DocumentDecision):Promise<void>
  moveDocument?(projectId:string,id:string,stage:string|null,expected:unknown):Promise<void>
  deleteDocument?(projectId:string,id:string,expected:unknown):Promise<void>
  chooseMapVersion?(decision:MapDecision,keepLocal?:boolean):Promise<void>
  projectNoteAuthority?(projectId:string):Promise<ContentNoteMigrationView>
  beginNoteMigration?(projectId:string):Promise<ContentNoteMigrationView>
  noteConflicts?(projectId:string):Promise<ContentNoteConflict[]>
  chooseNoteVersion?(projectId:string,decision:ContentNoteConflict,selected:string):Promise<void>
  projectStructuralAuthority(projectId: string): Promise<StructuralView>
  beginStageMigration(projectId: string): Promise<StructuralView>
  decideStructure(projectId: string, decision: StructuralDecision): Promise<string>
  metadataTransportMode(): Promise<1 | 2 | 3>
  prepareMetadataTransport(): Promise<void>
  declareMetadataReaderReady(): Promise<void>
  cutoverMetadataTransport(): Promise<void>
  projectCoverPreview?(projectId:string,reference:import('@/cloud/projectCoverReference').ProjectCoverReference):Promise<string|null>
  projectCoverAuthority?(projectId:string):Promise<CoverAuthorityStatus>
  beginCoverMigration?(projectId:string):Promise<void>
  projectMetadataAuthority(projectId: string): Promise<MetadataAuthorityView>
  beginProjectMetadataMigration(projectId: string): Promise<MetadataMigrationStatus>
  adoptProjectMetadata(projectId: string, expectedHead: string, expectedLocal: ProjectMetadata): Promise<MetadataAuthorityView>
  decideProjectMetadata(projectId: string, kind: MetadataDecisionKind, selectedEventId: string | null,
    proposed: ProjectMetadata | null, expectedLocal: ProjectMetadata, expectedTips: string[]): Promise<string>
  lock(): Promise<void>
  logout(): Promise<void>
  dispose(): Promise<void>
}

let runtimeFactory: () => CloudSessionRuntime = () => new NoteSyncRuntime()
let projectLoader: () => Promise<LocalProjectSummary[]> = async () => (
  (await projectsApi.list({})).map(project => ({ id: project.id, name: project.name }))
)

/** Test-only injection; production continues to construct the single runtime directly. */
export function configureCloudSessionRuntimeFactoryForTests(factory: (() => CloudSessionRuntime) | null): void {
  runtimeFactory = factory ?? (() => new NoteSyncRuntime())
}

/** Test-only injection for the existing platform project reader. */
export function configureCloudSessionProjectLoaderForTests(loader: (() => Promise<LocalProjectSummary[]>) | null): void {
  projectLoader = loader ?? (async () => (await projectsApi.list({})).map(project => ({ id: project.id, name: project.name })))
}

function safeErrorMessage(error: unknown): string {
  if (error instanceof AccountCryptoProvisioningConflictError) {
    return 'Запись шифрования уже существует и отличается от подготовленной. Перезапись запрещена.'
  }
  if (error instanceof AccountCryptoAlreadyProvisionedError) {
    return 'Для этого аккаунта уже настроено шифрование. Разблокируйте существующий ключ.'
  }
  if (error instanceof CloudProjectBootstrapBlockedError) return safeBootstrapReason(error.code)
  if (error instanceof ApiError && error.status === 0) return 'Не удалось связаться с сервером. Можно безопасно повторить операцию.'
  if (error instanceof ApiError && error.status === 401) return 'Сеанс аккаунта истёк. Войдите снова.'
  if (error instanceof StaleAuthContextError) return 'Сеанс аккаунта изменился. Войдите снова.'
  return 'Операция облачной синхронизации не завершена. Повторите её после проверки подключения.'
}

function safeBootstrapReason(code: string): string { return presentStatus('error', code).description }
function safeRegistryReason(code: string): string { return presentStatus('error', code).description }

function isAuthenticationFailure(error: unknown): boolean {
  return (error instanceof ApiError && error.status === 401) || error instanceof StaleAuthContextError
}

function statusForRecord(record: CloudProjectBootstrapRecord): CloudProjectUiStatus {
  if (record.phase === 'prepared') return 'registering'
  if (record.phase === 'registered') return 'preparing_initial_notes'
  if (record.phase === 'captured') return 'uploading_initial_notes'
  if (record.phase === 'completing') return 'completing_registration'
  if (record.phase === 'ready') return 'initial_sync_completed'
  if (record.phase === 'paused') return 'paused'
  return 'blocked'
}

export const useCloudSessionStore = defineStore('cloud-session', () => {
  // Runtime, key material, server bootstrap tokens and pending Recovery Key are
  // lexical-only. Pinia exposes only redacted account/project presentation data.
  let runtime: CloudSessionRuntime | null = null
  let pending: PendingAccountCryptoProvisioning | null = null
  let syncFlight: { readonly epoch: number, readonly promise: Promise<void> } | null = null
  let projectFlight: { readonly epoch: number, readonly promise: Promise<void> } | null = null
  let unlockFlight: { readonly epoch: number, readonly promise: Promise<void> } | null = null
  let unlocked = false
  let lifecycleEpoch = 0
  let recoveryTimer: ReturnType<typeof setTimeout> | null = null
  let recoveryGeneration = 0
  let noteListener: Promise<() => void> | null = null
  const pendingNoteProjects = new Set<string>()
  const readyNoteProjects = new Set<string>()
  let noteFlight: { readonly epoch: number, readonly promise: Promise<void> } | null = null

  const supported = ref(false)
  const status = ref<CloudSessionStatus>('unavailable')
  const username = ref<string | null>(null)
  const hasProvisionedKey = ref(false)
  const errorMessage = ref<string | null>(null)
  const errorCode = ref('unknown_error')
  const blockedEventCodes = ref<readonly string[]>([])
  const blockedEvents = ref<readonly string[]>([])
  const hasRemainingWork = ref(false)
  const lastCycleAt = ref<string | null>(null)
  const busy = ref(false)
  const projects = ref<CloudProjectView[]>([])
  const metadataTransportMode = ref<1 | 2 | 3 | null>(null)
  const catalogAuthority=ref<CatalogView|null>(null)
  const structuralAuthority = ref<Record<string, StructuralView>>({})
  const coverAuthority=ref<Record<string,CoverAuthorityStatus>>({})
  const metadataAuthority = ref<Record<string, MetadataAuthorityView>>({})
  const canRunCycle = ref(false)
  const projectBootstrapEnabled = canEnableCloudProjectSync()
  const authenticated = computed(() => username.value !== null)

  function resetVisibleSession(): void {
    unlocked = false
    pendingNoteProjects.clear()
    readyNoteProjects.clear()
    username.value = null
    hasProvisionedKey.value = false
    errorMessage.value = null
    blockedEvents.value = []
    blockedEventCodes.value = []
    hasRemainingWork.value = false
    lastCycleAt.value = null
    projects.value = []
    metadataTransportMode.value = null
    coverAuthority.value = {}
    metadataAuthority.value = {}
    structuralAuthority.value = {}
    progressAuthority.value = {}
    gameAuthority.value = null
    documentAuthority.value = {}
    mapAuthority.value = {}
    noteAuthority.value = {}
    noteConflicts.value = {}
    catalogAuthority.value=null
    canRunCycle.value = false
  }

  function clearPending(): void {
    pending?.dispose()
    pending = null
  }

  function requireRuntime(): CloudSessionRuntime {
    if (!desktopCloudEnabled() || runtime === null) throw new Error('Desktop cloud session is unavailable.')
    return runtime
  }

  function current(epoch: number): boolean {
    return lifecycleEpoch === epoch
  }

  function stopRecovery(): void {
    recoveryGeneration += 1
    if (recoveryTimer !== null) clearTimeout(recoveryTimer)
    recoveryTimer = null
  }

  function scheduleRecovery(epoch: number, generation: number): void {
    if (!current(epoch) || generation !== recoveryGeneration || !unlocked || !authenticated.value
      || !canRunCycle.value || readyNoteProjects.size === 0 || recoveryTimer !== null) return
    // Native sealed-outbox retries can be deferred for up to five minutes,
    // including across restarts. Do not report a successful recovery before
    // those durable rows become eligible for another upload.
    const delay = 300_000
    recoveryTimer = setTimeout(() => {
      recoveryTimer = null
      if (!current(epoch) || generation !== recoveryGeneration || !unlocked || !authenticated.value
        || !canRunCycle.value || readyNoteProjects.size === 0) return
      void retry(undefined, generation).catch(() => undefined)
    }, delay)
  }

  function setFailure(error: unknown, epoch = lifecycleEpoch): void {
    if (!current(epoch)) return
    errorMessage.value = safeErrorMessage(error)
    errorCode.value = technicalCode(error && typeof error === 'object' && 'code' in error && typeof error.code === 'string' ? error.code : 'unknown_error')
    hasRemainingWork.value = false
    if (isAuthenticationFailure(error)) {
      lifecycleEpoch += 1
      stopRecovery()
      clearPending()
      resetVisibleSession()
      busy.value = false
      status.value = 'logged_out'
      return
    }
    status.value = error instanceof AccountCryptoProvisioningConflictError
      || error instanceof CloudProjectBootstrapBlockedError ? 'blocked' : 'retryable_error'
  }

  function initialize(): void {
    if (currentPlatform() !== 'tauri' || !desktopCloudEnabled()) {
      supported.value = false
      status.value = 'unavailable'
      return
    }
    supported.value = true
    if (runtime === null) runtime = runtimeFactory()
    if (noteListener === null) {
      noteListener = Promise.resolve().then(() => onLocalNoteWrite(queueNoteSync))
      // Login awaits registration and reports failures through the existing status.
      void noteListener.catch(() => undefined)
    }
    if (status.value === 'unavailable') status.value = 'logged_out'
  }

  async function login(accountUsername: string, accountPassword: string): Promise<void> {
    const activeRuntime = requireRuntime()
    const epoch = ++lifecycleEpoch
    stopRecovery()
    unlocked = false
    pendingNoteProjects.clear()
    busy.value = true
    errorMessage.value = null
    clearPending()
    projects.value = []
    try {
      if (noteListener) await noteListener
      if (!current(epoch)) return
      const result = await activeRuntime.login(accountUsername, accountPassword)
      if (!current(epoch)) return
      username.value = result.context.username
      const record = await activeRuntime.cryptoRecord()
      if (!current(epoch)) return
      hasProvisionedKey.value = record.provisioned
      status.value = record.provisioned ? 'key_locked' : 'provisioning'
    } catch (error) {
      setFailure(error, epoch)
      throw error
    } finally {
      if (current(epoch)) busy.value = false
    }
  }

  async function prepareProvisioning(encryptionPassword: string): Promise<Uint8Array> {
    const activeRuntime = requireRuntime()
    if (status.value !== 'provisioning' || pending !== null) throw new Error('Encryption provisioning is not available.')
    const epoch = lifecycleEpoch
    busy.value = true
    errorMessage.value = null
    try {
      const prepared = await activeRuntime.beginCryptoProvisioning(encryptionPassword)
      if (!current(epoch)) {
        prepared.dispose()
        throw new StaleAuthContextError()
      }
      pending = prepared
      return prepared.recoveryKeyForDisplay()
    } catch (error) {
      setFailure(error, epoch)
      if (current(epoch) && !isAuthenticationFailure(error) && pending === null) status.value = 'provisioning'
      throw error
    } finally {
      if (current(epoch)) busy.value = false
    }
  }

  async function submitProvisioning(): Promise<void> {
    const activePending = pending
    if (activePending === null) throw new Error('Encryption provisioning is not available.')
    const epoch = lifecycleEpoch
    busy.value = true
    errorMessage.value = null
    try {
      activePending.confirmRecoveryKeySaved()
      await requireRuntime().submitCryptoProvisioning(activePending)
      if (!current(epoch)) return
      pending = null
      hasProvisionedKey.value = true
      status.value = 'key_locked'
    } catch (error) {
      setFailure(error, epoch)
      if (current(epoch) && !(error instanceof AccountCryptoProvisioningConflictError) && pending === activePending) status.value = 'provisioning'
      throw error
    } finally {
      if (current(epoch)) busy.value = false
    }
  }

  async function reconcileProvisioning(): Promise<boolean> {
    const activePending = pending
    if (activePending === null) return false
    const epoch = lifecycleEpoch
    busy.value = true
    errorMessage.value = null
    try {
      const record = await requireRuntime().reconcileCryptoProvisioning(activePending)
      if (!current(epoch) || record === null) return false
      pending = null
      hasProvisionedKey.value = true
      status.value = 'key_locked'
      return true
    } catch (error) {
      setFailure(error, epoch)
      throw error
    } finally {
      if (current(epoch)) busy.value = false
    }
  }

  function cancelProvisioning(): void {
    clearPending()
    errorMessage.value = null
    if (username.value !== null) status.value = hasProvisionedKey.value ? 'key_locked' : 'provisioning'
  }

  function applyCycle(result: NoteSyncProductionResult, epoch: number): void {
    if (!current(epoch)) return
    blockedEventCodes.value = result.blocked.map(code => technicalCode(code) === 'unknown_error' ? 'blocked' : technicalCode(code))
    blockedEvents.value = result.blocked.map(() => 'Есть зашифрованное событие, требующее отдельного безопасного решения.')
    hasRemainingWork.value = result.hasRemainingWork
    lastCycleAt.value = new Date().toISOString()
    if (result.blocked.length > 0) status.value = 'blocked'
    else if (result.errors.length > 0) status.value = 'retryable_error'
    else if (result.hasRemainingWork) status.value = 'remaining_work'
    else status.value = 'completed'
  }

  async function runCycle(operation: () => Promise<NoteSyncProductionResult>, correlation?: string, requiredRecovery?: number): Promise<void> {
    const epoch = lifecycleEpoch
    if (syncFlight?.epoch === epoch) return syncFlight.promise
    const recoveryToken = recoveryGeneration
    const previousProject = projectFlight?.epoch === epoch ? projectFlight.promise : null
    status.value = 'syncing'
    errorMessage.value = null
    let promise!: Promise<void>
    promise = Promise.resolve().then(async () => {
      try {
        if (previousProject) await previousProject.catch(() => undefined)
        if (!current(epoch)) return
        if (requiredRecovery !== undefined && (requiredRecovery !== recoveryGeneration
          || !unlocked || !canRunCycle.value || readyNoteProjects.size === 0)) {
          if (status.value === 'syncing') status.value = canRunCycle.value ? 'ready' : 'blocked'
          return
        }
        const result = await diagnostics.run('sync', 'sync_cycle', async correlation => {
          const result = await operation()
          diagnostics.record('sync', 'sync_cycle', 'sync_result', correlation, { count: result.blocked.length, pending: result.errors.length, retry: result.hasRemainingWork }, result.blocked.length || result.errors.length ? 'warning' : 'info')
          if (result.transport_version === 3) {
            diagnostics.record('sync', 'sync_cycle', 'pull_result', correlation, { count: result.cycle.pulled.reduce((sum, page) => sum + page.new_events, 0) })
            diagnostics.record('sync', 'sync_cycle', 'upload_result', correlation, { count: result.cycle.noteUploaded + result.cycle.resolutionUploaded + result.cycle.metadataUploaded })
            for (const apply of [result.cycle.metadataApply, result.cycle.structuralApply]) {
              if (apply) diagnostics.record('sync', 'sync_cycle', 'apply_result', correlation, { applied: apply.applied, conflicts: apply.conflicts, orphans: apply.orphans, pending: apply.blocked.length }, apply.blocked.length || apply.conflicts ? 'warning' : 'info')
            }
            if (result.cycle.ack) diagnostics.record('sync', 'sync_cycle', 'ack_result', correlation, { status: result.cycle.ack.status })
          }
          for (const blocker of result.blocked.slice(0, 8)) {
            diagnostics.record('sync', 'sync_cycle', 'blocker', correlation, { status: 'blocked', error_code: technicalCode(blocker) }, 'warning')
          }
          for (const error of result.errors.slice(0, 8)) diagnostics.record('sync', 'sync_cycle', 'failed', correlation, safeError(error), 'error')
          return result
        }, correlation)
        if (current(epoch)) await refreshProjects()
        if (current(epoch) && result.transport_version === 3) {
          const gameChanged=(result.cycle.gameApply?.applied??0)+(result.cycle.gameApply?.conflicts??0)
          // A local deferred mutation can block authority without creating any
          // Game inbox event. Refresh an inspected view even in an idle cycle.
          if(gameAuthority.value)await inspectGame()
          if(gameChanged>0&&current(epoch))announceDataChange('game')
          const progressChanged=(result.cycle.progressApply?.applied??0)+(result.cycle.progressApply?.conflicts??0)
          if(progressChanged>0)for(const projectId of Object.keys(progressAuthority.value)){if(!current(epoch))break;await inspectProgress(projectId)}
          const documentsChanged=(result.cycle.documentApply?.applied??0)+(result.cycle.documentApply?.conflicts??0)
          if(documentsChanged>0)for(const projectId of Object.keys(documentAuthority.value)){if(!current(epoch))break;await inspectDocuments(projectId)}
          const mapsChanged=(result.cycle.mapApply?.applied??0)+(result.cycle.mapApply?.conflicts??0)
          if(mapsChanged>0)for(const projectId of Object.keys(mapAuthority.value)){if(!current(epoch))break;await inspectMaps(projectId)}
          if(current(epoch)&&((result.cycle.metadataApply?.applied??0)+(result.cycle.structuralApply?.applied??0)+mapsChanged+documentsChanged+progressChanged)>0)announceDataChange('projects')
        }
        applyCycle(result, epoch)
        if (current(epoch) && recoveryToken === recoveryGeneration) {
          if (result.blocked.length === 0 && result.errors.length > 0
            && result.errors.every(error => error.code === 'network_error')) {
            errorMessage.value = 'Не удалось связаться с сервером. Можно безопасно повторить операцию.'
            scheduleRecovery(epoch, recoveryToken)
          } else {
            stopRecovery()
          }
        }
      } catch (error) {
        setFailure(error, epoch)
        if (current(epoch) && recoveryToken === recoveryGeneration) {
          if (isSyncNetworkFailure(error)) scheduleRecovery(epoch, recoveryToken)
          else stopRecovery()
        }
        throw error
      } finally {
        if (syncFlight?.promise === promise) syncFlight = null
      }
    })
    syncFlight = { epoch, promise }
    return promise
  }

  function unlock(encryptionPassword: string): Promise<void> {
    const epoch = lifecycleEpoch
    if (unlockFlight?.epoch === epoch) return unlockFlight.promise
    if (unlocked) return Promise.resolve()
    busy.value = true
    errorMessage.value = null
    let promise!: Promise<void>
    promise = (async () => { try {
      const result = await diagnostics.run('encryption', 'unlock', () => requireRuntime().unlock(encryptionPassword))
      if (!current(epoch)) return
      await applyRegistry(result.registry, epoch)
      if (!current(epoch)) return
      unlocked = true
      status.value = result.registry.readyForNormalCycle ? 'ready' : 'blocked'
      if (result.registry.readyForNormalCycle && result.registry.local.some(project => project.phase === 'ready')) {
        // The normal C21 cycle revalidates authentication, key lease and registry.
        // Its failure is presented by runCycle; do not hold the unlock UI open.
        void retry().catch(() => undefined)
      }
    } catch (error) {
      setFailure(error, epoch)
      throw error
    } finally {
      if (unlockFlight?.promise === promise) unlockFlight = null
      if (current(epoch)) busy.value = false
    } })()
    unlockFlight = { epoch, promise }
    return promise
  }

  async function applyRegistry(registry: CloudRegistryReconciliation, epoch: number): Promise<void> {
    const localProjects = await projectLoader()
    if (!current(epoch)) return
    const localById = new Map(localProjects.map(project => [project.id, project]))
    const recordById = new Map(registry.local.map(record => [record.project_id, record]))
    const remoteById = new Map(registry.remote.map(project => [project.project_id, project]))
    const next: CloudProjectView[] = []

    for (const project of localProjects) {
      const record = recordById.get(project.id)
      const remote = remoteById.get(project.id)
      if (record) {
        next.push({
          projectId: project.id, name: project.name, origin: 'local',
          status: statusForRecord(record), connectionAvailable: false,
          mode: record.mode, reasonCode: record.blocked_reason ? technicalCode(record.blocked_reason) : undefined, reason: record.blocked_reason === null ? null : safeBootstrapReason(record.blocked_reason),
        })
        continue
      }
      if (remote) {
        next.push({
          projectId: project.id, name: project.name, origin: 'local', status: 'blocked',
          connectionAvailable: false, mode: null,
          reason: 'На устройстве есть похожий проект, но связь с облачной версией не подтверждена. Импорт и объединение пока недоступны.',
        })
        continue
      }
      let issues: Array<{ note_id: string, code: string }> = []
      try {
        issues = await requireRuntime().preflightLocalProject(project.id)
      } catch (error) {
        if (isAuthenticationFailure(error)) throw error
        issues = [{ note_id: '', code: 'invalid_note_payload' }]
      }
      if (!current(epoch)) return
      next.push({
        projectId: project.id, name: project.name, origin: 'local',
        status: issues.length ? 'unsupported' : 'local_only',
        connectionAvailable: issues.length === 0, mode: null,
        reasonCode: issues.length ? technicalCode(issues[0]!.code) : undefined,
        reason: issues.length ? safeBootstrapReason(issues[0]!.code) : null,
      })
    }

    for (const remote of registry.remote) {
      if (recordById.has(remote.project_id) || localById.has(remote.project_id)) continue
      const importAvailable = remote.state === 'active' && remote.bootstrap_id !== null
      next.push({
        projectId: remote.project_id, name: null, origin: 'remote',
        status: importAvailable ? 'import_available' : 'blocked',
        connectionAvailable: importAvailable, mode: 'import_remote',
        reason: importAvailable ? null : remote.state === 'legacy'
          ? 'Удалённый проект имеет устаревшее или неизвестное состояние. Безопасный импорт недоступен.'
          : 'Первоначальное подключение удалённого проекта выполняется на другом устройстве.',
      })
    }

    projects.value = next.sort((left, right) => (left.name ?? left.projectId).localeCompare(right.name ?? right.projectId))
    readyNoteProjects.clear()
    for (const record of registry.local) if (record.phase === 'ready') readyNoteProjects.add(record.project_id)
    blockedEvents.value = [...new Set(registry.reasons.map(safeRegistryReason))]
    blockedEventCodes.value = [...new Set(registry.reasons.map(technicalCode))]
    canRunCycle.value = registry.readyForNormalCycle
    if (!canRunCycle.value || readyNoteProjects.size === 0) stopRecovery()
    hasRemainingWork.value = !registry.readyForNormalCycle
  }

  async function refreshProjects(): Promise<void> {
    const epoch = lifecycleEpoch
    try {
      const registry = await requireRuntime().reconcileProjects()
      if (!current(epoch)) return
      await applyRegistry(registry, epoch)
    } catch (error) {
      setFailure(error, epoch)
      throw error
    }
  }

  async function prepareProjectConnection(projectId: string): Promise<boolean> {
    if (!projectBootstrapEnabled) return false
    const epoch = lifecycleEpoch
    errorMessage.value = null
    const issues = await requireRuntime().preflightLocalProject(projectId)
    if (!current(epoch)) return false
    if (issues.length) {
      const reason = safeBootstrapReason(issues[0]!.code)
      projects.value = projects.value.map(project => project.projectId === projectId
        ? { ...project, status: 'unsupported', connectionAvailable: false, reason }
        : project)
      return false
    }
    return true
  }

  function setProjectStage(projectId: string, stage: CloudProjectBootstrapStage, epoch: number): void {
    if (!current(epoch)) return
    projects.value = projects.value.map(project => project.projectId === projectId
      ? { ...project, status: stage, connectionAvailable: false, reason: null }
      : project)
  }

  async function runProjectOperation(
    projectId: string,
    operation: (report: CloudProjectBootstrapReporter) => Promise<CloudProjectBootstrapProgress>,
  ): Promise<void> {
    const epoch = lifecycleEpoch
    if (projectFlight?.epoch === epoch) return projectFlight.promise
    const previousSync = syncFlight?.epoch === epoch ? syncFlight.promise : null
    busy.value = true
    errorMessage.value = null
    const report: CloudProjectBootstrapReporter = stage => setProjectStage(projectId, stage, epoch)
    let promise!: Promise<void>
    promise = Promise.resolve().then(async () => {
      try {
        if (previousSync) await previousSync.catch(() => undefined)
        if (!current(epoch)) return
        const result = await operation(report)
        if (!current(epoch)) return
        await applyRegistry(result.registry, epoch)
        if (result.cycle) applyCycle(result.cycle, epoch)
        if (result.hasRemainingWork) setProjectStage(projectId, 'remaining_work', epoch)
        else setProjectStage(projectId, 'initial_sync_completed', epoch)
      } catch (error) {
        if (error instanceof MetadataImportContinuationError) {
          if (current(epoch)) projects.value = projects.value.map(project => project.projectId === projectId
            ? { ...project, status: 'import_available', connectionAvailable: true, reason: null } : project)
          throw error
        }
        setFailure(error, epoch)
        if (current(epoch)) {
          projects.value = projects.value.map(project => project.projectId === projectId
            ? { ...project, status: 'blocked', connectionAvailable: false, reason: safeErrorMessage(error) }
            : project)
        }
        throw error
      } finally {
        if (projectFlight?.promise === promise) projectFlight = null
        if (current(epoch)) busy.value = false
      }
    })
    projectFlight = { epoch, promise }
    return promise
  }

  async function bootstrapProject(projectId: string): Promise<void> {
    if (!projectBootstrapEnabled) throw new CloudProjectBootstrapBlockedError('project_bootstrap_disabled')
    await diagnostics.run('projects', 'project_connect', () => runProjectOperation(projectId, report => requireRuntime().bootstrapLocalProject(projectId, report)))
  }

  async function importProject(projectId: string, displayName: string): Promise<void> {
    if (!projectBootstrapEnabled) throw new CloudProjectBootstrapBlockedError('project_bootstrap_disabled')
    const epoch = lifecycleEpoch
    await diagnostics.run('projects', 'project_import', () => runProjectOperation(projectId, report => requireRuntime().importRemoteProject(projectId, displayName.trim(), report)))
    if (current(epoch)) announceDataChange('projects')
  }

  async function resumeProject(projectId: string): Promise<void> {
    const project = projects.value.find(item => item.projectId === projectId)
    if (!project) throw new Error('Cloud project is unavailable.')
    if (project.status === 'paused') {
      const epoch = lifecycleEpoch
      const registry = await requireRuntime().setProjectPaused(projectId, false)
      if (current(epoch)) await applyRegistry(registry, epoch)
      return
    }
    if (project.mode === 'import_remote') {
      if (!project.name) throw new Error('Imported project name is unavailable.')
      return importProject(projectId, project.name)
    }
    return bootstrapProject(projectId)
  }

  async function pauseProject(projectId: string): Promise<void> {
    const epoch = lifecycleEpoch
    stopRecovery()
    readyNoteProjects.delete(projectId)
    const registry = await requireRuntime().setProjectPaused(projectId, true)
    if (current(epoch)) await applyRegistry(registry, epoch)
  }

  async function retry(correlation?: string, requiredRecovery?: number): Promise<void> {
    if (!hasProvisionedKey.value) throw new Error('Encryption provisioning is required before sync.')
    if (!unlocked || !authenticated.value) throw new Error('Cloud session must be unlocked before sync.')
    await diagnostics.run('sync', 'retry', () => runCycle(() => requireRuntime().retry(), correlation, requiredRecovery), correlation)
  }

  function queueNoteSync(projectId: string): void {
    if (!unlocked || !authenticated.value) return
    // The durable local outbox will be scanned by the already scheduled retry.
    if (recoveryTimer !== null) return
    const epoch = lifecycleEpoch
    pendingNoteProjects.add(projectId)
    if (noteFlight?.epoch === epoch) return
    let promise!: Promise<void>
    promise = Promise.resolve().then(async () => {
      try {
        while (current(epoch) && unlocked && pendingNoteProjects.size) {
          if (recoveryTimer !== null) {
            // These writes are already durable and will be included in recovery.
            pendingNoteProjects.clear()
            return
          }
          // Wait for any earlier work before consuming the batch: a write may have
          // committed after that cycle's outbox scan and requires a fresh cycle.
          while (current(epoch)) {
            const prior = projectFlight?.epoch === epoch ? projectFlight.promise
              : syncFlight?.epoch === epoch ? syncFlight.promise : null
            if (!prior) break
            await prior.catch(() => undefined)
          }
          if (!current(epoch) || !unlocked) return
          if (recoveryTimer !== null) {
            pendingNoteProjects.clear()
            return
          }
          const batch = [...pendingNoteProjects]
          pendingNoteProjects.clear()
          const connected = batch.some(id => readyNoteProjects.has(id))
          if (!connected || !canRunCycle.value) continue
          try { await retry() } catch { /* runCycle exposes the safe error state */ }
        }
      } finally {
        if (noteFlight?.promise === promise) noteFlight = null
      }
    })
    noteFlight = { epoch, promise }
  }

  async function inspectCatalog():Promise<void>{const epoch=lifecycleEpoch;const runtime=requireRuntime();if(!runtime.catalogAuthority)throw new Error('catalog_runtime_unavailable');const view=await runtime.catalogAuthority();if(current(epoch))catalogAuthority.value=view}
  async function beginCatalog():Promise<void>{await diagnostics.run('migrations','catalog_migration',async correlation=>{const epoch=lifecycleEpoch;const runtime=requireRuntime();if(!runtime.beginCatalogMigration)throw new Error('catalog_runtime_unavailable');await runtime.beginCatalogMigration();if(!current(epoch))return;await retry(correlation);if(!current(epoch))return;await inspectCatalog();announceDataChange('projects')})}
  async function decideCatalog(decision:CatalogDecision):Promise<void>{await diagnostics.run('projects','conflict_resolution',async correlation=>{const epoch=lifecycleEpoch;const runtime=requireRuntime();if(!runtime.decideCatalog)throw new Error('catalog_runtime_unavailable');await runtime.decideCatalog(decision);if(!current(epoch))return;await retry(correlation);if(!current(epoch))return;await inspectCatalog();announceDataChange('projects')})}

  const noteConflicts=ref<Record<string,ContentNoteConflict[]>>({})
  const progressAuthority=ref<Record<string,ProgressAuthorityView>>({})
  const gameAuthority=ref<GameAuthorityView|null>(null)
  async function inspectGame():Promise<void>{const epoch=lifecycleEpoch;const runtime=requireRuntime();if(!runtime.gameAuthority)throw new Error('game_codec_not_activated');const view=await runtime.gameAuthority();if(current(epoch))gameAuthority.value=view}
  async function beginGame():Promise<void>{const epoch=lifecycleEpoch;const runtime=requireRuntime();if(!runtime.beginGameMigration)throw new Error('game_codec_not_activated');await runtime.beginGameMigration();if(!current(epoch))return;await retry();if(current(epoch))await inspectGame()}
  async function chooseGameHistory(decision:GameDecision):Promise<void>{const epoch=lifecycleEpoch;const runtime=requireRuntime();if(!runtime.chooseGameHistory)throw new Error('game_codec_not_activated');await runtime.chooseGameHistory(decision);if(!current(epoch))return;await retry();if(current(epoch))await inspectGame()}
  async function compensateGameReward(decision:GameCompensation):Promise<void>{const epoch=lifecycleEpoch;const runtime=requireRuntime();if(!runtime.compensateGameReward)throw new Error('game_codec_not_activated');await runtime.compensateGameReward(decision);if(!current(epoch))return;await retry();if(current(epoch))await inspectGame()}
  async function rebuildGameProjection(ownerKey:string):Promise<void>{const epoch=lifecycleEpoch;const runtime=requireRuntime();if(!runtime.rebuildGameProjection)throw new Error('game_codec_not_activated');await runtime.rebuildGameProjection(ownerKey);if(current(epoch))await inspectGame()}
  async function inspectProgress(projectId:string):Promise<void>{const epoch=lifecycleEpoch;const runtime=requireRuntime();if(!runtime.projectProgressAuthority)throw new Error('progress_runtime_unavailable');const view=await runtime.projectProgressAuthority(projectId);if(current(epoch))progressAuthority.value={...progressAuthority.value,[projectId]:view}}
  async function beginProgress(projectId:string):Promise<void>{await diagnostics.run('migrations','progress_migration',async correlation=>{const epoch=lifecycleEpoch;const runtime=requireRuntime();if(!runtime.beginProgressMigration)throw new Error('progress_runtime_unavailable');await runtime.beginProgressMigration(projectId);if(!current(epoch))return;await retry(correlation);if(current(epoch))await inspectProgress(projectId)})}
  async function chooseProgressHistory(decision:ProgressDecision):Promise<void>{const epoch=lifecycleEpoch;const runtime=requireRuntime();if(!runtime.chooseProgressHistory)throw new Error('progress_runtime_unavailable');await runtime.chooseProgressHistory(decision);if(!current(epoch))return;await retry();if(!current(epoch))return;await inspectProgress(decision.project_id);if(current(epoch))announceDataChange('projects')}
  const documentAuthority=ref<Record<string,DocumentAuthorityView>>({})
  async function inspectDocuments(projectId:string):Promise<void>{const epoch=lifecycleEpoch;const runtime=requireRuntime();if(!runtime.projectDocumentAuthority)throw new Error('document_runtime_unavailable');const view=await runtime.projectDocumentAuthority(projectId);if(current(epoch))documentAuthority.value={...documentAuthority.value,[projectId]:view}}
  async function beginDocuments(projectId:string):Promise<void>{await diagnostics.run('migrations','document_migration',async correlation=>{const epoch=lifecycleEpoch;const runtime=requireRuntime();if(!runtime.beginDocumentMigration)throw new Error('document_runtime_unavailable');await runtime.beginDocumentMigration(projectId);if(!current(epoch))return;await retry(correlation);if(current(epoch))await inspectDocuments(projectId)})}
  async function chooseDocumentVersion(decision:DocumentDecision):Promise<void>{const epoch=lifecycleEpoch;const runtime=requireRuntime();if(!runtime.chooseDocumentVersion)throw new Error('document_runtime_unavailable');await runtime.chooseDocumentVersion(decision);if(!current(epoch))return;await retry();if(!current(epoch))return;await inspectDocuments(decision.project_id);if(current(epoch))announceDataChange('projects')}
  async function moveDocument(projectId:string,id:string,stage:string|null,expected:unknown):Promise<void>{const epoch=lifecycleEpoch;const runtime=requireRuntime();if(!runtime.moveDocument)throw new Error('document_runtime_unavailable');await runtime.moveDocument(projectId,id,stage,expected);if(!current(epoch))return;await retry();if(!current(epoch))return;await inspectDocuments(projectId);if(current(epoch))announceDataChange('projects')}
  async function deleteDocument(projectId:string,id:string,expected:unknown):Promise<void>{const epoch=lifecycleEpoch;const runtime=requireRuntime();if(!runtime.deleteDocument)throw new Error('document_runtime_unavailable');await runtime.deleteDocument(projectId,id,expected);if(!current(epoch))return;await retry();if(!current(epoch))return;await inspectDocuments(projectId);if(current(epoch))announceDataChange('projects')}
  const mapAuthority=ref<Record<string,MapAuthorityView>>({})
  async function inspectMaps(projectId:string):Promise<void>{const epoch=lifecycleEpoch;const runtime=requireRuntime();if(!runtime.projectMapAuthority)throw new Error('map_runtime_unavailable');const view=await runtime.projectMapAuthority(projectId);if(current(epoch))mapAuthority.value={...mapAuthority.value,[projectId]:view}}
  async function beginMaps(projectId:string):Promise<void>{await diagnostics.run('migrations','map_migration',async correlation=>{const epoch=lifecycleEpoch;const runtime=requireRuntime();if(!runtime.beginMapMigration)throw new Error('map_runtime_unavailable');await runtime.beginMapMigration(projectId);if(!current(epoch))return;await retry(correlation);if(current(epoch))await inspectMaps(projectId)})}
  async function chooseMapVersion(decision:MapDecision,keepLocal?:boolean):Promise<void>{await diagnostics.run('sync','map_resolution',async()=>{const epoch=lifecycleEpoch;const runtime=requireRuntime();if(!runtime.chooseMapVersion)throw new Error('map_runtime_unavailable');await runtime.chooseMapVersion(decision,keepLocal);if(!current(epoch))return;await retry();if(!current(epoch))return;await inspectMaps(decision.project_id);if(current(epoch))announceDataChange('projects')})}
  const noteAuthority=ref<Record<string,ContentNoteMigrationView>>({})
  async function inspectNotes(projectId:string):Promise<void>{const epoch=lifecycleEpoch;const runtime=requireRuntime();if(!runtime.projectNoteAuthority)throw new Error('content_note_runtime_unavailable');const view=await runtime.projectNoteAuthority(projectId);if(current(epoch)){noteAuthority.value={...noteAuthority.value,[projectId]:view};if(runtime.noteConflicts){const groups=await runtime.noteConflicts(projectId);if(current(epoch))noteConflicts.value={...noteConflicts.value,[projectId]:groups}}}}
  async function chooseNoteVersion(projectId:string,decision:ContentNoteConflict,selected:string):Promise<void>{const runtime=requireRuntime();if(!runtime.chooseNoteVersion)throw new Error('content_note_runtime_unavailable');await diagnostics.run('sync','conflict_resolution',async()=>{const epoch=lifecycleEpoch;await runtime.chooseNoteVersion!(projectId,decision,selected);if(!current(epoch))return;await retry();if(!current(epoch))return;await inspectNotes(projectId);if(current(epoch))announceDataChange('projects')})}
  async function beginNotes(projectId:string):Promise<void>{await diagnostics.run('migrations','note_migration',async correlation=>{const epoch=lifecycleEpoch;const runtime=requireRuntime();if(!runtime.beginNoteMigration)throw new Error('content_note_runtime_unavailable');await runtime.beginNoteMigration(projectId);if(!current(epoch))return;await retry(correlation);if(current(epoch))await inspectNotes(projectId)})}

  async function inspectStructure(projectId: string): Promise<void> {
    const epoch = lifecycleEpoch
    const view = await requireRuntime().projectStructuralAuthority(projectId)
    if (current(epoch)) structuralAuthority.value = { ...structuralAuthority.value, [projectId]: view }
  }
  async function beginStructure(projectId: string): Promise<void> {
    await diagnostics.run('migrations', 'stage_migration', async correlation => {
      const epoch = lifecycleEpoch
      await requireRuntime().beginStageMigration(projectId)
      if (!current(epoch)) return
      await retry(correlation)
      if (!current(epoch)) return
      await inspectStructure(projectId)
      announceDataChange('projects')
    })
  }
  async function decideStructure(projectId: string, decision: StructuralDecision): Promise<void> {
    await diagnostics.run('stages', 'conflict_resolution', async correlation => {
      const epoch = lifecycleEpoch
      await requireRuntime().decideStructure(projectId, decision)
      if (!current(epoch)) return
      await retry(correlation)
      if (!current(epoch)) return
      await inspectStructure(projectId)
      announceDataChange('projects')
    })
  }

  async function inspectProjectMetadata(projectId: string): Promise<void> {
    const epoch = lifecycleEpoch
    const activeRuntime = requireRuntime()
    const [mode, authority] = await Promise.all([
      activeRuntime.metadataTransportMode(), activeRuntime.projectMetadataAuthority(projectId),
    ])
    if (!current(epoch)) return
    metadataTransportMode.value = mode
    metadataAuthority.value = { ...metadataAuthority.value, [projectId]: authority }
  }

  async function prepareMetadataTransport(): Promise<void> {
    const epoch = lifecycleEpoch
    await requireRuntime().prepareMetadataTransport()
    const mode = await requireRuntime().metadataTransportMode()
    if (current(epoch)) metadataTransportMode.value = mode
  }

  async function declareMetadataReaderReady(): Promise<void> {
    await requireRuntime().declareMetadataReaderReady()
  }

  async function cutoverMetadataTransport(): Promise<void> {
    const epoch = lifecycleEpoch
    await requireRuntime().cutoverMetadataTransport()
    if (current(epoch)) metadataTransportMode.value = 3
  }

  /** A normal saved cover starts one bounded cycle; it never captures a legacy source. */
  async function syncSavedCover(projectId: string): Promise<void> {
    if (!supported.value || !hasProvisionedKey.value || !canRunCycle.value
      || !['ready', 'syncing', 'completed', 'retryable_error', 'blocked', 'remaining_work'].includes(status.value)
      || !runtime?.projectCoverAuthority) return
    const epoch = lifecycleEpoch
    try {
      if (syncFlight?.epoch === epoch) await syncFlight.promise
      if (!current(epoch) || !runtime?.projectCoverAuthority) return
      const view = await runtime.projectCoverAuthority(projectId)
      if (!current(epoch)) return
      coverAuthority.value = { ...coverAuthority.value, [projectId]: view }
      if (view.active && view.pending) await retry()
    } catch (error) {
      // A local save has already committed. Keep network failure in the cloud
      // status while the exact native transfer remains durable for retry.
      setFailure(error, epoch)
    }
  }

  async function coverPreview(projectId:string,reference:import('@/cloud/projectCoverReference').ProjectCoverReference):Promise<string|null>{
    const epoch=lifecycleEpoch,runtime=requireRuntime();if(!runtime.projectCoverPreview)return null
    const result=await runtime.projectCoverPreview(projectId,reference);return current(epoch)?result:null
  }
  async function inspectCover(projectId:string):Promise<void>{
    const epoch=lifecycleEpoch,runtime=requireRuntime()
    if(!runtime.projectCoverAuthority)return
    const view=await runtime.projectCoverAuthority(projectId)
    if(current(epoch))coverAuthority.value={...coverAuthority.value,[projectId]:view}
  }
  async function beginCover(projectId:string):Promise<void>{
    await diagnostics.run('migrations','metadata_migration',async correlation=>{
      const epoch=lifecycleEpoch,runtime=requireRuntime()
      if(!runtime.beginCoverMigration)throw new TypeError('cover_runtime_unavailable')
      await runtime.beginCoverMigration(projectId)
      if(!current(epoch))return
      await inspectCover(projectId);await retry(correlation);await inspectCover(projectId);await inspectProjectMetadata(projectId)
    })
  }

  async function beginMetadataMigration(projectId: string): Promise<void> {
    await diagnostics.run('migrations', 'metadata_migration', async correlation => {
      const epoch = lifecycleEpoch
      await requireRuntime().beginProjectMetadataMigration(projectId)
      if (!current(epoch)) return
      await inspectProjectMetadata(projectId)
      await retry(correlation)
      await inspectProjectMetadata(projectId)
    })
  }

  async function adoptMetadata(projectId: string, expectedHead: string, expectedLocal: ProjectMetadata): Promise<void> {
    const epoch = lifecycleEpoch
    await requireRuntime().adoptProjectMetadata(projectId, expectedHead, expectedLocal)
    if (!current(epoch)) return
    announceDataChange('projects')
    await refreshProjects()
    await inspectProjectMetadata(projectId)
  }

  async function decideMetadata(projectId: string, kind: MetadataDecisionKind, selectedEventId: string | null,
    proposed: ProjectMetadata | null, expectedLocal: ProjectMetadata, expectedTips: string[]): Promise<void> {
    await diagnostics.run('projects', 'conflict_resolution', async correlation => {
      const epoch = lifecycleEpoch
      await requireRuntime().decideProjectMetadata(projectId, kind, selectedEventId, proposed, expectedLocal, expectedTips)
      if (!current(epoch)) return
      announceDataChange('projects')
      await inspectProjectMetadata(projectId)
      await retry(correlation)
      await inspectProjectMetadata(projectId)
    })
  }

  async function lock(): Promise<void> {
    const epoch = ++lifecycleEpoch
    stopRecovery()
    unlocked = false
    pendingNoteProjects.clear()
    readyNoteProjects.clear()
    clearPending()
    try {
      await requireRuntime().lock()
    } finally {
      if (current(epoch)) {
        blockedEvents.value = []
        blockedEventCodes.value = []
        hasRemainingWork.value = false
        errorMessage.value = null
        projects.value = []
        canRunCycle.value = false
        busy.value = false
        if (username.value !== null) status.value = 'key_locked'
      }
    }
  }

  async function logout(): Promise<void> {
    const epoch = ++lifecycleEpoch
    stopRecovery()
    unlocked = false
    pendingNoteProjects.clear()
    readyNoteProjects.clear()
    clearPending()
    try {
      await runtime?.logout()
    } finally {
      if (current(epoch)) {
        resetVisibleSession()
        busy.value = false
        status.value = supported.value ? 'logged_out' : 'unavailable'
      }
    }
  }

  async function dispose(): Promise<void> {
    ++lifecycleEpoch
    stopRecovery()
    unlocked = false
    pendingNoteProjects.clear()
    readyNoteProjects.clear()
    const listener = noteListener
    noteListener = null
    if (listener) void listener.then(unlisten => unlisten()).catch(() => undefined)
    clearPending()
    syncFlight = null
    projectFlight = null
    const activeRuntime = runtime
    runtime = null
    await activeRuntime?.dispose()
    resetVisibleSession()
    busy.value = false
    status.value = supported.value ? 'logged_out' : 'unavailable'
  }

  return {
    supported, status, username, hasProvisionedKey, errorMessage, errorCode, blockedEvents, blockedEventCodes,
    hasRemainingWork, lastCycleAt, busy, authenticated, projects, canRunCycle,
    catalogAuthority,inspectCatalog,beginCatalog,decideCatalog,
    coverAuthority,inspectCover,beginCover,coverPreview,syncSavedCover,
    metadataTransportMode, metadataAuthority, structuralAuthority, inspectStructure, beginStructure, decideStructure,
    projectBootstrapEnabled,
    initialize, login, prepareProvisioning, submitProvisioning, reconcileProvisioning,
    cancelProvisioning, unlock, refreshProjects, prepareProjectConnection,
    bootstrapProject, importProject, resumeProject, pauseProject,
    progressAuthority,inspectProgress,beginProgress,chooseProgressHistory,
    gameAuthority,inspectGame,beginGame,chooseGameHistory,compensateGameReward,rebuildGameProjection,
    documentAuthority,inspectDocuments,beginDocuments,chooseDocumentVersion,moveDocument,deleteDocument,
    mapAuthority,inspectMaps,beginMaps,chooseMapVersion,
    noteAuthority,noteConflicts,inspectNotes,beginNotes,chooseNoteVersion,
    retry, inspectProjectMetadata, prepareMetadataTransport, declareMetadataReaderReady, cutoverMetadataTransport,
    beginMetadataMigration, adoptMetadata, decideMetadata, lock, logout, dispose,
  }
})
