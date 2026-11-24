import { ApiError } from '@/api/client'
import { BrowserCloudReadError } from '@/platform/web/cloudRead'
import { flushPromises, mount } from '@vue/test-utils'
import { defineComponent } from 'vue'
import { createPinia, setActivePinia } from 'pinia'
import { expect, it, vi } from 'vitest'
import { BrowserReplayError } from '@/platform/web/replayRecords'
import type { BrowserCloudAccountSession } from '@/platform/web/cloudAccountSession'
import BrowserCloudProjectRead from './BrowserCloudProjectRead.vue'
import gameFixtures from '@/cloud/__fixtures__/gameCodecV1.json'

it('shows only available game views and preserves the balance across them', async () => {
  const pinia = createPinia(); setActivePinia(pinia)
  const genesis = gameFixtures.examples.find(example => example.name === 'account-genesis')!.event
  if (genesis.action.kind !== 'genesis') throw new Error('Expected an account genesis fixture')
  const snapshot = {
    project: { name: 'Project' }, stages: [], notes: [], games: [],
    accountGame: genesis.action.base, accountGameHead: genesis.header.event_id,
  }
  const session = {
    status: 'unlocked', onRevoked: () => () => {},
    readCloudProject: vi.fn().mockRejectedValue(new BrowserReplayError('unsupported_history')),
    readRecoveryProject: vi.fn().mockResolvedValue(snapshot),
    gameWriteReady: vi.fn().mockResolvedValueOnce(false).mockResolvedValueOnce(true),
  } as unknown as BrowserCloudAccountSession
  const view = mount(BrowserCloudProjectRead, {
    props: { session, projectId: 'game-project', autoOpenRecovery: true, workspace: true, section: 'game' },
    global: { plugins: [pinia], stubs: { IonIcon: true } },
  })
  const views = () => view.findAll('[data-game-view]').map(button => button.attributes('data-game-view'))
  await flushPromises()
  expect(views()).toEqual(['overview', 'inventory'])
  const balance = view.get('[data-testid="cloud-game-coins"]').text()
  await view.get('[data-game-view="inventory"]').trigger('click')
  expect(view.find('[data-testid="cloud-game-inventory"]').exists()).toBe(true)
  expect(view.get('[data-testid="cloud-game-inventory"]').find('button').exists()).toBe(false)
  await view.get('[data-testid="cloud-project-refresh"]').trigger('click')
  await flushPromises()
  expect(views()).toEqual(['overview', 'inventory', 'shop'])
  await view.get('[data-game-view="shop"]').trigger('click')
  expect(view.get('[data-game-view="shop"]').attributes('aria-pressed')).toBe('true')
  expect(view.find('[data-testid="cloud-game-inventory"]').exists()).toBe(false)
  expect(view.get('[data-testid="cloud-game-coins"]').text()).toBe(balance)
})

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
      id: 'document', stage_id: null, title: 'Native manuscript', content_json: { type: 'doc', content: [{ type: 'paragraph', content: [
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
    id: 'document', stage_id: null, title: 'Native manuscript', content_json: { type: 'doc', content: [{ type: 'paragraph', content: [{ type: 'text', text: 'Вторая редакция' }] }] },
  }] } as never)
  await wrapper.get('[data-testid="cloud-notes-open"]').trigger('click'); await flushPromises()
  expect(wrapper.get('[data-document-id="document"]').attributes('aria-pressed')).toBe('true')
  expect(wrapper.get('[data-testid="cloud-document-text"]').text()).toBe('Вторая редакция')
  revoke(); await flushPromises()
  expect(wrapper.find('[data-testid="cloud-document-list"]').exists()).toBe(false)
  expect(wrapper.text()).not.toContain('private text')
})

