import { flushPromises, mount } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import { beforeEach, describe, expect, it, vi } from 'vitest'

vi.mock('@/platform/runtime', () => ({ currentPlatform: vi.fn(() => 'tauri') }))
vi.mock('@/platform/desktopCloud', () => ({ desktopCloudEnabled: () => true }))
vi.mock('@/services/localNoteWrites', () => ({ onLocalNoteWrite: vi.fn().mockResolvedValue(() => {}) }))

import type { CurrentUserCryptoRecord } from '@/api/accountCrypto'
import type { NoteSyncOrchestratorResult } from '@/cloud/noteSyncOrchestrator'
import {
  configureCloudSessionProjectLoaderForTests,
  configureCloudSessionRuntimeFactoryForTests,
  useCloudSessionStore,
  type CloudSessionRuntime,
} from '@/stores/cloudSession'
import { MetadataImportContinuationError } from '@/cloud/projectMetadataMigrationRuntime'
import CloudSyncSettingsCard from './CloudSyncSettingsCard.vue'

const RECORD: CurrentUserCryptoRecord = {
  provisioned: true,
  password: {
    crypto_version: 1, wrapping_version: 1,
    kdf: { kdf_version: 1, algorithm: 'argon2id13', salt: 'salt', opslimit: 2, memlimit: 67108864 },
    nonce: 'nonce', ciphertext: 'ciphertext',
  },
  recovery: { crypto_version: 1, wrapping_version: 1, nonce: 'recovery-nonce', ciphertext: 'recovery-ciphertext' },
}
const CYCLE: NoteSyncOrchestratorResult = { stages: [], sealed: [], uploaded: 0, pulled: [], applied: [], blocked: [], errors: [], hasRemainingWork: false }
const REGISTRY = { readyForCycle: true, readyForNormalCycle: true, reasons: [], remote: [], local: [], currentCursor: 0 }
const PROJECT = '123e4567-e89b-42d3-a456-426614174010'
const DEVICE = '123e4567-e89b-42d3-a456-426614174001'
const TOKEN = '123e4567-e89b-42d3-a456-426614174020'

function runtime(): CloudSessionRuntime {
  return {
    login: vi.fn().mockResolvedValue({ context: { username: 'normal-user' } }),
    cryptoRecord: vi.fn().mockResolvedValue(RECORD),
    beginCryptoProvisioning: vi.fn(), submitCryptoProvisioning: vi.fn(), reconcileCryptoProvisioning: vi.fn(),
    unlock: vi.fn().mockResolvedValue({ identity: {}, registry: REGISTRY }),
    reconcileProjects: vi.fn().mockResolvedValue(REGISTRY), preflightLocalProject: vi.fn().mockResolvedValue([]),
    bootstrapLocalProject: vi.fn(), importRemoteProject: vi.fn(), setProjectPaused: vi.fn().mockResolvedValue(REGISTRY),
    retry: vi.fn().mockResolvedValue(CYCLE),
    lock: vi.fn().mockResolvedValue(undefined), logout: vi.fn().mockResolvedValue(undefined), dispose: vi.fn().mockResolvedValue(undefined),
  } as unknown as CloudSessionRuntime
}

