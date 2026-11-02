import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'

vi.mock('@/platform/runtime', () => ({ currentPlatform: vi.fn(() => 'tauri') }))
vi.mock('@/platform/desktopCloud', () => ({ desktopCloudEnabled: vi.fn(() => true) }))
const bridge = vi.hoisted(() => ({ invoke: vi.fn(), listeners: new Set<(event: { payload: unknown }) => void>() }))
vi.mock('@tauri-apps/api/core', () => ({ invoke: bridge.invoke }))
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async (_name: string, listener: (event: { payload: unknown }) => void) => {
    bridge.listeners.add(listener)
    return () => bridge.listeners.delete(listener)
  }),
  emit: vi.fn(async (_name: string, payload: unknown) => {
    for (const listener of bridge.listeners) listener({ payload })
  }),
}))

import type { CurrentUserCryptoRecord } from '@/api/accountCrypto'
import { diagnostics } from '@/diagnostics/service'
import { ApiError } from '@/api/client'
import { currentPlatform } from '@/platform/runtime'
import { SQLiteNotesRepository } from '@/infrastructure/sqlite/notesRepository'
import { announceDataChange } from '@/services/dataChanges'
import type { PendingAccountCryptoProvisioning } from '@/cloud/accountCryptoProvisioning'
import { AccountCryptoProvisioningConflictError } from '@/cloud/accountCryptoProvisioning'
import type { NoteSyncProductionResult } from '@/cloud/noteSyncTransportRouter'
import type { CloudProjectBootstrapProgress, CloudRegistryReconciliation } from '@/cloud/projectBootstrap'
import type { CloudProjectBootstrapRecord } from '@/infrastructure/sqlite/cloudProjectBootstrapRepository'
import {
  configureCloudSessionProjectLoaderForTests,
  configureCloudSessionRuntimeFactoryForTests,
  useCloudSessionStore,
  type CloudSessionRuntime,
} from './cloudSession'

const PROVISIONED: CurrentUserCryptoRecord = {
  provisioned: true,
  password: {
    crypto_version: 1, wrapping_version: 1,
    kdf: { kdf_version: 1, algorithm: 'argon2id13', salt: 'salt', opslimit: 2, memlimit: 67108864 },
    nonce: 'nonce', ciphertext: 'ciphertext',
  },
  recovery: { crypto_version: 1, wrapping_version: 1, nonce: 'recovery-nonce', ciphertext: 'recovery-ciphertext' },
}
const UNPROVISIONED: CurrentUserCryptoRecord = { provisioned: false, password: null, recovery: null }
const V1_CYCLE = {
  stages: [], sealed: [], uploaded: 0, pulled: [], applied: [], blocked: [], errors: [], hasRemainingWork: false,
}
const EMPTY_CYCLE: NoteSyncProductionResult = {
  transport_version: 1, cutover_epoch: 0, cycle: V1_CYCLE,
  hasRemainingWork: false, blocked: [], errors: [],
}
const READY_REGISTRY = { readyForCycle: true, readyForNormalCycle: true, reasons: [], remote: [], local: [], currentCursor: 0 }
const PROJECT = '123e4567-e89b-42d3-a456-426614174010'
const DEVICE = '123e4567-e89b-42d3-a456-426614174001'
const TOKEN = '123e4567-e89b-42d3-a456-426614174020'

function bootstrapRecord(phase: CloudProjectBootstrapRecord['phase'] = 'ready', mode: CloudProjectBootstrapRecord['mode'] = 'upload_existing'): CloudProjectBootstrapRecord {
  return {
    project_id: PROJECT, account_id: 'local-account', device_id: DEVICE,
    bootstrap_id: TOKEN, mode, phase, remote_state: 'active',
    initial_event_count: 1, initial_local_ordinal_hi: 1, remote_high_water: 1,
    initial_max_server_sequence: 1, blocked_reason: null,
  }
}

function registry(options: Partial<CloudRegistryReconciliation> = {}): CloudRegistryReconciliation {
  return {
    readyForCycle: true, readyForNormalCycle: true, reasons: [], remote: [], local: [], currentCursor: 0,
    ...options,
  }
}

function progress(value: CloudRegistryReconciliation, record = bootstrapRecord()): CloudProjectBootstrapProgress {
  return { project: record, registry: value, cycle: EMPTY_CYCLE, hasRemainingWork: false }
}

function pending(): PendingAccountCryptoProvisioning {
  return {
    recoveryKeyForDisplay: vi.fn(() => new Uint8Array([1, 2, 3])),
    confirmRecoveryKeySaved: vi.fn(),
    dispose: vi.fn(),
  } as unknown as PendingAccountCryptoProvisioning
}

function deferred<T>() {
  let resolve!: (value: T) => void
  const promise = new Promise<T>(done => { resolve = done })
  return { promise, resolve }
}

function runtime(record: CurrentUserCryptoRecord = PROVISIONED): CloudSessionRuntime {
  const prepared = pending()
  return {
    login: vi.fn().mockResolvedValue({ context: { username: 'normal-user' } }),
    cryptoRecord: vi.fn().mockResolvedValue(record),
    beginCryptoProvisioning: vi.fn().mockResolvedValue(prepared),
    submitCryptoProvisioning: vi.fn().mockResolvedValue(PROVISIONED),
    reconcileCryptoProvisioning: vi.fn().mockResolvedValue(null),
    unlock: vi.fn().mockResolvedValue({ identity: {}, registry: READY_REGISTRY }),
    reconcileProjects: vi.fn().mockResolvedValue(READY_REGISTRY),
    preflightLocalProject: vi.fn().mockResolvedValue([]),
    bootstrapLocalProject: vi.fn(),
    importRemoteProject: vi.fn(),
    setProjectPaused: vi.fn().mockResolvedValue(READY_REGISTRY),
    projectStructuralAuthority: vi.fn(), beginStageMigration: vi.fn(), decideStructure: vi.fn(),
    metadataTransportMode: vi.fn().mockResolvedValue(3),
    prepareMetadataTransport: vi.fn(), declareMetadataReaderReady: vi.fn(), cutoverMetadataTransport: vi.fn(),
    projectMetadataAuthority: vi.fn(), beginProjectMetadataMigration: vi.fn(),
    adoptProjectMetadata: vi.fn(), decideProjectMetadata: vi.fn(),
    retry: vi.fn().mockResolvedValue(EMPTY_CYCLE),
    lock: vi.fn().mockResolvedValue(undefined),
    logout: vi.fn().mockResolvedValue(undefined),
    dispose: vi.fn().mockResolvedValue(undefined),
  }
}