it('isolates documents by Stage and restores the account-scoped selection on remount', async () => {
  const pinia = createPinia(); setActivePinia(pinia)
  const key = `worta:recovery-stage:${JSON.stringify(['stage-reader-account', 'project'])}`
  const descriptor = Object.getOwnPropertyDescriptor(window, 'localStorage'), preferences = new Map<string, string>()
  Object.defineProperty(window, 'localStorage', { configurable: true, value: {
    getItem: (key: string) => preferences.get(key) ?? null, setItem: (key: string, value: string) => preferences.set(key, value), removeItem: (key: string) => preferences.delete(key),
  } })
  const session = { status: 'unlocked', currentIdentity: { local_account_id: 'stage-reader-account' }, onRevoked: () => () => {},
    readCloudProject: vi.fn().mockRejectedValue(new BrowserReplayError('unsupported_history')),
    readRecoveryProject: vi.fn().mockResolvedValue({ project: { name: 'Project' }, stages: [{ id: 'b', name: 'Second' }, { id: 'a', name: 'First' }], notes: [], documents: [
      { id: 'project-document', stage_id: null, title: 'Project manuscript', content_json: { type: 'doc', content: [{ type: 'paragraph', content: [{ type: 'text', text: 'Project text' }] }] } },
      { id: 'stage-document', stage_id: 'a', title: 'Stage manuscript', content_json: { type: 'doc', content: [{ type: 'paragraph', content: [{ type: 'text', text: 'Stage A text' }] }] } },
    ] }),
  } as unknown as BrowserCloudAccountSession
  const open = async () => {
    const view = mount(BrowserCloudProjectRead, { props: { session, projectId: 'project' }, global: { plugins: [pinia] } })
    await flushPromises(); await view.get('[data-testid="cloud-notes-open"]').trigger('click'); await flushPromises()
    return view
  }
  const first = await open()
  expect(first.find('[data-document-id="project-document"]').exists()).toBe(true)
  expect(first.find('[data-document-id="stage-document"]').exists()).toBe(false)
  await first.get('[data-stage-id="a"]').trigger('click')
  await first.get('[data-document-id="stage-document"]').trigger('click')
  expect(first.get('[data-testid="cloud-document-text"]').text()).toBe('Stage A text')
  await first.get('[data-stage-id="b"]').trigger('click')
  expect(first.find('[data-testid="cloud-document-list"]').exists()).toBe(false)
  expect(first.find('[data-testid="cloud-document-text"]').exists()).toBe(false)
  await first.get('[data-stage-id="a"]').trigger('click'); first.unmount()
  const reopened = await open()
  expect(reopened.get('[data-stage-id="a"]').attributes('aria-pressed')).toBe('true')
  expect(reopened.find('[data-document-id="project-document"]').exists()).toBe(false)
  expect(reopened.find('[data-document-id="stage-document"]').exists()).toBe(true)
  reopened.unmount(); window.localStorage.removeItem(key)
  if (descriptor) Object.defineProperty(window, 'localStorage', descriptor)
  else Reflect.deleteProperty(window, 'localStorage')
})

it('opens a verified project Map in secure read-only mode even when Stages exist', async () => {
  const pinia = createPinia(); setActivePinia(pinia)
  let revoke = () => {}
  const session = { status: 'unlocked', onRevoked: (callback: () => void) => { revoke = callback; return () => {} },
    readCloudProject: vi.fn().mockRejectedValue(new BrowserReplayError('unsupported_history')),
    readRecoveryProject: vi.fn().mockResolvedValue({ project: { name: 'Project' }, stages: [{ id: 's', name: 'Stage' }], notes: [], documents: [],
      maps: [{ project_id: 'project', event_id: 'event', stage_id: null, data: { nodeData: { id: 'root', topic: 'Verified map', children: [] } }, annotations: {} }] }),
  } as unknown as BrowserCloudAccountSession
  const mapRenderer = defineComponent({ props: { map: { type: Object, required: true }, secureReadOnly: Boolean, persist: Function, importXMind: Function },
    template: '<div data-testid="map-renderer">{{ map.data.nodeData.topic }}</div>' })
  const wrapper = mount(BrowserCloudProjectRead, { props: { session, projectId: 'project' }, global: { plugins: [pinia], stubs: { MindMapEditor: mapRenderer } } })
  await flushPromises(); await wrapper.get('[data-testid="cloud-notes-open"]').trigger('click'); await flushPromises()
  expect(wrapper.get('[data-testid="cloud-project-notes-select"]').attributes('aria-pressed')).toBe('true')
  const renderer = wrapper.findComponent(mapRenderer)
  expect(renderer.props('secureReadOnly')).toBe(true)
  expect(renderer.props('map')).toMatchObject({ project_id: 'project', stage_id: null, read_only: true })
  expect(wrapper.text()).toContain('Verified map')
  await expect(renderer.props('persist')!()).rejects.toThrow('read_only')
  revoke(); await flushPromises()
  expect(wrapper.find('[data-testid="cloud-map-read"]').exists()).toBe(false)
})

