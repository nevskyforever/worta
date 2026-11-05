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

it('renders HTML notes in an isolated frame with active content and links removed', async () => {
  const pinia = createPinia(); setActivePinia(pinia)
  const session = { status: 'unlocked', onRevoked: () => () => {},
    readCloudProject: vi.fn().mockResolvedValue({ metadata: { name: 'Project', goal: null, status: 'active', deadline: null } }),
    readRecoveryProject: vi.fn().mockResolvedValue({ project: { name: 'Project' }, stages: [], notes: [{ id: 'n', title: 'Note', stage_id: null, content_format: 'html',
      content: '<p onclick="alert(1)">Hello <strong>world</strong><img src="https://example.invalid/x"><script>alert(2)</script><a href="https://example.invalid">link</a><meta http-equiv="refresh" content="0;url=https://example.invalid"></p>' }] }),
  } as unknown as BrowserCloudAccountSession
  const wrapper = mount(BrowserCloudProjectRead, { props: { session, projectId: 'project' }, global: { plugins: [pinia] } })
  await flushPromises(); await wrapper.get('[data-testid="cloud-notes-open"]').trigger('click'); await flushPromises()
  const frame = wrapper.get('[data-testid="cloud-note-html"]')
  const srcdoc = frame.attributes('srcdoc')
  expect(frame.attributes('sandbox')).toBe('')
  expect(srcdoc).toContain('<strong>world</strong>')
  expect(srcdoc).toContain('link')
  expect(srcdoc).not.toMatch(/onclick|<script|<img|<a |http-equiv="refresh"|example\.invalid/)
})

it('shows ordered stages and switches between their notes', async () => {
  const pinia = createPinia(); setActivePinia(pinia)
  const session = { status: 'unlocked', onRevoked: () => () => {},
    readCloudProject: vi.fn().mockResolvedValue({ metadata: { name: 'Project', goal: null, status: 'active', deadline: null } }),
    readRecoveryProject: vi.fn().mockResolvedValue({ project: { name: 'Project' }, stages: [{ id: 'b', name: 'Second' }, { id: 'a', name: 'First' }], notes: [
      { id: 'n1', title: 'A', stage_id: 'a', content_format: 'plain', content: 'Text A' },
      { id: 'n2', title: 'B', stage_id: 'b', content_format: 'plain', content: 'Text B' },
    ] }),
  } as unknown as BrowserCloudAccountSession
  const wrapper = mount(BrowserCloudProjectRead, { props: { session, projectId: 'project' }, global: { plugins: [pinia] } })
  await flushPromises(); await wrapper.get('[data-testid="cloud-notes-open"]').trigger('click'); await flushPromises()
  expect(wrapper.findAll('[data-stage-id]').map(button => button.text())).toEqual(['Second', 'First'])
  expect(wrapper.text()).toContain('Text B'); expect(wrapper.text()).not.toContain('Text A')
  await wrapper.get('[data-stage-id="a"]').trigger('click')
  expect(wrapper.text()).toContain('Text A'); expect(wrapper.text()).not.toContain('Text B')
  await wrapper.get('[data-testid="cloud-notes-open"]').trigger('click'); await flushPromises()
  expect(wrapper.get('[data-stage-id="a"]').attributes('aria-pressed')).toBe('true')
  vi.mocked(session.readRecoveryProject).mockResolvedValueOnce({ project: { name: 'Project' }, stages: [{ id: 'b', name: 'Second' }], notes: [{ id: 'n2', title: 'B', stage_id: 'b', content_format: 'plain', content: 'Text B' }] } as never)
  await wrapper.get('[data-testid="cloud-notes-open"]').trigger('click'); await flushPromises()
  expect(wrapper.get('[data-stage-id="b"]').attributes('aria-pressed')).toBe('true')
})

it('explains that documents cannot yet be recovered without displaying partial data', async () => {
  const pinia = createPinia(); setActivePinia(pinia)
  const session = { status: 'unlocked', onRevoked: () => () => {},
    readCloudProject: vi.fn().mockResolvedValue({ metadata: { name: 'Old project', goal: null, status: 'active', deadline: null } }),
    readRecoveryProject: vi.fn().mockRejectedValue(new (await import('@/platform/web/cloudRead')).BrowserCloudReadError('document_recovery_unavailable')),
  } as unknown as BrowserCloudAccountSession
  const wrapper = mount(BrowserCloudProjectRead, { props: { session, projectId: 'project' }, global: { plugins: [pinia] } })
  await flushPromises(); await wrapper.get('[data-testid="cloud-notes-open"]').trigger('click'); await flushPromises()
  expect(wrapper.get('[role="alert"]').attributes('data-read-code')).toBe('document_recovery_unavailable')
  expect(wrapper.find('[data-testid="cloud-project-metadata"]').exists()).toBe(false)
  expect(wrapper.find('[data-testid="cloud-project-notes"]').exists()).toBe(false)
})

it('opens authenticated document text safely and clears it on session revocation', async () => {
  const pinia = createPinia(); setActivePinia(pinia)
  let revoke = () => {}
  const session = { status: 'unlocked', onRevoked: (callback: () => void) => { revoke = callback; return () => {} },
    readCloudProject: vi.fn().mockRejectedValue(new BrowserReplayError('unsupported_history')),
    readRecoveryProject: vi.fn().mockResolvedValue({ project: { name: 'Project' }, stages: [], notes: [], documents: [{
      id: 'document', title: 'Native manuscript', content_json: { type: 'doc', content: [{ type: 'paragraph', content: [
        { type: 'text', text: '<script>private text</script>' },
      ] }] },
    }] }),
  } as unknown as BrowserCloudAccountSession
  const wrapper = mount(BrowserCloudProjectRead, { props: { session, projectId: 'project' }, global: { plugins: [pinia] } })
  await flushPromises(); await wrapper.get('[data-testid="cloud-notes-open"]').trigger('click'); await flushPromises()
  expect(wrapper.find('[data-testid="cloud-document-text"]').exists()).toBe(false)
  await wrapper.get('[data-document-id="document"]').trigger('click')
  expect(wrapper.get('[data-testid="cloud-document-text"]').text()).toBe('<script>private text</script>')
  expect(wrapper.find('script').exists()).toBe(false)
  vi.mocked(session.readRecoveryProject).mockResolvedValueOnce({ project: { name: 'Project' }, stages: [], notes: [], documents: [{
    id: 'document', title: 'Native manuscript', content_json: { type: 'doc', content: [{ type: 'paragraph', content: [{ type: 'text', text: 'Вторая редакция' }] }] },
  }] } as never)
  await wrapper.get('[data-testid="cloud-notes-open"]').trigger('click'); await flushPromises()
  expect(wrapper.get('[data-document-id="document"]').attributes('aria-pressed')).toBe('true')
  expect(wrapper.get('[data-testid="cloud-document-text"]').text()).toBe('Вторая редакция')
  revoke(); await flushPromises()
  expect(wrapper.find('[data-testid="cloud-document-list"]').exists()).toBe(false)
  expect(wrapper.text()).not.toContain('private text')
})