describe('desktop cloud session owner', () => {
  beforeEach(() => {
    bridge.listeners.clear()
    bridge.invoke.mockReset()
    setActivePinia(createPinia())
    configureCloudSessionRuntimeFactoryForTests(null)
    configureCloudSessionProjectLoaderForTests(async () => [])
    vi.mocked(currentPlatform).mockReturnValue('tauri')
  })

  afterEach(() => vi.useRealTimers())

  async function noteSetup() {
    const instance = runtime()
    const connected = registry({ local: [bootstrapRecord()] })
    vi.mocked(instance.unlock).mockResolvedValue({ identity: {} as never, registry: connected })
    vi.mocked(instance.reconcileProjects).mockResolvedValue(connected)
    configureCloudSessionProjectLoaderForTests(async () => [{ id: PROJECT, name: 'Project' }, { id: 'local-only', name: 'Local' }])
    configureCloudSessionRuntimeFactoryForTests(() => instance)
    const store = useCloudSessionStore(); store.initialize(); await store.login('user', 'password'); await store.unlock('key')
    await vi.waitFor(() => expect(store.status).toBe('completed'))
    vi.mocked(instance.retry).mockClear()
    return { instance, store, repository: new SQLiteNotesRepository(), connected }
  }

  it('preserves a saved Note and retries bounded network failures until recovery', async () => {
    const { instance, store, repository } = await noteSetup()
    vi.useFakeTimers()
    let localText = ''
    bridge.invoke.mockImplementation(async (_command: string, args: { patch: { content: string } }) => {
      localText = args.patch.content
      return { id: 'n', content: localText }
    })
    const offline = new ApiError(0, 'network_error', 'offline')
    vi.mocked(instance.retry).mockRejectedValueOnce(offline).mockRejectedValueOnce(offline).mockResolvedValueOnce(EMPTY_CYCLE)
    await repository.update({ projectId: PROJECT }, 'n', { content: 'saved offline' })
    await vi.advanceTimersByTimeAsync(0)
    expect(localText).toBe('saved offline')
    expect(store.status).toBe('retryable_error')
    expect(store.errorMessage).toBeTruthy()
    expect(instance.retry).toHaveBeenCalledTimes(1)
    await vi.advanceTimersByTimeAsync(299_999)
    expect(instance.retry).toHaveBeenCalledTimes(1)
    await vi.advanceTimersByTimeAsync(1)
    expect(instance.retry).toHaveBeenCalledTimes(2)
    await vi.advanceTimersByTimeAsync(299_999)
    expect(instance.retry).toHaveBeenCalledTimes(2)
    await vi.advanceTimersByTimeAsync(1)
    expect(instance.retry).toHaveBeenCalledTimes(3)
    expect(store.status).toBe('completed')
    expect(localText).toBe('saved offline')
    await vi.advanceTimersByTimeAsync(600_000)
    expect(instance.retry).toHaveBeenCalledTimes(3)
    await store.dispose()
  })

  it('backs off returned network stage failures without parallel cycles or extra attempts on saves', async () => {
    const { instance, store, repository } = await noteSetup()
    vi.useFakeTimers()
    bridge.invoke.mockResolvedValue({ id: 'n', content: 'saved' })
    const networkResult = { ...EMPTY_CYCLE, errors: [{ stage: 'pull', code: 'network_error' }], hasRemainingWork: true }
    const inFlight = deferred<NoteSyncProductionResult>()
    vi.mocked(instance.retry).mockResolvedValueOnce(networkResult).mockReturnValueOnce(inFlight.promise)
    await repository.update({ projectId: PROJECT }, 'n', { content: 'one' })
    await vi.advanceTimersByTimeAsync(0)
    expect(store.status).toBe('retryable_error')
    expect(store.errorMessage).toBeTruthy()
    await repository.update({ projectId: PROJECT }, 'n', { content: 'two' })
    await vi.advanceTimersByTimeAsync(299_999)
    expect(instance.retry).toHaveBeenCalledTimes(1)
    await vi.advanceTimersByTimeAsync(1)
    expect(instance.retry).toHaveBeenCalledTimes(2)
    await vi.advanceTimersByTimeAsync(600_000)
    expect(instance.retry).toHaveBeenCalledTimes(2)
    inFlight.resolve(EMPTY_CYCLE)
    await vi.advanceTimersByTimeAsync(0)
    expect(store.status).toBe('completed')
    await store.dispose()
  })

  it.each(['lock', 'logout', 'pause'] as const)('cancels a scheduled recovery on %s', async action => {
    const { instance, store } = await noteSetup()
    vi.useFakeTimers()
    vi.mocked(instance.retry).mockRejectedValueOnce(new ApiError(0, 'network_error', 'offline'))
    await expect(store.retry()).rejects.toBeDefined()
    if (action === 'lock') await store.lock()
    else if (action === 'logout') await store.logout()
    else await store.pauseProject(PROJECT)
    await vi.advanceTimersByTimeAsync(600_000)
    expect(instance.retry).toHaveBeenCalledTimes(1)
    await store.dispose()
  })

  it('cancels a recovery already waiting behind bootstrap when sync is paused', async () => {
    const { instance, store, connected } = await noteSetup()
    vi.useFakeTimers()
    vi.mocked(instance.retry).mockRejectedValueOnce(new ApiError(0, 'network_error', 'offline'))
    await expect(store.retry()).rejects.toBeDefined()
    const bootstrap = deferred<CloudProjectBootstrapProgress>()
    vi.mocked(instance.bootstrapLocalProject).mockReturnValueOnce(bootstrap.promise)
    const project = store.bootstrapProject(PROJECT)
    await vi.advanceTimersByTimeAsync(0)
    expect(instance.bootstrapLocalProject).toHaveBeenCalledTimes(1)
    await vi.advanceTimersByTimeAsync(300_000)
    await store.pauseProject(PROJECT)
    bootstrap.resolve(progress(connected))
    await project
    await vi.advanceTimersByTimeAsync(0)
    expect(instance.retry).toHaveBeenCalledTimes(1)
    await store.dispose()
  })

  it.each([
    new ApiError(401, 'invalid_token', 'unauthorized'),
    new ApiError(503, 'note_readers_not_ready', 'blocked'),
    new Error('corrupt data'),
  ])('does not auto-retry authentication, capability or data failures', async error => {
    const { instance, store } = await noteSetup()
    vi.useFakeTimers()
    vi.mocked(instance.retry).mockRejectedValueOnce(error)
    await expect(store.retry()).rejects.toBeDefined()
    await vi.advanceTimersByTimeAsync(600_000)
    expect(instance.retry).toHaveBeenCalledTimes(1)
    await store.dispose()
  })

  it('does not retry a network result mixed with a security blocker', async () => {
    const { instance, store } = await noteSetup()
    vi.useFakeTimers()
    vi.mocked(instance.retry).mockResolvedValueOnce({ ...EMPTY_CYCLE, blocked: ['capability_blocked'], errors: [{ stage: 'pull', code: 'network_error' }] })
    await store.retry()
    await vi.advanceTimersByTimeAsync(600_000)
    expect(instance.retry).toHaveBeenCalledTimes(1)
    expect(store.status).toBe('blocked')
    await store.dispose()
  })

  it.each(['invalid_token', 'malformed_receipt', 'note_readers_not_ready'])('does not retry mixed network and %s stage errors', async code => {
    const { instance, store } = await noteSetup()
    vi.useFakeTimers()
    vi.mocked(instance.retry).mockResolvedValueOnce({ ...EMPTY_CYCLE, errors: [{ stage: 'pull', code: 'network_error' }, { stage: 'apply', code }] })
    await store.retry()
    await vi.advanceTimersByTimeAsync(600_000)
    expect(instance.retry).toHaveBeenCalledTimes(1)
    await store.dispose()
  })

  it('makes at most one attempt per five minutes during a long outage', async () => {
    const { instance, store } = await noteSetup()
    vi.useFakeTimers()
    vi.mocked(instance.retry).mockRejectedValue(new ApiError(0, 'network_error', 'offline'))
    await expect(store.retry()).rejects.toBeDefined()
    await vi.advanceTimersByTimeAsync(3_600_000)
    expect(instance.retry).toHaveBeenCalledTimes(13)
    await store.logout()
    await vi.advanceTimersByTimeAsync(600_000)
    expect(instance.retry).toHaveBeenCalledTimes(13)
    await store.dispose()
  })

  it('defers saves made during a failing cycle to the scheduled recovery', async () => {
    const { instance, store, repository } = await noteSetup()
    vi.useFakeTimers()
    bridge.invoke.mockResolvedValue({ id: 'n', content: 'saved' })
    let reject!: (error: Error) => void
    vi.mocked(instance.retry).mockReturnValueOnce(new Promise((_, fail) => { reject = fail })).mockResolvedValueOnce(EMPTY_CYCLE)
    await repository.update({ projectId: PROJECT }, 'n', { content: 'one' })
    await vi.advanceTimersByTimeAsync(0)
    expect(instance.retry).toHaveBeenCalledTimes(1)
    await repository.update({ projectId: PROJECT }, 'n', { content: 'two' })
    reject(new ApiError(0, 'network_error', 'offline'))
    await vi.advanceTimersByTimeAsync(0)
    expect(instance.retry).toHaveBeenCalledTimes(1)
    await vi.advanceTimersByTimeAsync(300_000)
    expect(instance.retry).toHaveBeenCalledTimes(2)
    expect(store.status).toBe('completed')
    await store.dispose()
  })

  it('starts a cycle only after a successful local note command; failed and incoming changes do not trigger it', async () => {
    const { instance, store, repository } = await noteSetup()
    const write = deferred<{ id: string, content: string }>()
    bridge.invoke.mockReturnValueOnce(write.promise)
    const saved = repository.update({ projectId: PROJECT }, 'n', { content: 'local text' })
    await Promise.resolve()
    expect(instance.retry).not.toHaveBeenCalled()
    write.resolve({ id: 'n', content: 'local text' })
    expect(await saved).toMatchObject({ content: 'local text' })
    await vi.waitFor(() => expect(instance.retry).toHaveBeenCalledTimes(1))
    await vi.waitFor(() => expect(store.status).toBe('completed'))
    bridge.invoke.mockRejectedValueOnce(new Error('write failed'))
    await expect(repository.update({ projectId: PROJECT }, 'n', { content: 'lost' })).rejects.toThrow('write failed')
    announceDataChange('projects')
    await Promise.resolve(); await Promise.resolve()
    expect(instance.retry).toHaveBeenCalledTimes(1)
    await store.dispose()
  })

  it('coalesces writes during a cycle into a trailing cycle and preserves committed text on network failure', async () => {
    const { instance, store, repository } = await noteSetup()
    let text = ''
    bridge.invoke.mockImplementation(async (_command: string, args: { patch: { content: string } }) => {
      text = args.patch.content
      return { id: 'n', content: text }
    })
    const firstCycle = deferred<NoteSyncProductionResult>()
    vi.mocked(instance.retry).mockReturnValueOnce(firstCycle.promise).mockRejectedValueOnce(new ApiError(0, 'offline', 'offline'))
    await repository.update({ projectId: PROJECT }, 'n', { content: 'one' })
    await vi.waitFor(() => expect(instance.retry).toHaveBeenCalledTimes(1))
    await Promise.all(['two', 'three', 'four'].map(content => repository.update({ projectId: PROJECT }, 'n', { content })))
    expect(instance.retry).toHaveBeenCalledTimes(1)
    firstCycle.resolve(EMPTY_CYCLE)
    await vi.waitFor(() => expect(instance.retry).toHaveBeenCalledTimes(2))
    await vi.waitFor(() => expect(store.status).toBe('retryable_error'))
    expect(text).toBe('four')
    expect(store.errorMessage).toBeTruthy()
    await store.dispose()
  })

  it('does not schedule a local-only or locked project, and cancels queued saves on logout', async () => {
    const { instance, store, repository } = await noteSetup()
    bridge.invoke.mockResolvedValue({ id: 'n', content: 'saved' })
    await repository.update({ projectId: 'local-only' }, 'n', { content: 'saved' })
    await Promise.resolve(); await Promise.resolve()
    expect(instance.retry).not.toHaveBeenCalled()
    const active = deferred<NoteSyncProductionResult>()
    vi.mocked(instance.retry).mockReturnValueOnce(active.promise)
    const manual = store.retry()
    await vi.waitFor(() => expect(instance.retry).toHaveBeenCalledTimes(1))
    await repository.update({ projectId: PROJECT }, 'n', { content: 'saved' })
    await store.logout(); active.resolve(EMPTY_CYCLE); await manual
    await Promise.resolve(); await Promise.resolve()
    expect(instance.retry).toHaveBeenCalledTimes(1)
    await store.login('user', 'password')
    await repository.update({ projectId: PROJECT }, 'n', { content: 'locked' })
    await Promise.resolve(); await Promise.resolve()
    expect(instance.retry).toHaveBeenCalledTimes(1)
    await store.dispose()
  })

  it.each(['bootstrap', 'import'] as const)('runs a committed note save after %s completes', async operation => {
    const { instance, store, repository, connected } = await noteSetup()
    const boot = deferred<CloudProjectBootstrapProgress>()
    const worker = operation === 'bootstrap' ? instance.bootstrapLocalProject : instance.importRemoteProject
    vi.mocked(worker).mockReturnValueOnce(boot.promise)
    bridge.invoke.mockResolvedValue({ id: 'n', content: 'saved' })
    const project = operation === 'bootstrap' ? store.bootstrapProject(PROJECT) : store.importProject(PROJECT, 'Project')
    await vi.waitFor(() => expect(worker).toHaveBeenCalledTimes(1))
    await repository.update({ projectId: PROJECT }, 'n', { content: 'saved' })
    expect(instance.retry).not.toHaveBeenCalled()
    boot.resolve(progress(connected)); await project
    await vi.waitFor(() => expect(instance.retry).toHaveBeenCalledTimes(1))
    await store.dispose()
  })

  it('starts one C21 cycle after unlocking a connected project, and never for local-only projects', async () => {
    const instance = runtime()
    const connected = registry({ local: [bootstrapRecord()] })
    vi.mocked(instance.unlock).mockResolvedValue({ identity: {} as never, registry: connected })
    const cycle = deferred<NoteSyncProductionResult>()
    vi.mocked(instance.retry).mockReturnValue(cycle.promise)
    configureCloudSessionRuntimeFactoryForTests(() => instance)
    const store = useCloudSessionStore(); store.initialize(); await store.login('user', 'password')
    await Promise.all([store.unlock('key'), store.unlock('key')])
    await vi.waitFor(() => expect(instance.retry).toHaveBeenCalledTimes(1))
    await store.unlock('key')
    expect(instance.unlock).toHaveBeenCalledTimes(1)
    await vi.waitFor(() => expect(instance.retry).toHaveBeenCalledTimes(1))
    cycle.resolve(EMPTY_CYCLE)
    await vi.waitFor(() => expect(store.status).toBe('completed'))
    await store.lock()
    vi.mocked(instance.unlock).mockResolvedValue({ identity: {} as never, registry: READY_REGISTRY })
    await store.unlock('key')
    expect(instance.retry).toHaveBeenCalledTimes(1)
    expect(instance.bootstrapLocalProject).not.toHaveBeenCalled()
    await store.dispose()
  })

  it('does not start a cycle for a failed unlock or a blocked registry', async () => {
    const instance = runtime()
    configureCloudSessionRuntimeFactoryForTests(() => instance)
    const store = useCloudSessionStore(); store.initialize(); await store.login('user', 'password')
    vi.mocked(instance.unlock).mockRejectedValueOnce(new Error('locked'))
    await expect(store.unlock('bad')).rejects.toThrow('locked')
    vi.mocked(instance.unlock).mockResolvedValueOnce({ identity: {} as never, registry: registry({ readyForNormalCycle: false, local: [bootstrapRecord('paused')] }) })
    await store.unlock('key')
    expect(instance.retry).not.toHaveBeenCalled()
    await store.dispose()
  })

  it('does not start a cycle if logout occurs while unlock is pending', async () => {
    const instance = runtime()
    const pendingUnlock = deferred<Awaited<ReturnType<CloudSessionRuntime['unlock']>>>()
    vi.mocked(instance.unlock).mockReturnValue(pendingUnlock.promise)
    configureCloudSessionRuntimeFactoryForTests(() => instance)
    const store = useCloudSessionStore(); store.initialize(); await store.login('user', 'password')
    const unlocking = store.unlock('key')
    await store.logout()
    pendingUnlock.resolve({ identity: {} as never, registry: registry({ local: [bootstrapRecord()] }) })
    await unlocking
    expect(instance.retry).not.toHaveBeenCalled()
    expect(store.status).toBe('logged_out')
    await store.dispose()
  })

  it('serializes bootstrap and retry in both directions and cancels queued work after logout', async () => {
    const instance = runtime(); const connected = registry({ local: [bootstrapRecord()] })
    vi.mocked(instance.unlock).mockResolvedValue({ identity: {} as never, registry: connected })
    const cycle = deferred<NoteSyncProductionResult>()
    vi.mocked(instance.retry).mockReturnValueOnce(cycle.promise)
    const bootstrap = deferred<CloudProjectBootstrapProgress>()
    vi.mocked(instance.bootstrapLocalProject).mockReturnValue(bootstrap.promise)
    configureCloudSessionRuntimeFactoryForTests(() => instance)
    const store = useCloudSessionStore(); store.initialize(); await store.login('user', 'password'); await store.unlock('key')
    await vi.waitFor(() => expect(instance.retry).toHaveBeenCalledTimes(1))
    const project = store.bootstrapProject(PROJECT)
    expect(instance.bootstrapLocalProject).not.toHaveBeenCalled()
    cycle.resolve(EMPTY_CYCLE)
    await vi.waitFor(() => expect(instance.bootstrapLocalProject).toHaveBeenCalledTimes(1))
    const retry = store.retry()
    expect(instance.retry).toHaveBeenCalledTimes(1)
    await store.logout()
    bootstrap.resolve(progress(connected))
    await Promise.all([project, retry])
    expect(instance.retry).toHaveBeenCalledTimes(1)
    expect(store.status).toBe('logged_out')
    await store.dispose()
  })

  it('reports an automatic network failure without leaving the unlock UI busy', async () => {
    const instance = runtime()
    vi.mocked(instance.unlock).mockResolvedValue({ identity: {} as never, registry: registry({ local: [bootstrapRecord()] }) })
    vi.mocked(instance.retry).mockRejectedValue(new ApiError(0, 'offline', 'offline'))
    configureCloudSessionRuntimeFactoryForTests(() => instance)
    const store = useCloudSessionStore(); store.initialize(); await store.login('user', 'password'); await store.unlock('key')
    await vi.waitFor(() => expect(store.status).toBe('retryable_error'))
    expect(store.busy).toBe(false)
    expect(store.errorMessage).toBeTruthy()
    await store.dispose()
  })

  it('runs retry after an active import finishes and drops a queued retry on lock', async () => {
    const instance = runtime()
    const importing = deferred<CloudProjectBootstrapProgress>()
    vi.mocked(instance.importRemoteProject).mockReturnValue(importing.promise)
    configureCloudSessionRuntimeFactoryForTests(() => instance)
    const store = useCloudSessionStore(); store.initialize(); await store.login('user', 'password'); await store.unlock('key')
    const project = store.importProject(PROJECT, 'Project')
    await vi.waitFor(() => expect(instance.importRemoteProject).toHaveBeenCalledTimes(1))
    const retry = store.retry()
    expect(instance.retry).not.toHaveBeenCalled()
    importing.resolve(progress(registry()))
    await Promise.all([project, retry])
    expect(instance.retry).toHaveBeenCalledTimes(1)
    const nextImport = deferred<CloudProjectBootstrapProgress>()
    vi.mocked(instance.importRemoteProject).mockReturnValue(nextImport.promise)
    const nextProject = store.importProject(PROJECT, 'Project')
    await vi.waitFor(() => expect(instance.importRemoteProject).toHaveBeenCalledTimes(2))
    const queued = store.retry()
    await store.lock()
    nextImport.resolve(progress(registry()))
    await Promise.all([nextProject, queued])
    expect(instance.retry).toHaveBeenCalledTimes(1)
    expect(store.status).toBe('key_locked')
    await expect(store.retry()).rejects.toThrow('Cloud session must be unlocked')
    await store.dispose()
  })

  it('automatically retries only an already activated durable cover after ordinary save', async () => {
    const instance=runtime()
    instance.projectCoverAuthority=vi.fn().mockResolvedValue({metadata_state:'active',active:false,has_local_cover:true,blockers:[],pending:null})
    configureCloudSessionRuntimeFactoryForTests(()=>instance)
    const store=useCloudSessionStore();store.initialize()
    await store.login('normal-user','password');await store.unlock('encryption-password')
    await store.syncSavedCover(PROJECT)
    expect(instance.retry).not.toHaveBeenCalled()
    vi.mocked(instance.projectCoverAuthority).mockResolvedValue({metadata_state:'active',active:true,blockers:[],pending:{state:'uploaded',blocker:null}})
    await store.syncSavedCover(PROJECT)
    expect(instance.retry).toHaveBeenCalledOnce()
    expect(instance.beginProjectMetadataMigration).not.toHaveBeenCalled()
    await store.lock();await store.syncSavedCover(PROJECT)
    expect(instance.retry).toHaveBeenCalledOnce()
  })

  it('does not migrate or reconcile metadata on login, unlock or project listing', async () => {
    const instance = runtime()
    configureCloudSessionRuntimeFactoryForTests(() => instance)
    const store = useCloudSessionStore(); store.initialize()
    await store.login('normal-user', 'password'); await store.unlock('encryption-password'); await store.refreshProjects()
    expect(instance.beginProjectMetadataMigration).not.toHaveBeenCalled()
    expect(instance.adoptProjectMetadata).not.toHaveBeenCalled()
    expect(instance.decideProjectMetadata).not.toHaveBeenCalled()
    expect(instance.projectMetadataAuthority).not.toHaveBeenCalled()
  })

  it('does not restore metadata transport state after logout during explicit preparation', async () => {
    const instance = runtime(); const pending = deferred<void>()
    vi.mocked(instance.prepareMetadataTransport).mockReturnValue(pending.promise)
    configureCloudSessionRuntimeFactoryForTests(() => instance)
    const store = useCloudSessionStore(); store.initialize()
    await store.login('normal-user', 'password'); await store.unlock('encryption-password')
    const preparation = store.prepareMetadataTransport()
    await store.logout(); pending.resolve(); await preparation
    expect(store.metadataTransportMode).toBeNull()
    expect(store.metadataAuthority).toEqual({})
  })

  it('constructs exactly one desktop runtime and exposes no transient key material in Pinia state', () => {
    const instance = runtime()
    const factory = vi.fn(() => instance)
    configureCloudSessionRuntimeFactoryForTests(factory)
    const store = useCloudSessionStore()

    store.initialize(); store.initialize()

    expect(factory).toHaveBeenCalledTimes(1)
    expect(store.status).toBe('logged_out')
    expect(Object.keys(store.$state)).not.toEqual(expect.arrayContaining([
      'runtime', 'pending', 'recoveryKey', 'encryptionPassword', 'accountPassword', 'amk', 'kek',
    ]))
  })

  it('does not construct a cloud runtime outside Tauri', () => {
    vi.mocked(currentPlatform).mockReturnValue('web')
    const factory = vi.fn(() => runtime())
    configureCloudSessionRuntimeFactoryForTests(factory)
    const store = useCloudSessionStore()
    store.initialize()
    expect(factory).not.toHaveBeenCalled()
    expect(store.status).toBe('unavailable')
  })

  it('orders login, Recovery Key confirmation, then immutable provisioning POST', async () => {
    const instance = runtime(UNPROVISIONED)
    configureCloudSessionRuntimeFactoryForTests(() => instance)
    const store = useCloudSessionStore(); store.initialize()

    await store.login('normal-user', 'account-password')
    expect(store.status).toBe('provisioning')
    const display = await store.prepareProvisioning('separate-e2ee-password')
    expect(display).toEqual(new Uint8Array([1, 2, 3]))
    expect(instance.submitCryptoProvisioning).not.toHaveBeenCalled()

    await store.submitProvisioning()
    expect((instance.beginCryptoProvisioning as ReturnType<typeof vi.fn>).mock.invocationCallOrder[0]!)
      .toBeLessThan((instance.submitCryptoProvisioning as ReturnType<typeof vi.fn>).mock.invocationCallOrder[0]!)
    expect(store.status).toBe('key_locked')
    expect(store.hasProvisionedKey).toBe(true)
  })

  it('keeps the pending immutable request for a safe lost-response retry and blocks conflicting state', async () => {
    const instance = runtime(UNPROVISIONED)
    const retained = pending()
    ;(instance.beginCryptoProvisioning as ReturnType<typeof vi.fn>).mockResolvedValue(retained)
    ;(instance.submitCryptoProvisioning as ReturnType<typeof vi.fn>)
      .mockRejectedValueOnce(new Error('network unknown'))
      .mockResolvedValueOnce(PROVISIONED)
    configureCloudSessionRuntimeFactoryForTests(() => instance)
    const store = useCloudSessionStore(); store.initialize()
    await store.login('normal-user', 'account-password')
    await store.prepareProvisioning('separate-e2ee-password')

    await expect(store.submitProvisioning()).rejects.toThrow('network unknown')
    await store.submitProvisioning()
    expect(instance.beginCryptoProvisioning).toHaveBeenCalledTimes(1)
    expect(instance.submitCryptoProvisioning).toHaveBeenCalledTimes(2)
    expect(store.status).toBe('key_locked')
  })

  it('locks and invalidates the visible session after a 401 without retaining onboarding material', async () => {
    const instance = runtime(UNPROVISIONED)
    const retained = pending()
    ;(instance.beginCryptoProvisioning as ReturnType<typeof vi.fn>).mockResolvedValue(retained)
    ;(instance.submitCryptoProvisioning as ReturnType<typeof vi.fn>).mockRejectedValue(new ApiError(401, 'unauthorized', 'expired'))
    configureCloudSessionRuntimeFactoryForTests(() => instance)
    const store = useCloudSessionStore(); store.initialize()
    await store.login('normal-user', 'account-password')
    await store.prepareProvisioning('separate-e2ee-password')

    await expect(store.submitProvisioning()).rejects.toBeDefined()
    expect(store.status).toBe('logged_out')
    expect(store.username).toBeNull()
    expect(store.busy).toBe(false)
    expect(retained.dispose).toHaveBeenCalled()
  })

  it('disposes a pending setup on logout or account switch and surfaces immutable conflicts as blocked', async () => {
    const instance = runtime(UNPROVISIONED)
    const retained = pending()
    ;(instance.beginCryptoProvisioning as ReturnType<typeof vi.fn>).mockResolvedValue(retained)
    configureCloudSessionRuntimeFactoryForTests(() => instance)
    const store = useCloudSessionStore(); store.initialize()
    await store.login('first-user', 'account-password')
    await store.prepareProvisioning('separate-e2ee-password')
    await store.login('second-user', 'account-password')
    expect(retained.dispose).toHaveBeenCalled()

    const conflicting = pending()
    ;(instance.beginCryptoProvisioning as ReturnType<typeof vi.fn>).mockResolvedValue(conflicting)
    ;(instance.submitCryptoProvisioning as ReturnType<typeof vi.fn>)
      .mockRejectedValue(new AccountCryptoProvisioningConflictError())
    await store.prepareProvisioning('second-e2ee-password')
    await expect(store.submitProvisioning()).rejects.toBeInstanceOf(AccountCryptoProvisioningConflictError)
    expect(store.status).toBe('blocked')
    await store.logout()
    expect(conflicting.dispose).toHaveBeenCalled()
    expect(store.status).toBe('logged_out')
  })

  it('uses a single flight and reports bounded-cycle status without claiming global sync', async () => {
    const instance = runtime()
    let resolve!: (value: NoteSyncProductionResult) => void
    ;(instance.retry as ReturnType<typeof vi.fn>).mockImplementation(() => new Promise(done => { resolve = done }))
    configureCloudSessionRuntimeFactoryForTests(() => instance)
    const store = useCloudSessionStore(); store.initialize()
    await store.login('normal-user', 'account-password')
    await store.unlock('encryption-password')

    const first = store.retry(); const second = store.retry()
    await vi.waitFor(() => expect(instance.retry).toHaveBeenCalledTimes(1))
    resolve({ ...EMPTY_CYCLE, hasRemainingWork: true })
    await Promise.all([first, second])
    expect(store.status).toBe('remaining_work')
    expect(store.hasRemainingWork).toBe(true)
  })

  it('does not start a cycle on unlock and exposes an unreconciled registry as blocked', async () => {
    const instance = runtime()
    ;(instance.unlock as ReturnType<typeof vi.fn>).mockResolvedValue({
      identity: {}, registry: {
        ...READY_REGISTRY, readyForCycle: false, readyForNormalCycle: false,
        reasons: ['missing_local_binding:remote-project'],
      },
    })
    configureCloudSessionRuntimeFactoryForTests(() => instance)
    const store = useCloudSessionStore(); store.initialize()
    await store.login('normal-user', 'account-password')
    await store.unlock('separate-e2ee-password')

    expect(instance.retry).not.toHaveBeenCalled()
    expect(store.status).toBe('blocked')
    expect(store.blockedEvents).toEqual(['Некоторые данные проекта ещё не получены. Синхронизация продолжится после их получения.'])
    expect(store.lastCycleAt).toBeNull()
  })

  it('keeps projects local-only until native preflight and a separate explicit confirmation', async () => {
    const instance = runtime()
    const ready = registry({
      remote: [{ project_id: PROJECT, bootstrap_id: TOKEN, origin_device_id: DEVICE, state: 'active', initial_event_count: 1, initial_max_server_sequence: 1 }],
      local: [bootstrapRecord()],
    })
    ;(instance.bootstrapLocalProject as ReturnType<typeof vi.fn>).mockResolvedValue(progress(ready))
    configureCloudSessionRuntimeFactoryForTests(() => instance)
    configureCloudSessionProjectLoaderForTests(async () => [{ id: PROJECT, name: 'Роман' }])
    const store = useCloudSessionStore(); store.initialize()
    await store.login('normal-user', 'account-password')
    await store.unlock('e2ee-password')

    expect(store.projects).toMatchObject([{ projectId: PROJECT, status: 'local_only', connectionAvailable: true }])
    expect(instance.bootstrapLocalProject).not.toHaveBeenCalled()
    await expect(store.prepareProjectConnection(PROJECT)).resolves.toBe(true)
    expect(instance.preflightLocalProject).toHaveBeenCalledWith(PROJECT)
    expect(instance.bootstrapLocalProject).not.toHaveBeenCalled()

    await store.bootstrapProject(PROJECT)
    await vi.waitFor(() => expect(instance.bootstrapLocalProject).toHaveBeenCalledTimes(1))
    expect(store.projects).toMatchObject([{ projectId: PROJECT, status: 'initial_sync_completed' }])
    expect(JSON.stringify(store.$state)).not.toContain(TOKEN)
  })

  it('blocks the whole project before registration when one Note is unsupported', async () => {
    const instance = runtime()
    ;(instance.preflightLocalProject as ReturnType<typeof vi.fn>).mockResolvedValue([
      { note_id: 'note-id', code: 'dependency_not_synced' },
    ])
    configureCloudSessionRuntimeFactoryForTests(() => instance)
    configureCloudSessionProjectLoaderForTests(async () => [{ id: PROJECT, name: 'Роман' }])
    const store = useCloudSessionStore(); store.initialize()
    await store.login('normal-user', 'account-password')
    await store.unlock('e2ee-password')

    expect(store.projects[0]).toMatchObject({ status: 'unsupported', connectionAvailable: false })
    await expect(store.prepareProjectConnection(PROJECT)).resolves.toBe(false)
    expect(instance.bootstrapLocalProject).not.toHaveBeenCalled()
  })

  it('offers explicit second-device import but blocks an unproven same-ID local collision', async () => {
    const remoteRegistry = registry({
      readyForCycle: false, readyForNormalCycle: false,
      reasons: [`missing_local_binding:${PROJECT}`],
      remote: [{ project_id: PROJECT, bootstrap_id: TOKEN, origin_device_id: DEVICE, state: 'active', initial_event_count: 1, initial_max_server_sequence: 1 }],
    })
    const importedRegistry = registry({ remote: remoteRegistry.remote, local: [bootstrapRecord('ready', 'import_remote')] })
    const instance = runtime()
    ;(instance.unlock as ReturnType<typeof vi.fn>).mockResolvedValue({ identity: {}, registry: remoteRegistry })
    ;(instance.importRemoteProject as ReturnType<typeof vi.fn>).mockResolvedValue(progress(importedRegistry, bootstrapRecord('ready', 'import_remote')))
    let localProjects: Array<{ id: string, name: string }> = []
    configureCloudSessionRuntimeFactoryForTests(() => instance)
    configureCloudSessionProjectLoaderForTests(async () => localProjects)
    const store = useCloudSessionStore(); store.initialize()
    await store.login('normal-user', 'account-password')
    await store.unlock('e2ee-password')
    expect(store.projects[0]).toMatchObject({ origin: 'remote', status: 'import_available' })

    localProjects = [{ id: PROJECT, name: 'Импортированный роман' }]
    const changes: string[] = []
    const onChange = (event: Event) => changes.push((event as CustomEvent<string>).detail)
    window.addEventListener('nfprogress:data-changed', onChange)
    await store.importProject(PROJECT, 'Импортированный роман')
    window.removeEventListener('nfprogress:data-changed', onChange)
    expect(changes).toContain('projects')
    expect(instance.importRemoteProject).toHaveBeenCalledWith(PROJECT, 'Импортированный роман', expect.any(Function))
    expect(store.projects[0]).toMatchObject({ status: 'initial_sync_completed', mode: 'import_remote' })

    const collisionInstance = runtime()
    ;(collisionInstance.unlock as ReturnType<typeof vi.fn>).mockResolvedValue({ identity: {}, registry: remoteRegistry })
    configureCloudSessionRuntimeFactoryForTests(() => collisionInstance)
    configureCloudSessionProjectLoaderForTests(async () => [{ id: PROJECT, name: 'Другой локальный проект' }])
    setActivePinia(createPinia())
    const collision = useCloudSessionStore(); collision.initialize()
    await collision.login('normal-user', 'account-password')
    await collision.unlock('e2ee-password')
    expect(collision.projects[0]).toMatchObject({ status: 'blocked', connectionAvailable: false })
    expect(collision.projects[0]!.reason).toContain('связь с облачной версией')
    expect(collisionInstance.importRemoteProject).not.toHaveBeenCalled()
  })

  it('lets authenticated metadata import proceed without an invented local name', async () => {
    const instance = runtime()
    ;(instance.importRemoteProject as ReturnType<typeof vi.fn>).mockResolvedValue(progress(READY_REGISTRY))
    configureCloudSessionRuntimeFactoryForTests(() => instance)
    const store = useCloudSessionStore(); store.initialize()
    await store.login('normal-user', 'account-password')
    await store.unlock('e2ee-password')
    await store.importProject(PROJECT, '')
    expect(instance.importRemoteProject).toHaveBeenCalledWith(PROJECT, '', expect.any(Function))
  })

  it('keeps one project flight and ignores its stale completion after logout', async () => {
    const instance = runtime()
    configureCloudSessionRuntimeFactoryForTests(() => instance)
    configureCloudSessionProjectLoaderForTests(async () => [{ id: PROJECT, name: 'Роман' }])
    const store = useCloudSessionStore(); store.initialize()
    await store.login('normal-user', 'account-password')
    await store.unlock('e2ee-password')
    const pendingBootstrap = deferred<CloudProjectBootstrapProgress>()
    ;(instance.bootstrapLocalProject as ReturnType<typeof vi.fn>).mockReturnValue(pendingBootstrap.promise)

    const first = store.bootstrapProject(PROJECT)
    const second = store.bootstrapProject(PROJECT)
    await vi.waitFor(() => expect(instance.bootstrapLocalProject).toHaveBeenCalledTimes(1))
    await store.logout()
    pendingBootstrap.resolve(progress(registry()))
    await Promise.all([first, second])
    expect(store.status).toBe('logged_out')
    expect(store.projects).toEqual([])
  })

  it('does not let an old bootstrap callback overwrite a newly logged-in account', async () => {
    const instance = runtime()
    configureCloudSessionRuntimeFactoryForTests(() => instance)
    configureCloudSessionProjectLoaderForTests(async () => [{ id: PROJECT, name: 'Роман' }])
    const store = useCloudSessionStore(); store.initialize()
    await store.login('first-user', 'account-password')
    await store.unlock('e2ee-password')
    const pendingBootstrap = deferred<CloudProjectBootstrapProgress>()
    ;(instance.bootstrapLocalProject as ReturnType<typeof vi.fn>).mockReturnValue(pendingBootstrap.promise)

    const oldOperation = store.bootstrapProject(PROJECT)
    await store.login('second-user', 'account-password')
    pendingBootstrap.resolve(progress(registry()))
    await oldOperation

    expect(store.username).toBe('normal-user')
    expect(store.status).toBe('key_locked')
    expect(store.projects).toEqual([])
  })

  it('locks stale bootstrap callbacks and invalidates project UI on a 401', async () => {
    const instance = runtime()
    configureCloudSessionRuntimeFactoryForTests(() => instance)
    configureCloudSessionProjectLoaderForTests(async () => [{ id: PROJECT, name: 'Роман' }])
    const store = useCloudSessionStore(); store.initialize()
    await store.login('normal-user', 'account-password')
    await store.unlock('e2ee-password')
    const pendingBootstrap = deferred<CloudProjectBootstrapProgress>()
    ;(instance.bootstrapLocalProject as ReturnType<typeof vi.fn>).mockReturnValueOnce(pendingBootstrap.promise)
    const staleOperation = store.bootstrapProject(PROJECT)
    await vi.waitFor(() => expect(instance.bootstrapLocalProject).toHaveBeenCalledTimes(1))
    await store.lock()
    pendingBootstrap.resolve(progress(registry()))
    await staleOperation
    expect(store.status).toBe('key_locked')
    expect(store.projects).toEqual([])

    await store.unlock('e2ee-password')
    ;(instance.bootstrapLocalProject as ReturnType<typeof vi.fn>).mockRejectedValueOnce(
      new ApiError(401, 'unauthorized', 'sensitive server detail'),
    )
    await expect(store.bootstrapProject(PROJECT)).rejects.toBeInstanceOf(ApiError)
    expect(store.status).toBe('logged_out')
    expect(store.username).toBeNull()
    expect(store.errorMessage).toBeNull()
    expect(store.busy).toBe(false)
  })

  it('surfaces paused projects and resumes without deleting durable state', async () => {
    const paused = bootstrapRecord('paused')
    const pausedRegistry = registry({
      readyForCycle: false, readyForNormalCycle: false, reasons: [`paused:${PROJECT}`],
      remote: [{ project_id: PROJECT, bootstrap_id: TOKEN, origin_device_id: DEVICE, state: 'active', initial_event_count: 1, initial_max_server_sequence: 1 }],
      local: [paused],
    })
    const instance = runtime()
    ;(instance.unlock as ReturnType<typeof vi.fn>).mockResolvedValue({ identity: {}, registry: pausedRegistry })
    ;(instance.setProjectPaused as ReturnType<typeof vi.fn>).mockResolvedValue(registry({ remote: pausedRegistry.remote, local: [bootstrapRecord()] }))
    configureCloudSessionRuntimeFactoryForTests(() => instance)
    configureCloudSessionProjectLoaderForTests(async () => [{ id: PROJECT, name: 'Роман' }])
    const store = useCloudSessionStore(); store.initialize()
    await store.login('normal-user', 'account-password')
    await store.unlock('e2ee-password')
    expect(store.projects[0]?.status).toBe('paused')
    await store.resumeProject(PROJECT)
    expect(instance.setProjectPaused).toHaveBeenCalledWith(PROJECT, false)
    expect(store.projects[0]?.status).toBe('initial_sync_completed')
  })
  it('records a bounded safe V3 cycle including apply, blocker and ACK using one correlation', async () => {
    const instance=runtime();configureCloudSessionRuntimeFactoryForTests(()=>instance)
    const cycle: NoteSyncProductionResult={transport_version:3,cutover_epoch:1,hasRemainingWork:true,blocked:['stage_dependency_missing:SECRET_ENTITY_ID'],errors:[],cycle:{
      stages:[],noteUploaded:2,resolutionUploaded:0,metadataUploaded:1,
      pulled:[{committed_cursor:7,new_events:3,replayed_events:0,has_more:false}],
      metadataApply:{applied:1,conflicts:0,orphans:0,blocked:[],listed:1},structuralApply:{applied:0,conflicts:1,orphans:1,blocked:['SECRET_ENTITY_ID'],listed:2},
      ack:{status:'no_progress',cursor:4},blocked:[],errors:[],hasRemainingWork:true,
    }}
    vi.mocked(instance.retry).mockResolvedValue(cycle)
    const store=useCloudSessionStore();store.initialize();await store.login('private-user','SECRET_PASSWORD');await store.unlock('SECRET_AMK')
    const record=vi.spyOn(diagnostics,'record');await store.retry()
    const calls=record.mock.calls;const cycleCalls=calls.filter(c=>c[1]==='sync_cycle')
    expect(cycleCalls.map(c=>c[2])).toEqual(['requested','started','sync_result','pull_result','upload_result','apply_result','apply_result','ack_result','blocker','succeeded'])
    const id=cycleCalls[1]![3];expect(cycleCalls.slice(1).every(c=>c[3]===id)).toBe(true)
    expect(cycleCalls.find(c=>c[2]==='ack_result')?.[4]).toEqual({status:'no_progress'})
    expect(cycleCalls.find(c=>c[2]==='blocker')?.[4]).toEqual({status:'blocked',error_code:'stage_dependency_missing'})
    expect(JSON.stringify(calls)).not.toContain('SECRET');expect(JSON.stringify(calls)).not.toContain('private-user');expect(store.status).toBe('blocked');record.mockRestore()
  })

})