it('keeps the previous total until the server-confirmed Progress projection arrives', async () => {
  const { presentProgress } = await import('@/platform/web/progressView')
  const show = (total: number, eventId: string) => ({
    project: { name: 'Project' }, notes: [], stages: [], documents: [],
    progress: [presentProgress({ project_id: 'project', stage_id: null, event_id: eventId,
      base_total: total.toFixed(6), entries: [] },
    { unit: 'symbols', goal: 1000, infinite: false, created_at: null }, '2026-11-23')],
  })
  let confirm!: (value: ReturnType<typeof show>) => void
  const save = vi.fn(() => new Promise<ReturnType<typeof show>>(resolve => { confirm = resolve }))
  const session = { status: 'unlocked', onRevoked: () => () => {},
    readCloudProject: vi.fn().mockRejectedValue(new BrowserReplayError('unsupported_history')),
    readRecoveryProject: vi.fn().mockResolvedValue(show(100, 'before')),
    progressWriteReady: vi.fn().mockResolvedValue(true), saveProgress: save,
  } as unknown as BrowserCloudAccountSession
  const wrapper = mount(BrowserCloudProjectRead, { props: { session, projectId: 'project', autoOpenRecovery: true, workspace: true },
    global: { plugins: [createPinia()], stubs: { RouterLink: true } } })
  await flushPromises()
  const pane = () => wrapper.get('[data-testid="cloud-progress-read"]')
  await pane().get('input').setValue('120')
  await pane().get('form').trigger('submit')
  expect(save).toHaveBeenCalledWith('project', null, 'before', { total: 120 }, expect.any(AbortSignal))
  expect(pane().get('[data-testid="cloud-progress-total"]').text()).toBe('100')
  confirm(show(120, 'confirmed')); await flushPromises()
  expect(pane().get('[data-testid="cloud-progress-total"]').text()).toBe('120')
  wrapper.unmount()
})

it('keeps an edit after a failed save and shows the confirmed version after retry', async () => {
  const pinia = createPinia(); setActivePinia(pinia)
  const original = { type: 'doc', content: [{ type: 'paragraph', content: [{ type: 'text', text: 'Desktop text' }] }] }
  const edited = { type: 'doc', content: [{ type: 'paragraph', content: [{ type: 'text', text: 'Web text', marks: [{ type: 'bold' }] }] }] }
  const current = { project: { name: 'Project' }, stages: [{ id: 'stage', name: 'Stage' }], notes: [],
    documents: [{ id: 'doc', stage_id: 'stage', title: 'Stage text', content_json: original }], documentHeads: { doc: 'native-head' } }
  const saveDocument = vi.fn().mockRejectedValueOnce(new ApiError(0, 'network_error', 'offline')).mockResolvedValueOnce({
    ...current, documents: [{ ...current.documents[0], content_json: edited }], documentHeads: { doc: 'web-head' },
  })
  const session = { status: 'unlocked', onRevoked: () => () => {}, readCloudProject: vi.fn().mockResolvedValue(null),
    readRecoveryProject: vi.fn().mockResolvedValue(current), documentWriteReady: vi.fn().mockResolvedValue(true),
    saveDocument,
  } as unknown as BrowserCloudAccountSession
  const editor = defineComponent({ props: { initialContent: Object, readOnly: Boolean }, emits: ['update'], template:
    '<button v-if="!readOnly" data-testid="change-document" @click="$emit(\'update\', next)">Change</button>',
    data: () => ({ next: edited }),
  })
  const wrapper = mount(BrowserCloudProjectRead, { props: { session, projectId: 'project', workspace: true,
    autoOpenRecovery: true, section: 'texts' }, global: { plugins: [pinia], stubs: { NFDocumentEditor: editor } } })
  await flushPromises()
  await wrapper.get('[data-stage-id="stage"]').trigger('click')
  expect(wrapper.get('[data-testid="cloud-document-read"]').text()).toContain('Stage text')
  await wrapper.get('[data-testid="cloud-document-edit-action"]').trigger('click')
  await flushPromises()
  await wrapper.get('[data-testid="change-document"]').trigger('click')
  await wrapper.get('[data-testid="cloud-document-save"]').trigger('click')
  await flushPromises()
  expect(wrapper.get('[data-testid="cloud-document-edit"]').find('[role="alert"]').exists()).toBe(true)
  await wrapper.get('[data-testid="cloud-document-save"]').trigger('click')
  await flushPromises()
  expect(saveDocument).toHaveBeenCalledTimes(2)
  expect(saveDocument.mock.calls.map(call => call.slice(0, 4))).toEqual(Array(2).fill(['project', 'stage', 'native-head', edited]))
  expect(wrapper.find('[data-testid="cloud-document-edit"]').exists()).toBe(false)
  expect(wrapper.find('[role="status"]').exists()).toBe(true)
  wrapper.unmount()
})

