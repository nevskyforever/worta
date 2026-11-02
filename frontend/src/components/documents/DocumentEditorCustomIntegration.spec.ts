import { flushPromises, mount } from '@vue/test-utils'
import { createPinia } from 'pinia'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { documentsApi } from '@/api/documents'
import { projectsApi } from '@/api/projects'
import { projectFixture } from '@/test/fixtures'
import type { ProjectDocument, TiptapDocument } from '@/types/documents'
import NFDocumentEditor from './editor/NFDocumentEditor.vue'
import DocumentEditorView from './DocumentEditorView.vue'

const { onBeforeRouteLeave } = vi.hoisted(() => ({ onBeforeRouteLeave: vi.fn() }))
const positionStorage = new Map<string, string>()

vi.mock('vue-router', () => ({
  onBeforeRouteLeave,
  useRouter: () => ({ push: vi.fn() }),
}))
vi.mock('@/api/projects', () => ({ projectsApi: { get: vi.fn() } }))
vi.mock('@/platform/runtime', () => ({ currentPlatform: vi.fn(() => 'tauri') }))
vi.mock('@/services/documentDocx', () => ({
  blobToBase64: vi.fn(),
  exportDocx: vi.fn(async () => new Blob(['copy'])),
  importDocx: vi.fn(),
}))
vi.mock('@/api/documents', () => ({
  documentsApi: {
    acceptWord: vi.fn(),
    external: vi.fn(),
    get: vi.fn(),
    link: vi.fn(),
    recordProgress: vi.fn(),
    resolveExternal: vi.fn(),
    save: vi.fn(),
    writeDocx: vi.fn(),
    writeDocxContent: vi.fn(),
    parseWord: vi.fn(),
  },
}))

const initialContent: TiptapDocument = {
  type: 'doc',
  content: [{ type: 'paragraph', content: [{ type: 'text', text: 'Исходный текст' }] }],
}
const documentFixture: ProjectDocument = {
  project_id: 'project-id',
  stage_id: null,
  content: initialContent,
  exists: true,
  updated_at: null,
  docx_path: null,
  sync_state: 'unlinked',
  last_synced_hash: null,
  last_synced_at: null,
  local_dirty: false,
  word_dirty: false,
  symbols: 14,
  has_content: true,
}

type CustomEditorExpose = {
  getJSON: () => TiptapDocument
  getSelection: () => number | null
  getScrollContainer: () => HTMLElement | null
  setContent: (content: TiptapDocument, emitUpdate?: boolean) => void
}

function mountEditor(project = projectFixture(), document = documentFixture) {
  vi.mocked(projectsApi.get).mockResolvedValue(project)
  vi.mocked(documentsApi.get).mockResolvedValue(document)
  vi.mocked(documentsApi.save).mockResolvedValue(document)
  vi.mocked(documentsApi.writeDocxContent).mockResolvedValue(document)
  return mount(DocumentEditorView, {
    props: { scope: { projectId: 'project-id' }, title: 'Текст' },
    global: { plugins: [createPinia()] },
  })
}

