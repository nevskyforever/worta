import { createPinia } from 'pinia'
import { useProjectsStore } from '@/stores/projects'
import { projectFixture } from '@/test/fixtures'
import { mount } from '@vue/test-utils'
import { defineComponent, reactive } from 'vue'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import DocumentPage from './DocumentPage.vue'

const route = reactive({
  name: 'document',
  params: { projectId: 'project-id' } as Record<string, string>,
  query: {} as Record<string, string>,
})

vi.mock('vue-router', () => ({
  useRoute: () => route,
}))

vi.mock('@/stores/locale', () => ({
  useLocaleStore: () => ({ translate: (value: string) => value }),
}))

function mountPage() {
  return mount(DocumentPage, {
    global: {
      plugins: [createPinia()],
      stubs: {
        IonContent: { template: '<div><slot /></div>' },
        IonPage: { template: '<div><slot /></div>' },
        DocumentEditor: defineComponent({
          name: 'DocumentEditor',
          props: ['scope', 'title'],
          template: '<div class="document-editor-stub" />',
        }),
      },
    },
  })
}

describe('DocumentPage route scope', () => {
  beforeEach(() => {
    route.name = 'document'
    route.params = { projectId: 'project-id' }
    route.query = {}
  })

  it('keeps a project document authoritative even with a stale stageId query', () => {
    route.query = { stageId: 'stale-stage' }

    const editor = mountPage().getComponent({ name: 'DocumentEditor' })

    expect(editor.props('scope')).toEqual({ projectId: 'project-id', stageId: undefined })
    expect(editor.props('title')).toBe('Текст проекта')
  })

  it.each([['Роман', 'Текст этапа'], ['Общий проект', 'Текст источника']])('names a stage document in %s', async (name, title) => {
    route.name = 'stage-document'
    route.params = { projectId: 'project-id', stageId: 'stage-id' }
    route.query = { stageId: 'stale-stage' }

    const wrapper = mountPage()
    useProjectsStore().currentProject = projectFixture({ id: 'project-id', name })
    const editor = wrapper.getComponent({ name: 'DocumentEditor' })

    expect(editor.props('scope')).toEqual({ projectId: 'project-id', stageId: 'stage-id' })
    await wrapper.vm.$nextTick()
    expect(editor.props('title')).toBe(title)
  })
})