it('compares the latest cloud text before explicitly saving an outdated draft', async () => {
  const initial = { type: 'doc', content: [{ type: 'paragraph', content: [{ type: 'text', text: 'Base' }] }] }
  const edited = { type: 'doc', content: [{ type: 'paragraph', content: [{ type: 'text', text: 'My draft' }] }] }
  const cloud = { type: 'doc', content: [{ type: 'paragraph', content: [{ type: 'text', text: 'New cloud version' }] }] }
  const project = (head: string, content: typeof initial) => ({ project: { name: 'Project' }, stages: [], notes: [],
    documents: [{ id: 'doc', stage_id: null, title: 'Text', content_json: content }], documentHeads: { doc: head } })
  const saveDocument = vi.fn().mockRejectedValueOnce(new BrowserCloudReadError('document_changed'))
    .mockResolvedValueOnce(project('saved', edited))
  const session = { status: 'unlocked', onRevoked: () => () => {}, readCloudProject: vi.fn().mockResolvedValue(null),
    readRecoveryProject: vi.fn().mockResolvedValueOnce(project('base', initial)).mockResolvedValueOnce(project('cloud', cloud)),
    documentWriteReady: vi.fn().mockResolvedValue(true), saveDocument,
  } as unknown as BrowserCloudAccountSession
  const editor = defineComponent({ props: { initialContent: Object, readOnly: Boolean }, emits: ['update'], template:
    '<button v-if="!readOnly" data-testid="change-document" @click="$emit(\'update\', next)">Edit</button><pre v-else>{{ initialContent }}</pre>',
    data: () => ({ next: edited }),
  })
  const wrapper = mount(BrowserCloudProjectRead, { props: { session, projectId: 'project', workspace: true,
    autoOpenRecovery: true, section: 'texts' }, global: { plugins: [createPinia()], stubs: { NFDocumentEditor: editor } } })
  const pane = () => wrapper.get('[data-testid="cloud-document-edit"]')
  await flushPromises()
  await wrapper.get('[data-testid="cloud-document-edit-action"]').trigger('click')
  await flushPromises()
  await pane().get('[data-testid="change-document"]').trigger('click')
  await pane().get('[data-testid="cloud-document-save"]').trigger('click')
  await flushPromises()
  expect(pane().find('[data-testid="cloud-document-save"]').exists()).toBe(false)
  await pane().get('button.nf-button').trigger('click')
  await flushPromises()
  expect(pane().text()).toContain('New cloud version')
  expect(saveDocument).toHaveBeenCalledTimes(1)
  await pane().get('[data-testid="cloud-document-keep-draft"]').trigger('click')
  await pane().get('[data-testid="cloud-document-save"]').trigger('click')
  await flushPromises()
  expect(saveDocument.mock.calls.map(call => call.slice(0, 4))).toEqual([
    ['project', null, 'base', edited], ['project', null, 'cloud', edited],
  ])
  wrapper.unmount()
})


it('shows verified project settings when content recovery fails', async () => {
  const session = {
    status: 'unlocked', onRevoked: () => () => {},
    readCloudProject: vi.fn().mockRejectedValueOnce(new BrowserReplayError('unsupported_history'))
      .mockResolvedValueOnce({ metadata: { name: 'Verified settings', goal: 500, unit: 'symbols', status: 'активен' } }),
    readRecoveryProject: vi.fn().mockRejectedValue(new BrowserReplayError('unsupported_history_family')),
    createStage: vi.fn(),
  } as unknown as BrowserCloudAccountSession
  const wrapper = mount(BrowserCloudProjectRead, {
    props: { session, projectId: 'fallback', autoOpenRecovery: true, workspace: true, section: 'project' },
    global: { plugins: [createPinia()] },
  })
  await flushPromises()
  expect(wrapper.get('[data-testid="cloud-project-details"]').get('h3').text()).toBe('Verified settings')
  expect(wrapper.get('[role="alert"]').attributes('data-read-code')).toBe('unsupported_history_family')
  expect(wrapper.find('[data-testid="cloud-stage-create"]').exists()).toBe(false)
  expect(wrapper.find('[data-testid="cloud-progress-read"]').exists()).toBe(false)
  wrapper.unmount()
})