describe('DocumentEditorView custom editor integration', () => {
  beforeEach(() => {
    vi.useFakeTimers()
    Object.defineProperty(URL, 'createObjectURL', { configurable: true, value: vi.fn(() => 'blob:copy') })
    Object.defineProperty(URL, 'revokeObjectURL', { configurable: true, value: vi.fn() })
    vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(() => undefined)
    positionStorage.clear()
    Object.defineProperty(window, 'localStorage', {
      configurable: true,
      value: {
        getItem: (key: string) => positionStorage.get(key) ?? null,
        removeItem: (key: string) => { positionStorage.delete(key) },
        setItem: (key: string, value: string) => { positionStorage.set(key, value) },
      },
    })
    vi.mocked(documentsApi.get).mockReset()
    vi.mocked(documentsApi.save).mockReset()
    vi.mocked(documentsApi.recordProgress).mockReset()
    vi.mocked(documentsApi.resolveExternal).mockReset()
    vi.mocked(documentsApi.external).mockReset()
    vi.mocked(documentsApi.acceptWord).mockReset()
    vi.mocked(documentsApi.parseWord).mockReset()
    vi.mocked(documentsApi.writeDocxContent).mockReset()
    vi.mocked(projectsApi.get).mockReset()
  })

  afterEach(() => {
    vi.useRealTimers()
    vi.restoreAllMocks()
  })

  it('loads the document and autosaves emitted JSON through useDocumentSync', async () => {
    const wrapper = mountEditor()
    await flushPromises()
    const customEditor = wrapper.getComponent(NFDocumentEditor)
    const api = customEditor.vm as unknown as CustomEditorExpose
    expect(api.getJSON()).toMatchObject(initialContent)

    const next: TiptapDocument = {
      type: 'doc',
      content: [{ type: 'paragraph', content: [{ type: 'text', text: 'Черновик' }] }],
    }
    api.setContent(next, true)
    await vi.advanceTimersByTimeAsync(700)

    expect(documentsApi.save).toHaveBeenCalledTimes(1)
    expect(vi.mocked(documentsApi.save).mock.calls[0]?.[0]).toEqual({ projectId: 'project-id' })
    expect(vi.mocked(documentsApi.save).mock.calls[0]?.[1]).toMatchObject(next)
    wrapper.unmount()
  })

  it('restores the custom editor selection and viewport scroll position', async () => {
    window.localStorage.setItem(
      'nfprogress:document-position:project-id:project',
      JSON.stringify({ selection: 4, scrollTop: 37 }),
    )
    const wrapper = mountEditor()
    await flushPromises()
    await vi.advanceTimersByTimeAsync(50)
    await flushPromises()

    const api = wrapper.getComponent(NFDocumentEditor).vm as unknown as CustomEditorExpose
    expect(api.getSelection()).toBe(4)
    expect(api.getScrollContainer()?.scrollTop).toBe(37)
    expect(wrapper.getComponent(NFDocumentEditor).props('zoom')).toBe(100)
    expect(wrapper.getComponent(NFDocumentEditor).props('typewriterMode')).toBe(false)
    wrapper.unmount()
  })

  it('restores zoom and Typewriter before the saved selection and scroll position', async () => {
    window.localStorage.setItem(
      'nfprogress:document-position:project-id:project',
      JSON.stringify({ selection: 6, scrollTop: 83, zoom: 170, typewriterMode: true }),
    )
    const wrapper = mountEditor()
    await flushPromises()

    const customEditor = wrapper.getComponent(NFDocumentEditor)
    expect(customEditor.props('zoom')).toBe(170)
    expect(customEditor.props('typewriterMode')).toBe(true)
    expect(wrapper.get('.document-editor-view__typewriter-toggle').attributes('aria-pressed')).toBe('true')
    expect(wrapper.get('.document-editor-view__zoom').text()).toContain('170%')

    await vi.advanceTimersByTimeAsync(50)
    await flushPromises()
    const api = customEditor.vm as unknown as CustomEditorExpose
    expect(api.getSelection()).toBe(6)
    expect(api.getScrollContainer()?.scrollTop).toBe(83)
    wrapper.unmount()
  })

  it('repairs invalid optional view state fields with backward-compatible defaults', async () => {
    window.localStorage.setItem(
      'nfprogress:document-position:project-id:project',
      JSON.stringify({ selection: 3, scrollTop: 21, zoom: 900, typewriterMode: 'yes' }),
    )
    const wrapper = mountEditor()
    await flushPromises()
    await vi.advanceTimersByTimeAsync(50)
    await flushPromises()

    const customEditor = wrapper.getComponent(NFDocumentEditor)
    const api = customEditor.vm as unknown as CustomEditorExpose
    expect(customEditor.props('zoom')).toBe(100)
    expect(customEditor.props('typewriterMode')).toBe(false)
    expect(api.getSelection()).toBe(3)
    expect(api.getScrollContainer()?.scrollTop).toBe(21)
    wrapper.unmount()
  })

  it('saves zoom and Typewriter through the existing per-document view state', async () => {
    const wrapper = mountEditor()
    await flushPromises()

    await wrapper.get('.document-editor-view__zoom button:last-child').trigger('click')
    await wrapper.get('.document-editor-view__typewriter-toggle').trigger('click')
    await vi.advanceTimersByTimeAsync(250)

    expect(JSON.parse(positionStorage.get('nfprogress:document-position:project-id:project') ?? '{}')).toMatchObject({
      version: 1,
      zoom: 110,
      typewriterMode: true,
    })
    wrapper.unmount()
  })

  it('keeps non-empty nfprogress content pending when background polling detects changed Word content', async () => {
    const linkedDocument = { ...documentFixture, docx_path: '/tmp/document.docx', last_synced_hash: 'accepted-hash' }
    const wordDocument: TiptapDocument = {
      type: 'doc',
      content: [{ type: 'paragraph', content: [{ type: 'text', text: 'Текст Word' }] }],
    }
    vi.mocked(documentsApi.external)
      .mockResolvedValueOnce({ state: 'external_changed', content_base64: 'AQI=', hash: 'word-hash' })
      .mockResolvedValue({ state: 'synced' })
    vi.mocked(documentsApi.parseWord).mockResolvedValue({ content: wordDocument, symbols: 10, hash: 'word-hash' })
    const wrapper = mountEditor(projectFixture(), linkedDocument)
    await flushPromises()

    await vi.advanceTimersByTimeAsync(5000)
    await flushPromises()

    const api = wrapper.getComponent(NFDocumentEditor).vm as unknown as CustomEditorExpose
    expect(api.getJSON()).toMatchObject(initialContent)
    expect(wrapper.find('[role="alertdialog"]').exists()).toBe(true)
    expect(documentsApi.acceptWord).not.toHaveBeenCalled()
    wrapper.unmount()
  })

  it('never clears non-empty nfprogress content when changed Word content is empty', async () => {
    const linkedDocument = { ...documentFixture, docx_path: '/tmp/document.docx', last_synced_hash: 'accepted-hash' }
    const emptyWord: TiptapDocument = { type: 'doc', content: [{ type: 'paragraph' }] }
    vi.mocked(documentsApi.external).mockResolvedValue({ state: 'external_changed', content_base64: 'AQI=', hash: 'empty-word-hash' })
    vi.mocked(documentsApi.parseWord).mockResolvedValue({ content: emptyWord, symbols: 0, hash: 'empty-word-hash' })
    const wrapper = mountEditor(projectFixture(), linkedDocument)
    await flushPromises()

    await vi.advanceTimersByTimeAsync(5000)
    await flushPromises()

    const api = wrapper.getComponent(NFDocumentEditor).vm as unknown as CustomEditorExpose
    expect(api.getJSON()).toMatchObject(initialContent)
    expect(wrapper.find('[role="alertdialog"]').exists()).toBe(true)
    expect(documentsApi.acceptWord).not.toHaveBeenCalled()
    wrapper.unmount()
  })

  it('applies changed Word content only after explicit acceptance', async () => {
    const linkedDocument = { ...documentFixture, docx_path: '/tmp/document.docx', last_synced_hash: 'accepted-hash' }
    const wordDocument: TiptapDocument = {
      type: 'doc',
      content: [{ type: 'paragraph', content: [{ type: 'text', text: 'Принятый Word' }] }],
    }
    const acceptedDocument = { ...linkedDocument, content: wordDocument, sync_state: 'synced', last_synced_hash: 'word-hash' }
    vi.mocked(documentsApi.external).mockResolvedValue({ state: 'external_changed', content_base64: 'AQI=', hash: 'word-hash' })
    vi.mocked(documentsApi.parseWord).mockResolvedValue({ content: wordDocument, symbols: 13, hash: 'word-hash' })
    vi.mocked(documentsApi.acceptWord).mockResolvedValue(acceptedDocument)
    const wrapper = mountEditor(projectFixture(), linkedDocument)
    await flushPromises()
    await vi.advanceTimersByTimeAsync(5000)
    await flushPromises()

    const acceptButton = wrapper.findAll('[role="alertdialog"] button').find((button) => button.text() === 'Принять Word')
    expect(acceptButton).toBeDefined()
    await acceptButton!.trigger('click')
    await flushPromises()

    const api = wrapper.getComponent(NFDocumentEditor).vm as unknown as CustomEditorExpose
    expect(api.getJSON()).toMatchObject(wordDocument)
    expect(documentsApi.acceptWord).toHaveBeenCalledWith({ projectId: 'project-id' }, expect.objectContaining(wordDocument), 'word-hash')
    wrapper.unmount()
  })

  it('keeps nfprogress content and writes it to Word after explicit nfprogress choice', async () => {
    const linkedDocument = { ...documentFixture, docx_path: '/tmp/document.docx', last_synced_hash: 'accepted-hash' }
    const syncedDocument = { ...linkedDocument, sync_state: 'synced', last_synced_hash: 'nfprogress-hash' }
    const wordDocument: TiptapDocument = {
      type: 'doc',
      content: [{ type: 'paragraph', content: [{ type: 'text', text: 'Не принимать' }] }],
    }
    vi.mocked(documentsApi.external)
      .mockResolvedValueOnce({ state: 'external_changed', content_base64: 'AQI=', hash: 'word-hash' })
      .mockResolvedValue({ state: 'synced' })
    vi.mocked(documentsApi.parseWord).mockResolvedValue({ content: wordDocument, symbols: 11, hash: 'word-hash' })
    vi.mocked(documentsApi.acceptWord).mockResolvedValue({
      ...linkedDocument,
      sync_state: 'synced',
      last_synced_hash: 'word-hash',
    })
    vi.mocked(documentsApi.resolveExternal).mockResolvedValue(syncedDocument)
    const wrapper = mountEditor(projectFixture(), linkedDocument)
    await flushPromises()
    await vi.advanceTimersByTimeAsync(5000)
    await flushPromises()

    const keepButton = wrapper.findAll('[role="alertdialog"] button').find((button) => button.text() === 'Оставить worta')
    expect(keepButton).toBeDefined()
    await keepButton!.trigger('click')
    await flushPromises()

    const api = wrapper.getComponent(NFDocumentEditor).vm as unknown as CustomEditorExpose
    expect(api.getJSON()).toMatchObject(initialContent)
    expect(documentsApi.resolveExternal).toHaveBeenCalledOnce()
    expect(documentsApi.resolveExternal).toHaveBeenCalledWith({ projectId: 'project-id' }, 'cloud')
    expect(documentsApi.acceptWord).not.toHaveBeenCalled()

    await vi.advanceTimersByTimeAsync(5000)
    await flushPromises()
    expect(documentsApi.parseWord).toHaveBeenCalledOnce()
    expect(wrapper.find('[role="alertdialog"]').exists()).toBe(false)
    wrapper.unmount()
  })

  it('saves an nfprogress copy before applying Word after the explicit both choice', async () => {
    const linkedDocument = { ...documentFixture, docx_path: '/tmp/document.docx', last_synced_hash: 'accepted-hash' }
    const wordDocument: TiptapDocument = {
      type: 'doc',
      content: [{ type: 'paragraph', content: [{ type: 'text', text: 'Word canonical' }] }],
    }
    vi.mocked(documentsApi.external).mockResolvedValue({ state: 'conflict', content_base64: 'AQI=', hash: 'word-hash' })
    vi.mocked(documentsApi.parseWord).mockResolvedValue({ content: wordDocument, symbols: 14, hash: 'word-hash' })
    vi.mocked(documentsApi.acceptWord).mockResolvedValue({
      ...linkedDocument,
      content: wordDocument,
      sync_state: 'synced',
      last_synced_hash: 'word-hash',
    })
    const wrapper = mountEditor(projectFixture(), linkedDocument)
    await flushPromises()
    await vi.advanceTimersByTimeAsync(5000)
    await flushPromises()

    const bothButton = wrapper.findAll('[role="alertdialog"] button').find((button) => button.text() === 'Сохранить обе')
    expect(bothButton).toBeDefined()
    await bothButton!.trigger('click')
    await flushPromises()

    const api = wrapper.getComponent(NFDocumentEditor).vm as unknown as CustomEditorExpose
    expect(URL.createObjectURL).toHaveBeenCalledOnce()
    expect(api.getJSON()).toMatchObject(wordDocument)
    expect(documentsApi.acceptWord).toHaveBeenCalledWith({ projectId: 'project-id' }, expect.objectContaining(wordDocument), 'word-hash')
    wrapper.unmount()
  })

  it('keeps document and daily-goal progress in the shared status bar', async () => {
    const wrapper = mountEditor(projectFixture({
      total: 0,
      goal: 100,
      today_goal: 100,
      plan_daily_goal: 100,
      added_today: 40,
    }))
    await flushPromises()

    expect(wrapper.get('.document-editor-view__unit-count').text()).toContain('14 / 100')
    expect(wrapper.get('[role="progressbar"]').attributes('aria-valuenow')).toBe('54')
    wrapper.unmount()
  })

  it('records the exact current editor snapshot through the existing progress flow', async () => {
    vi.mocked(documentsApi.recordProgress).mockResolvedValue({
      changed: true,
      symbols: 8,
      progress: null,
      document: documentFixture,
    })
    const wrapper = mountEditor(projectFixture({ total: 0 }))
    await flushPromises()
    const api = wrapper.getComponent(NFDocumentEditor).vm as unknown as CustomEditorExpose
    const next: TiptapDocument = {
      type: 'doc',
      content: [{ type: 'paragraph', content: [{ type: 'text', text: 'Черновик' }] }],
    }
    api.setContent(next, true)
    await wrapper.vm.$nextTick()

    const recordButton = wrapper.findAll('button').find((button) => button.text().includes('Добавить запись'))
    expect(recordButton).toBeDefined()
    await recordButton!.trigger('click')
    await flushPromises()

    expect(documentsApi.recordProgress).toHaveBeenCalledTimes(1)
    expect(vi.mocked(documentsApi.recordProgress).mock.calls[0]?.[0]).toEqual({ projectId: 'project-id' })
    expect(vi.mocked(documentsApi.recordProgress).mock.calls[0]?.[1]).toMatchObject(next)
    wrapper.unmount()
  })
})
