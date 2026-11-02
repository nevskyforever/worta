import { createPinia, setActivePinia } from 'pinia'
import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { settingsApi } from '@/api/settings'
import { supportsNativeUpdates, supportsUpdateChecks } from '@/platform/runtime'
import { useUpdaterStore } from '@/stores/updater'
import type { SettingsResponse } from '@/types/content'

import SettingsPage from './SettingsPage.vue'

vi.mock('@/api/settings', () => ({
  settingsApi: {
    get: vi.fn(),
    update: vi.fn(),
  },
}))

vi.mock('@/platform/runtime', () => ({
  currentPlatform: vi.fn(() => 'web'),
  supportsNativeUpdates: vi.fn(() => false),
  supportsUpdateChecks: vi.fn(() => false),
}))

const webSettings: SettingsResponse = {
  version: '5.3.9',
  values: {
    language: 'ru',
    frontend_theme: 'system',
    frontend_motion: 'full',
    start_day_time: '00:00:00',
    notification_display_time: 10,
    game_mode: false,
    inf_project: false,
    global_streak: false,
    show_written_today_in_all_projects: false,
  },
  platform: 'web',
  capabilities: {
    local_file_sync: false,
    background_file_sync: false,
    native_updates: false,
    remote_api: true,
  },
  editable_keys: [
    'language',
    'frontend_theme',
    'frontend_motion',
    'start_day_time',
    'notification_display_time',
    'game_mode',
    'inf_project',
    'global_streak',
    'show_written_today_in_all_projects',
  ],
}

const desktopSettings: SettingsResponse = {
  ...webSettings,
  values: { ...webSettings.values, background_synch: true },
  platform: 'desktop',
  capabilities: {
    local_file_sync: true,
    background_file_sync: true,
    native_updates: true,
    remote_api: false,
  },
  editable_keys: [...webSettings.editable_keys, 'background_synch'],
}

