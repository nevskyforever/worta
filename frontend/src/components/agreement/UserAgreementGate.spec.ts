import { createPinia } from 'pinia'
import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { contentApi } from '@/api/content'
import { settingsApi } from '@/api/settings'
import { requestApplicationClose } from '@/platform/runtime'
import { useLocaleStore } from '@/stores/locale'
import { useThemeStore } from '@/stores/theme'
import type { SupportedLanguage } from '@/types/api'
import type { SettingsResponse } from '@/types/content'

import UserAgreementGate from './UserAgreementGate.vue'

vi.mock('@/api/content', () => ({
  contentApi: {
    agreement: vi.fn(),
    locale: vi.fn(),
  },
}))

vi.mock('@/api/settings', () => ({
  settingsApi: {
    update: vi.fn(),
    acceptUserAgreement: vi.fn(),
  },
}))

vi.mock('@/platform/runtime', () => ({
  requestApplicationClose: vi.fn(),
  syncNativeSystemBars: vi.fn().mockResolvedValue(undefined),
}))

function settingsResponse(
  values: SettingsResponse['values'] = { language: 'ru', frontend_theme: 'system' },
): SettingsResponse {
  return {
    values,
    platform: 'web',
    capabilities: {
      local_file_sync: false,
      background_file_sync: false,
      native_updates: false,
      remote_api: true,
    },
    editable_keys: ['language', 'frontend_theme'],
  }
}

function agreement(language: SupportedLanguage) {
  return {
    id: 'agreement-v1',
    language,
    html: '<!doctype html><html><head></head><body><h1>Terms</h1></body></html>',
  }
}

describe('UserAgreementGate', () => {
  beforeEach(() => {
    vi.mocked(contentApi.agreement).mockReset()
    vi.mocked(contentApi.locale).mockReset()
    vi.mocked(settingsApi.update).mockReset()
    vi.mocked(settingsApi.acceptUserAgreement).mockReset()
    vi.mocked(requestApplicationClose).mockReset()
    vi.mocked(contentApi.agreement).mockImplementation(async (language) => agreement(language))
    vi.mocked(contentApi.locale).mockResolvedValue({})
    vi.mocked(settingsApi.update).mockResolvedValue(settingsResponse())
    vi.mocked(settingsApi.acceptUserAgreement).mockResolvedValue(
      settingsResponse({
        language: 'ru',
        frontend_theme: 'system',
        user_agreement: true,
      }),
    )
    vi.mocked(requestApplicationClose).mockResolvedValue(false)
  })

  it('keeps acceptance disabled until confirmation and emits only the backend-confirmed result', async () => {
    const wrapper = mount(UserAgreementGate, { global: { plugins: [createPinia()] } })
    await flushPromises()

    const acceptButton = wrapper
      .findAll('button')
      .find((button) => button.text().includes('Принять и продолжить'))
    expect(acceptButton?.attributes('disabled')).toBeDefined()
    expect(wrapper.get('iframe').attributes('srcdoc')).toContain('data-nfprogress-agreement-theme')

    await wrapper.get('input[type="checkbox"]').setValue(true)
    expect(acceptButton?.attributes('disabled')).toBeUndefined()
    await acceptButton?.trigger('click')
    await flushPromises()

    expect(settingsApi.acceptUserAgreement).toHaveBeenCalledWith('agreement-v1')
    expect(wrapper.emitted<SettingsResponse[]>('accepted')?.[0]?.[0]?.values.user_agreement).toBe(
      true,
    )
  })

  it('overrides inline white text with the active theme colour', async () => {
    vi.mocked(contentApi.agreement).mockResolvedValue({
      id: 'agreement-v1',
      language: 'ru',
      html: '<!doctype html><html><head></head><body><p><span style="color:#ffffff">Terms</span></p></body></html>',
    })
    const pinia = createPinia()
    const wrapper = mount(UserAgreementGate, { global: { plugins: [pinia] } })
    useThemeStore(pinia).setPreference('light')
    await flushPromises()

    let srcdoc = wrapper.get('iframe').attributes('srcdoc') ?? ''
    expect(srcdoc).toContain('color: #1e2925 !important')
    expect(srcdoc).toMatch(/body\s+\*\s*\{\s*color:\s*inherit\s*!important/)
    expect(srcdoc).toContain('style="color:#ffffff"')

    useThemeStore(pinia).setPreference('dark')
    await flushPromises()
    srcdoc = wrapper.get('iframe').attributes('srcdoc') ?? ''
    expect(srcdoc).toContain('color: #edf2ef !important')
  })

  it('persists a language before applying it and reloads the shared agreement', async () => {
    vi.mocked(settingsApi.update).mockResolvedValue(
      settingsResponse({ language: 'en', frontend_theme: 'system' }),
    )
    const wrapper = mount(UserAgreementGate, { global: { plugins: [createPinia()] } })
    await flushPromises()
    await useLocaleStore().setLanguage('ru')

    await wrapper.get('#agreement-language').setValue('en')
    await flushPromises()

    expect(settingsApi.update).toHaveBeenCalledWith({ language: 'en' })
    expect(contentApi.locale).toHaveBeenCalledWith('en')
    expect(contentApi.agreement).toHaveBeenLastCalledWith('en', expect.any(AbortSignal))
    expect(vi.mocked(settingsApi.update).mock.invocationCallOrder[0]).toBeLessThan(
      vi.mocked(contentApi.locale).mock.invocationCallOrder[0] ?? Number.POSITIVE_INFINITY,
    )
  })

  it('keeps web and mobile clients on a blocking screen after decline', async () => {
    const wrapper = mount(UserAgreementGate, { global: { plugins: [createPinia()] } })
    await flushPromises()

    const declineButton = wrapper
      .findAll('button')
      .find((button) => button.text().includes('Не принимаю'))
    await declineButton?.trigger('click')
    await flushPromises()

    expect(requestApplicationClose).toHaveBeenCalledOnce()
    expect(wrapper.text()).toContain('функции приложения остаются недоступны')
    expect(wrapper.find('iframe').exists()).toBe(false)
  })

  it('offers an accessible retry when agreement loading fails', async () => {
    vi.mocked(contentApi.agreement)
      .mockRejectedValueOnce(new Error('offline'))
      .mockResolvedValueOnce(agreement('ru'))
    const wrapper = mount(UserAgreementGate, { global: { plugins: [createPinia()] } })
    await flushPromises()

    expect(wrapper.get('[role="alert"]').text()).toContain('Произошла непредвиденная ошибка')
    await wrapper.get('button').trigger('click')
    await flushPromises()

    expect(contentApi.agreement).toHaveBeenCalledTimes(2)
    expect(wrapper.find('iframe').exists()).toBe(true)
  })
})
