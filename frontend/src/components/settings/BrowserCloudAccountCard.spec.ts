import { createPinia } from 'pinia'
import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, expect, it, vi } from 'vitest'
import { useLocaleStore } from '@/stores/locale'
import { CryptoError } from '@/crypto'

const fixture = vi.hoisted(() => ({
  status: 'logged_out' as 'logged_out' | 'legal_required' | 'authenticated' | 'unlocked' | 'locked',
  username: null as string | null,
  requireLegal: false,
  cloudResponse: { cloud_project_ids: ['server-project-1'], cloud_project_count: 1, max_cloud_projects: 8 },
  cloudFailure: false,
  createCloudProject: vi.fn(), login: vi.fn(), acceptLegal: vi.fn(), unlock: vi.fn(), lock: vi.fn(), logout: vi.fn(), dispose: vi.fn(),
}))

vi.mock('@/platform/web/cloudAccountSession', () => ({
  BrowserCloudAccountSession: class {
    get status() { return fixture.status }
    get username() { return fixture.username }
    get legalDocuments() { return fixture.status === 'legal_required' ? [{
      document_id: 'wotra.beta_terms', document_type: 'beta_terms', revision: '1',
      content_checksum: 'a'.repeat(64), language: 'ru', title: 'Закрытое тестирование worta',
      content: 'Возможны ошибки и потеря данных.', mandatory: true,
      requires_explicit_acceptance: true, accepted: false,
    }] : [] }
    async createCloudProject(name: string, signal: AbortSignal) { return fixture.createCloudProject(name, signal) }
    async listCloudProjects() {
      if (fixture.cloudFailure) throw Error('network_failure')
      return structuredClone(fixture.cloudResponse)
    }
    async login(username: string, password: string) {
      fixture.login(username, password)
      fixture.username = username
      fixture.status = fixture.requireLegal ? 'legal_required' : 'authenticated'
    }
    async acceptLegalDocuments(language: string, documents: unknown[]) {
      fixture.acceptLegal(language, documents)
      fixture.status = 'authenticated'
    }
    async unlock(password: string) {
      fixture.unlock(password)
      if (password === 'wrong') throw new CryptoError('decrypt_failed')
      fixture.status = 'unlocked'
    }
    async lock() { fixture.lock(); fixture.status = 'locked' }
    async logout() { fixture.logout(); fixture.status = 'logged_out'; fixture.username = null }
    async dispose() { fixture.dispose() }
  },
}))

import BrowserCloudAccountCard from './BrowserCloudAccountCard.vue'

beforeEach(() => {
  fixture.status = 'logged_out'
  fixture.username = null
  fixture.requireLegal = false
  fixture.cloudResponse = { cloud_project_ids: ['server-project-1'], cloud_project_count: 1, max_cloud_projects: 8 }
  fixture.cloudFailure = false
  fixture.createCloudProject.mockReset()
  for (const name of ['login', 'acceptLegal', 'unlock', 'lock', 'logout', 'dispose'] as const) fixture[name].mockClear()
})

it('checks legal status at login and gates the encrypted account until explicit acceptance', async () => {
  fixture.requireLegal = true
  const pinia = createPinia()
  useLocaleStore(pinia).language = 'ru'
  const wrapper = mount(BrowserCloudAccountCard, { global: { plugins: [pinia] } })
  await wrapper.get('input[autocomplete="username"]').setValue('one')
  await wrapper.get('input[autocomplete="current-password"]').setValue('account-pass')
  await wrapper.get('form').trigger('submit')
  await flushPromises()
  expect(wrapper.get('[data-status]').attributes('data-status')).toBe('legal_required')
  expect(wrapper.text()).toContain('Закрытое тестирование worta')
  expect(wrapper.text()).toContain('потеря данных')
  const accept = wrapper.findAll('button').find(button => button.text().includes('Принять действующие'))!
  expect(accept.attributes('disabled')).toBeDefined()
  await wrapper.get('input[type="checkbox"]').setValue(true)
  await accept.trigger('click')
  await flushPromises()
  expect(fixture.acceptLegal).toHaveBeenCalledOnce()
  expect(wrapper.get('[data-status]').attributes('data-status')).toBe('authenticated')
  wrapper.unmount()
})

