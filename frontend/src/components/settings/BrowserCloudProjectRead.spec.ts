import { flushPromises, mount } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import { expect, it, vi } from 'vitest'
import { BrowserReplayError } from '@/platform/web/replayRecords'
import type { BrowserCloudAccountSession } from '@/platform/web/cloudAccountSession'
import BrowserCloudProjectRead from './BrowserCloudProjectRead.vue'

it('clears earlier metadata and explains a safely refused recovery history', async () => {
  const pinia = createPinia(); setActivePinia(pinia)
  const session = { status: 'unlocked', onRevoked: () => () => {},
    readCloudProject: vi.fn().mockResolvedValue({ metadata: { name: 'Old project', goal: null, status: 'active', deadline: null } }),
    readRecoveryProject: vi.fn().mockRejectedValue(new BrowserReplayError('unsupported_history_family')),
  } as unknown as BrowserCloudAccountSession
  const wrapper = mount(BrowserCloudProjectRead, { props: { session, projectId: 'project' }, global: { plugins: [pinia] } })
  await flushPromises()
  expect(wrapper.text()).toContain('Old project')
  await wrapper.get('[data-testid="cloud-notes-open"]').trigger('click'); await flushPromises()
  expect(wrapper.find('[data-testid="cloud-project-metadata"]').exists()).toBe(false)
  expect(wrapper.find('[data-testid="cloud-project-notes"]').exists()).toBe(false)
  expect(wrapper.get('[role="alert"]').text()).toMatch(/истори|history|historial|historique|histórico|Projektverlauf/i)
  expect(wrapper.get('[role="alert"]').text()).not.toContain('unsupported_history_family')
})

it('does not offer to register a recovery browser as a sync device', async () => {
  const pinia = createPinia(); setActivePinia(pinia)
  const session = { status: 'unlocked', onRevoked: () => () => {},
    readCloudProject: vi.fn().mockRejectedValue(new (await import('@/platform/web/cloudRead')).BrowserCloudReadError('sync_device_not_registered')),
    registerCloudReadDevice: vi.fn(),
  } as unknown as BrowserCloudAccountSession
  const wrapper = mount(BrowserCloudProjectRead, { props: { session, projectId: 'project' }, global: { plugins: [pinia] } })
  await flushPromises()
  expect(wrapper.get('[role="alert"]').attributes('data-read-code')).toBe('sync_device_not_registered')
  expect(wrapper.findAll('button')).toHaveLength(2)
  expect(session.registerCloudReadDevice).not.toHaveBeenCalled()
})