describe('CloudSyncSettingsCard', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    configureCloudSessionRuntimeFactoryForTests(null)
    configureCloudSessionProjectLoaderForTests(async () => [])
  })

  it('shows unlock separately from a bounded cycle and never labels it as fully synced', async () => {
    const pinia = createPinia(); setActivePinia(pinia)
    configureCloudSessionRuntimeFactoryForTests(runtime)
    const cloud = useCloudSessionStore(); cloud.initialize()
    const wrapper = mount(CloudSyncSettingsCard, {
      global: { plugins: [pinia], stubs: { IonIcon: true, IonSpinner: true } },
    })

    expect(wrapper.text()).toContain('Войти в облачный аккаунт')
    await wrapper.find('input[autocomplete="username"]').setValue('normal-user')
    await wrapper.find('input[autocomplete="current-password"]').setValue('account-password')
    await wrapper.find('form').trigger('submit')
    await flushPromises()
    expect(wrapper.text()).toContain('Введите пароль шифрования')

    await cloud.unlock('separate-e2ee-password')
    await flushPromises()
    expect(wrapper.text()).toContain('Готово к синхронизации')
    expect(wrapper.text()).not.toContain('Последний ограниченный цикл завершён')
    expect(wrapper.text()).not.toContain('Все данные синхронизированы')
    wrapper.unmount()
  })

  it('requires native preflight and a second confirmation before project registration', async () => {
    const instance = runtime()
    const record = {
      project_id: PROJECT, account_id: 'local-account', device_id: DEVICE, bootstrap_id: TOKEN,
      mode: 'upload_existing' as const, phase: 'ready' as const, remote_state: 'active' as const,
      initial_event_count: 1, initial_local_ordinal_hi: 1, remote_high_water: 1,
      initial_max_server_sequence: 1, blocked_reason: null,
    }
    const readyRegistry = {
      ...REGISTRY,
      remote: [{ project_id: PROJECT, bootstrap_id: TOKEN, origin_device_id: DEVICE, state: 'active' as const, initial_event_count: 1, initial_max_server_sequence: 1 }],
      local: [record], currentCursor: 1,
    }
    ;(instance.bootstrapLocalProject as ReturnType<typeof vi.fn>).mockImplementation(async (_projectId, report) => {
      report?.('registering'); report?.('preparing_initial_notes'); report?.('uploading_initial_notes')
      report?.('completing_registration'); report?.('pulling_remote_notes'); report?.('initial_sync_completed')
      return { project: record, registry: readyRegistry, cycle: CYCLE, hasRemainingWork: false }
    })
    configureCloudSessionRuntimeFactoryForTests(() => instance)
    configureCloudSessionProjectLoaderForTests(async () => [{ id: PROJECT, name: 'Роман' }])
    const pinia = createPinia(); setActivePinia(pinia)
    const cloud = useCloudSessionStore(); cloud.initialize()
    const wrapper = mount(CloudSyncSettingsCard, {
      global: { plugins: [pinia], stubs: { IonIcon: true, IonSpinner: true } },
    })
    await cloud.login('normal-user', 'account-password')
    await cloud.unlock('e2ee-password')
    await flushPromises()

    const check = wrapper.findAll('button').find(button => button.text().includes('Проверить и подключить'))
    expect(check).toBeDefined()
    await check!.trigger('click'); await flushPromises()
    expect(instance.preflightLocalProject).toHaveBeenCalledWith(PROJECT)
    expect(instance.bootstrapLocalProject).not.toHaveBeenCalled()
    expect(wrapper.text()).toContain('Подтвердить первоначальное подключение')

    const confirm = wrapper.findAll('button').find(button => button.text().includes('Подтвердить первоначальное подключение'))
    await confirm!.trigger('click'); await flushPromises()
    expect(instance.bootstrapLocalProject).toHaveBeenCalledTimes(1)
    expect(wrapper.text()).toContain('Обмен изменениями завершён')
    expect(wrapper.text()).toContain('Обмениваются поддерживаемые заметки проекта')
    expect(wrapper.text()).not.toContain('Полностью синхронизировано')
    wrapper.unmount()
  })

  it('allows authenticated cloud metadata to supply the imported project name', async () => {
    const instance = runtime()
    const remoteRegistry = {
      ...REGISTRY,
      remote: [{
        project_id: PROJECT,
        bootstrap_id: TOKEN,
        origin_device_id: DEVICE,
        state: 'active' as const,
        initial_event_count: 1,
        initial_max_server_sequence: 1,
      }],
    }
    ;(instance.unlock as ReturnType<typeof vi.fn>).mockResolvedValue({ identity: {}, registry: remoteRegistry })
    configureCloudSessionRuntimeFactoryForTests(() => instance)
    const pinia = createPinia(); setActivePinia(pinia)
    const cloud = useCloudSessionStore(); cloud.initialize()
    const wrapper = mount(CloudSyncSettingsCard, {
      global: { plugins: [pinia], stubs: { IonIcon: true, IonSpinner: true } },
    })

    await cloud.login('normal-user', 'account-password')
    await cloud.unlock('e2ee-password')
    await flushPromises()
    const startImport = wrapper.findAll('button').find(button => button.text().includes('Импортировать на это устройство'))
    expect(startImport).toBeDefined()
    await startImport!.trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('Название для проекта без облачных настроек')
    expect(wrapper.find('input').attributes('required')).toBeUndefined()
    expect(wrapper.text()).toContain('Введите название только при отсутствии этих настроек')
    expect(instance.importRemoteProject).not.toHaveBeenCalled()
    ;(instance.importRemoteProject as ReturnType<typeof vi.fn>).mockRejectedValueOnce(new MetadataImportContinuationError(16))
    await wrapper.find('form.cloud-sync-card__inline-form').trigger('submit'); await flushPromises()
    expect(wrapper.text()).toContain('Проверенные страницы сохранены. Повторите импорт')
    expect(cloud.projects[0]?.status).toBe('import_available')
    expect(wrapper.findAll('button').some(button => button.text().includes('Импортировать на это устройство'))).toBe(true)
    wrapper.unmount()
  })
})