it('shows the bounded Web account, unlock, lock and logout flow without Note actions', async () => {
  const pinia = createPinia()
  useLocaleStore(pinia).language = 'ru'
  const wrapper = mount(BrowserCloudAccountCard, { global: { plugins: [pinia] } })
  expect(wrapper.text()).toContain('Облачные проекты worta')
  expect(wrapper.get('[data-status]').attributes('data-status')).toBe('logged_out')
  await wrapper.get('input[autocomplete="username"]').setValue('one')
  await wrapper.get('input[autocomplete="current-password"]').setValue('account-pass')
  await wrapper.get('form').trigger('submit')
  await flushPromises()
  expect(fixture.login).toHaveBeenCalledWith('one', 'account-pass')
  expect(wrapper.get('[data-status]').attributes('data-status')).toBe('authenticated')
  await wrapper.get('button').trigger('click')
  await flushPromises()
  expect(wrapper.get('[data-testid="server-cloud-project-catalog"]').text()).toContain('server-project-1')
  expect(wrapper.text()).toContain('Проектов на сервере: 1 из 8')
  expect(wrapper.text()).toContain('Сервер вернул только идентификаторы')
  expect(wrapper.text()).not.toContain('PRIVATE_LOCAL_PROJECT')
  fixture.cloudFailure = true
  await wrapper.findAll('[data-testid="server-cloud-project-catalog"] button')[0]!.trigger('click')
  await flushPromises()
  expect(wrapper.get('[role="alert"]').text()).toContain('Не удалось получить список с сервера')
  fixture.cloudFailure = false
  fixture.cloudResponse = { cloud_project_ids: [], cloud_project_count: 0, max_cloud_projects: 8 }
  await wrapper.findAll('[data-testid="server-cloud-project-catalog"] button')[0]!.trigger('click')
  await flushPromises()
  expect(wrapper.text()).toContain('На сервере пока нет облачных проектов')
  await wrapper.get('input[autocomplete="off"]').setValue('wrong')
  await wrapper.get('form').trigger('submit')
  await flushPromises()
  expect(wrapper.get('[role="alert"]').text()).toContain('Проверьте пароль шифрования')
  expect(wrapper.get('[data-status]').attributes('data-status')).toBe('authenticated')
  await wrapper.get('input[autocomplete="off"]').setValue('correct')
  await wrapper.get('form').trigger('submit')
  await flushPromises()
  expect(wrapper.get('[data-status]').attributes('data-status')).toBe('unlocked')
  expect(wrapper.text()).toContain('Ключ шифрования разблокирован')
  expect(wrapper.find('button[data-note-action]').exists()).toBe(false)
  await wrapper.findAll('button').find(button => button.text().includes('Заблокировать ключ'))!.trigger('click')
  await flushPromises()
  expect(fixture.lock).toHaveBeenCalledOnce()
  expect(wrapper.get('[data-status]').attributes('data-status')).toBe('locked')
  await wrapper.findAll('button').find(button => button.text().includes('Выйти из аккаунта'))!.trigger('click')
  await flushPromises()
  expect(fixture.logout).toHaveBeenCalledOnce()
  expect(wrapper.get('[data-status]').attributes('data-status')).toBe('logged_out')
  wrapper.unmount()
  expect(fixture.dispose).toHaveBeenCalledOnce()
})

it('offers keyboard submission only when unlocked, preserves a failed name, and refreshes the catalog after retry', async () => {
  const pinia = createPinia()
  useLocaleStore(pinia).language = 'ru'
  fixture.status = 'authenticated'
  fixture.cloudResponse = { cloud_project_ids: [], cloud_project_count: 0, max_cloud_projects: 8 }
  const wrapper = mount(BrowserCloudAccountCard, { global: { plugins: [pinia] } })
  expect(wrapper.find('[data-testid="cloud-project-create"]').exists()).toBe(false)
  await wrapper.get('input[autocomplete="off"]').setValue('correct')
  await wrapper.get('form').trigger('submit')
  await flushPromises()
  const form = wrapper.get('[data-testid="cloud-project-create"]')
  expect(form.get('button').attributes('disabled')).toBeDefined()
  await form.get('input').setValue('  Первый проект  ')
  let fail!: (error: Error) => void
  fixture.createCloudProject.mockImplementationOnce(() => new Promise((_resolve, reject) => { fail = reject }))
  await form.trigger('submit')
  expect(form.get('[role="status"]').text()).toContain('Сохраняем')
  expect(form.get('button').attributes('disabled')).toBeDefined()
  await form.trigger('submit')
  expect(fixture.createCloudProject).toHaveBeenCalledExactlyOnceWith('Первый проект', expect.any(AbortSignal))
  fail(new Error('network failure'))
  await flushPromises()
  expect(form.get('[role="alert"]').text()).toContain('с тем же названием')
  expect(form.get('input').element.value).toBe('  Первый проект  ')
  fixture.createCloudProject.mockImplementationOnce(async () => {
    fixture.cloudResponse = { cloud_project_ids: ['new-cloud-project'], cloud_project_count: 1, max_cloud_projects: 8 }
    return { projectId: 'new-cloud-project', serverSequence: 1 }
  })
  await form.trigger('submit')
  await flushPromises()
  expect(form.get('input').element.value).toBe('')
  expect(form.get('[role="status"]').text()).toContain('Облачный проект создан')
  expect(wrapper.get('[data-testid="server-cloud-project-catalog"]').text()).toContain('new-cloud-project')
  await wrapper.get('[data-testid="cloud-account-lock"]').trigger('click')
  await flushPromises()
  expect(wrapper.find('[data-testid="cloud-project-create"]').exists()).toBe(false)
  wrapper.unmount()
})