describe('SettingsPage', () => {
  beforeEach(() => {
    vi.mocked(settingsApi.get).mockReset()
    vi.mocked(settingsApi.update).mockReset()
    vi.mocked(supportsNativeUpdates).mockReset()
    vi.mocked(supportsUpdateChecks).mockReset()
    vi.mocked(supportsNativeUpdates).mockReturnValue(false)
    vi.mocked(supportsUpdateChecks).mockReturnValue(false)
    vi.mocked(settingsApi.get).mockResolvedValue(webSettings)
    vi.mocked(settingsApi.update).mockResolvedValue({
      ...webSettings,
      values: { ...webSettings.values, game_mode: true },
    })
  })

  it('shows platform-editable controls only and persists a backend-authoritative patch', async () => {
    vi.mocked(settingsApi.get).mockResolvedValue({
      ...webSettings,
      capabilities: { ...webSettings.capabilities, developer_mode_available: true },
    })
    const wrapper = mount(SettingsPage, {
      global: {
        plugins: [createPinia()],
        stubs: {
          IonContent: { template: '<div><slot /></div>' },
          IonPage: { template: '<div><slot /></div>' },
          IonIcon: true,
          IonSpinner: true,
        },
      },
    })
    await flushPromises()

    expect(wrapper.find('#settings-background-sync').exists()).toBe(false)
    expect(wrapper.find('#settings-infinite-project').exists()).toBe(true)
    expect(wrapper.find('#settings-written-today').exists()).toBe(true)
    expect(wrapper.find('#settings-notification-time').exists()).toBe(true)
    expect(wrapper.find('#settings-motion').exists()).toBe(true)
    expect(wrapper.find('[data-testid="browser-cloud-account"]').exists()).toBe(true)
    const cloudAccount = wrapper.get('[data-testid="browser-cloud-account"]')
    expect(cloudAccount.text()).toContain('Облачные проекты WORTA')
    expect(cloudAccount.get('[data-status="logged_out"]').text()).toContain('Войдите в аккаунт')
    expect(cloudAccount.find('input[autocomplete="username"]').exists()).toBe(true)
    expect(cloudAccount.find('input[autocomplete="current-password"]').exists()).toBe(true)
    expect(cloudAccount.get('button[type="submit"]').text()).toContain('Войти в облачный аккаунт')
    expect(wrapper.text()).toContain('Автор: nevskyforever / nfproject')
    await wrapper.get('#settings-game-mode').setValue(true)
    await wrapper.get('#settings-notification-time').setValue(20)
    await wrapper.get('#settings-motion').setValue('reduced')
    await wrapper.get('form').trigger('submit')
    await flushPromises()

    expect(settingsApi.update).toHaveBeenCalledWith({
      game_mode: true,
      frontend_motion: 'reduced',
      notification_display_time: 20,
    })
    expect(wrapper.get('[role="status"]').text()).toContain('Настройки сохранены')
  })

  it('shows background sync and the signed updater only in a release-enabled desktop', async () => {
    vi.mocked(settingsApi.get).mockResolvedValue(desktopSettings)
    vi.mocked(supportsNativeUpdates).mockReturnValue(true)
    vi.mocked(supportsUpdateChecks).mockReturnValue(true)
    const wrapper = mount(SettingsPage, {
      global: {
        plugins: [createPinia()],
        stubs: {
          IonContent: { template: '<div><slot /></div>' },
          IonPage: { template: '<div><slot /></div>' },
          IonIcon: true,
          IonSpinner: true,
        },
      },
    })
    await flushPromises()

    expect(wrapper.find('#settings-background-sync').exists()).toBe(true)
    expect(wrapper.find('[data-testid="browser-cloud-account"]').exists()).toBe(false)
    expect(wrapper.find('#settings-check-updates').exists()).toBe(true)
    expect(wrapper.text()).toContain('Подписанные обновления проверяются автоматически')
  })

  it('shows the current-version confirmation in the update button', async () => {
    vi.mocked(settingsApi.get).mockResolvedValue(desktopSettings)
    vi.mocked(supportsUpdateChecks).mockReturnValue(true)
    const pinia = createPinia()
    setActivePinia(pinia)
    useUpdaterStore(pinia).status = 'current'
    const wrapper = mount(SettingsPage, {
      global: {
        plugins: [pinia],
        stubs: {
          IonContent: { template: '<div><slot /></div>' },
          IonPage: { template: '<div><slot /></div>' },
          IonIcon: true,
          IonSpinner: true,
        },
      },
    })
    await flushPromises()

    expect(wrapper.get('#settings-check-updates').text()).toContain('Установлена последняя версия')
  })

  it('shows the actual runtime version without a hardcoded fallback', async () => {
    vi.mocked(settingsApi.get).mockResolvedValue(webSettings)
    const wrapper = mount(SettingsPage, {
      global: {
        plugins: [createPinia()],
        stubs: {
          IonContent: { template: '<div><slot /></div>' },
          IonPage: { template: '<div><slot /></div>' },
          IonIcon: true,
          IonSpinner: true,
        },
      },
    })
    await flushPromises()

    expect(wrapper.text()).toContain('Версия worta 5.3.9')
    expect(wrapper.text()).not.toContain('Версия worta 5.0')
    expect(wrapper.find('[data-testid="browser-cloud-account"]').exists()).toBe(false)
  })

  it('shows an unknown marker when runtime version is unavailable', async () => {
    vi.mocked(settingsApi.get).mockResolvedValue({ ...webSettings, version: undefined })
    const wrapper = mount(SettingsPage, {
      global: {
        plugins: [createPinia()],
        stubs: {
          IonContent: { template: '<div><slot /></div>' },
          IonPage: { template: '<div><slot /></div>' },
          IonIcon: true,
          IonSpinner: true,
        },
      },
    })
    await flushPromises()

    expect(wrapper.text()).toContain('Версия worta —')
  })

  it('opens the existing developer dialog only when runtime capability allows it', async () => {
    vi.mocked(settingsApi.get).mockResolvedValue({
      ...desktopSettings,
      capabilities: {
        ...desktopSettings.capabilities,
        developer_mode_available: true,
      },
    })
    const wrapper = mount(SettingsPage, {
      global: {
        plugins: [createPinia()],
        stubs: {
          IonContent: { template: '<div><slot /></div>' },
          IonPage: { template: '<div><slot /></div>' },
          IonIcon: true,
          IonSpinner: true,
          DeveloperModeDialog: {
            name: 'DeveloperModeDialog',
            props: ['open'],
            template: '<div class="developer-dialog-stub" :data-open="open" />',
          },
        },
      },
    })
    await flushPromises()

    expect(wrapper.get('#settings-open-developer-mode').text()).toContain('Открыть режим разработчика')
    expect(wrapper.get('.developer-dialog-stub').attributes('data-open')).toBe('false')
    await wrapper.get('#settings-open-developer-mode').trigger('click')
    expect(wrapper.get('.developer-dialog-stub').attributes('data-open')).toBe('true')
  })
})
