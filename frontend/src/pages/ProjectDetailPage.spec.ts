import { createPinia } from 'pinia'
import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { projectsApi } from '@/api/projects'
import { integrationsApi } from '@/api/integrations'
import { documentsApi } from '@/api/documents'
import { settingsApi } from '@/api/settings'
import { copyProgressImage, downloadProgressImage } from '@/platform/progressShare'
import { useNotificationsStore } from '@/stores/notifications'
import { gameStateFixture } from '@/test/gameFixtures'
import { projectFixture } from '@/test/fixtures'
import type { Statistics } from '@/types/api'

import ProjectDetailPage from './ProjectDetailPage.vue'

const pushRoute = vi.fn()
const replaceRoute = vi.fn()
const routeParams: { projectId: string; stageId?: string } = { projectId: 'project-id' }

vi.mock('vue-router', () => ({
  useRoute: () => ({ params: routeParams }),
  useRouter: () => ({ replace: replaceRoute, push: pushRoute }),
}))

vi.mock('@/api/integrations', () => ({
  integrationsApi: {
    getProjectSyncs: vi.fn(),
    runProjectSyncs: vi.fn(),
  },
}))

vi.mock('@/api/documents', () => ({
  documentsApi: { get: vi.fn() },
}))

vi.mock('@/api/projects', () => ({
  projectsApi: {
    createStage: vi.fn(),
    get: vi.fn(),
    globalStreak: vi.fn(),
    recordProgress: vi.fn(),
    removeStage: vi.fn(),
    statistics: vi.fn(),
  },
}))

vi.mock('@/api/settings', () => ({
  settingsApi: { get: vi.fn() },
}))

vi.mock('@/platform/progressShare', () => ({
  copyProgressImage: vi.fn(),
  downloadProgressImage: vi.fn(),
  progressShareTitle: (name: string, parentName?: string) => parentName ? `${parentName}: ${name}` : name,
}))

const statisticsFixture: Statistics = {
  entity_id: 'project-id',
  unit: 'symbols',
  metrics: {
    entries_count: 0,
    total: 0,
    average_symbols_per_active_day: 0,
    average_symbols_per_entry: 0,
    average_entries_per_active_day: 0,
    freezes_used: 0,
    best_day: null,
    best_weekday: null,
    current_streak: 0,
    max_streak: 0,
    days_since_start: 0,
    active_days: 0,
    active_days_percent: 0,
  },
  timeline: [],
}

function workspaceStubs() {
  return {
    IonContent: { template: '<div><slot /></div>' },
    IonIcon: true,
    IonPage: { template: '<div><slot /></div>' },
    IonSpinner: true,
    ProgressWorkspace: {
      props: ['textSymbols'],
      emits: ['record'],
      template: '<div><output class="text-symbols">{{ textSymbols }}</output><button class="progress-record" type="button" @click="$emit(\'record\', { new_total: 25100 })">record</button></div>',
    },
    ProjectEditDialog: true,
    ProgressShareMenu: {
      props: ['label'],
      emits: ['copy', 'save'],
      template: '<button :aria-label="label" type="button" @click="$emit(\'copy\')">copy</button>',
    },
    RouterLink: { template: '<a><slot /></a>' },
    StageDialog: {
      props: ['open', 'defaultName', 'sharedSource'],
      emits: ['submit'],
      template: `
        <button
          v-if="open"
          class="stage-dialog-submit"
          type="button"
          @click="$emit('submit', { name: defaultName, infinite: true, total: 0 })"
        >
          {{ sharedSource ? defaultName : 'stage' }}
        </button>
      `,
    },
    StageWorkspace: {
      props: ['project'],
      emits: ['copy', 'save'],
      template: '<button class="stage-share" type="button" @click="$emit(\'copy\', project.stages[0])">stage share</button>',
    },
    StatePanel: true,
    StatisticsWorkspace: true,
  }
}

function mountWorkspace(pinia = createPinia()) {
  return mount(ProjectDetailPage, {
    global: {
      plugins: [pinia],
      stubs: workspaceStubs(),
    },
  })
}

describe('ProjectDetailPage progress sharing', () => {
  beforeEach(() => {
    vi.mocked(projectsApi.get).mockReset()
    vi.mocked(projectsApi.createStage).mockReset()
    vi.mocked(projectsApi.removeStage).mockReset()
    vi.mocked(projectsApi.globalStreak).mockReset()
    vi.mocked(projectsApi.recordProgress).mockReset()
    vi.mocked(projectsApi.statistics).mockReset()
    vi.mocked(copyProgressImage).mockReset()
    vi.mocked(downloadProgressImage).mockReset()
    vi.mocked(integrationsApi.getProjectSyncs).mockReset()
    vi.mocked(integrationsApi.runProjectSyncs).mockReset()
    vi.mocked(documentsApi.get).mockReset()
    vi.mocked(settingsApi.get).mockReset()
    pushRoute.mockReset()
    replaceRoute.mockReset()
    delete routeParams.stageId
    const stage = projectFixture({
      id: 'stage-id',
      name: 'Глава 3',
      progress: 50,
      parent_project_id: 'project-id',
      work_method: 'sync',
    })
    vi.mocked(projectsApi.get).mockResolvedValue(projectFixture({
      id: 'project-id',
      name: 'Дом у моря',
      stages_enabled: true,
      stages: [stage],
    }))
    vi.mocked(projectsApi.statistics).mockResolvedValue(statisticsFixture)
    vi.mocked(projectsApi.globalStreak).mockResolvedValue({
      enabled: true,
      status: 'Go',
      length: 7,
      max_length: 18,
    })
    vi.mocked(copyProgressImage).mockResolvedValue(undefined)
    vi.mocked(downloadProgressImage).mockResolvedValue(undefined)
    vi.mocked(integrationsApi.getProjectSyncs).mockResolvedValue({
      project_id: 'project-id',
      syncs: [{
        project_id: 'project-id',
        stage_id: 'stage-id',
        configured: false,
        type: null,
        path: null,
        item_id: null,
        last_synced_at: null,
        desktop_only: true,
      }],
    })
    vi.mocked(documentsApi.get).mockResolvedValue({
      project_id: 'project-id', stage_id: null,
      content: { type: 'doc', content: [] }, exists: false, updated_at: null,
      docx_path: null, sync_state: 'unlinked', last_synced_hash: null,
      last_synced_at: null, local_dirty: false, word_dirty: false,
      symbols: 0, has_content: false,
    })
    vi.mocked(settingsApi.get).mockResolvedValue({
      values: { global_streak: true },
      platform: 'web',
      capabilities: {
        local_file_sync: false,
        background_file_sync: false,
        native_updates: false,
        remote_api: true,
      },
      editable_keys: ['global_streak'],
    })
  })

  it('loads the current application document directly for its progress counter', async () => {
    vi.mocked(projectsApi.get).mockResolvedValue(projectFixture({
      id: 'project-id', stages_enabled: false, stages: [], work_method: 'app',
    }))
    vi.mocked(documentsApi.get).mockResolvedValue({
      project_id: 'project-id', stage_id: null,
      content: { type: 'doc', content: [{ type: 'paragraph' }] }, exists: true,
      updated_at: '2026-08-31T13:00:00+00:00', docx_path: null,
      sync_state: 'unlinked', last_synced_hash: null, last_synced_at: null,
      local_dirty: false, word_dirty: false, symbols: 12, has_content: true,
    })
    const wrapper = mountWorkspace()
    await flushPromises()

    expect(documentsApi.get).toHaveBeenCalledWith({ projectId: 'project-id' })
    expect(wrapper.get('.text-symbols').text()).toContain('12')
  })

  it('creates cards locally for both a project and its selected stage', async () => {
    const pinia = createPinia()
    const wrapper = mountWorkspace(pinia)
    await flushPromises()

    await wrapper.get('button[aria-label="Поделиться прогрессом «Дом у моря»"]').trigger('click')
    await flushPromises()
    await wrapper.get('.stage-share').trigger('click')
    await flushPromises()

    expect(copyProgressImage).toHaveBeenNthCalledWith(1, {
      title: 'Дом у моря',
      progress: 25,
      coverImage: null,
      statusLabel: 'Активен',
      progressText: '25,000 символов / 100,000 символов',
      footerLabel: 'Apr 30, 2027',
      footerDetail: 'Этапов: 1',
      theme: 'light',
    })
    expect(copyProgressImage).toHaveBeenNthCalledWith(2, {
      title: 'Дом у моря: Глава 3',
      progress: 50,
      coverImage: null,
      statusLabel: 'Активен',
      progressText: '25,000 символов / 100,000 символов',
      footerLabel: 'Apr 30, 2027',
      footerDetail: undefined,
      theme: 'light',
    })
    expect(useNotificationsStore(pinia).notifications).toContainEqual(expect.objectContaining({
      kind: 'info',
      message: 'Картинка прогресса добавлена в буфер обмена',
    }))
  })

  it('renders the stage list before the progress records', async () => {
    const wrapper = mountWorkspace()
    await flushPromises()

    expect([...wrapper.element.querySelectorAll('.stage-share, .progress-record')].map((element) => element.className))
      .toEqual(['stage-share', 'progress-record'])
    wrapper.unmount()
  })

  it('does not render the global streak in project details', async () => {
    vi.mocked(projectsApi.get).mockResolvedValue(projectFixture({
      id: 'project-id',
      deadline: null,
      streak_enabled: false,
    }))

    const wrapper = mountWorkspace()
    await flushPromises()

    expect(projectsApi.globalStreak).not.toHaveBeenCalled()
    expect(wrapper.find('.detail-streak--global').exists()).toBe(false)
    wrapper.unmount()
  })

  it('shows game reward messages and refreshes notification history after progress', async () => {
    const nextProject = projectFixture({ id: 'project-id', total: 25_100 })
    vi.mocked(projectsApi.recordProgress).mockResolvedValue({
      project: nextProject,
      entry: {
        id: 'entry-id',
        new_total: 25_100,
        new_total_symbols: 25_100,
        added: 100,
        added_symbols: 100,
        added_progress: 0.1,
        created_at: '2026-08-23T18:00:00',
      },
      added_symbols: 100,
      warning: null,
      game: {
        ok: true,
        message: 'Получено 138 монет и 1 258 опыта.',
        messages: [
          'Получено 138 монет и 1 258 опыта.',
          'Глобальный стрик продлён: 7 дней.',
        ],
        result: { rewarded: true },
        state: gameStateFixture({
          notifications: {
            unread: [{
              id: 'game-event',
              text: 'Доступна новая награда.',
              tag: 'game',
              created_at: '2026-08-23T18:00:00',
              status: 'new',
            }],
            read: [],
            unread_count: 1,
          },
        }),
      },
    })
    const pinia = createPinia()
    const wrapper = mountWorkspace(pinia)
    await flushPromises()

    await wrapper.get('.progress-record').trigger('click')
    await flushPromises()

    const notificationStore = useNotificationsStore(pinia)
    expect(notificationStore.notifications).toEqual(expect.arrayContaining([
      expect.objectContaining({
        kind: 'success',
        message: 'В проект добавлено 100 символов',
      }),
      expect.objectContaining({ message: 'Получено 138 монет и 1 258 опыта.' }),
      expect.objectContaining({ message: 'Глобальный стрик продлён: 7 дней.' }),
    ]))
    expect(notificationStore.gameHistory.unread_count).toBe(1)
    wrapper.unmount()
  })

  it('does not enable a progress card for a goal-free project', async () => {
    vi.mocked(projectsApi.get).mockResolvedValue(projectFixture({
      id: 'project-id',
      infinite: true,
      goal: null,
    }))
    const pinia = createPinia()
    const wrapper = mountWorkspace(pinia)
    await flushPromises()

    const shareButton = wrapper.get('button[aria-label*="Поделиться прогрессом"]')
    expect(shareButton.attributes('disabled')).toBeDefined()
    expect(copyProgressImage).not.toHaveBeenCalled()
    expect(downloadProgressImage).not.toHaveBeenCalled()
  })

  it('renders the project streak in the project facts', async () => {
    vi.mocked(projectsApi.get).mockResolvedValue(projectFixture({
      id: 'project-id',
      streak_length: 7,
      max_streak: 11,
      streak_status: 'Go',
    }))
    const wrapper = mountWorkspace()
    await flushPromises()

    expect(wrapper.get('.detail-streak--entity').attributes('aria-label')).toContain('Стрик проекта')
    expect(wrapper.get('.detail-streak--entity').attributes('aria-label')).toContain('7 дн.')
    expect(wrapper.get('.detail-streak--entity').text()).toContain('Максимум: 11')
  })

  it('shows the stage daily target when a stage is open', async () => {
    routeParams.stageId = 'stage-id'
    const stage = projectFixture({
      id: 'stage-id',
      name: 'Глава 3',
      today_goal: 200,
      parent_project_id: 'project-id',
    })
    vi.mocked(projectsApi.get).mockResolvedValue(projectFixture({
      id: 'project-id',
      today_goal: 900,
      stages_enabled: true,
      stages: [stage],
    }))
    const wrapper = mountWorkspace()
    await flushPromises()

    const dailyGoal = wrapper.findAll('.fact-card')
      .find((card) => card.text().includes('Цель на сегодня'))
    expect(dailyGoal?.text()).toContain('200')
    expect(dailyGoal?.text()).not.toContain('900')
  })

  it('opens a stage document with the stage id in the route path', async () => {
    routeParams.stageId = 'stage-id'
    const stage = projectFixture({
      id: 'stage-id',
      name: 'Глава 3',
      parent_project_id: 'project-id',
      work_method: 'app',
    })
    vi.mocked(projectsApi.get).mockResolvedValue(projectFixture({
      id: 'project-id',
      stages_enabled: true,
      stages: [stage],
    }))
    const wrapper = mountWorkspace()
    await flushPromises()

    const textButton = wrapper.findAll('button').find((button) => button.text().includes('Текст'))
    expect(textButton).toBeDefined()
    await textButton?.trigger('click')

    expect(pushRoute).toHaveBeenCalledWith({
      name: 'stage-document',
      params: { projectId: 'project-id', stageId: 'stage-id' },
      query: { title: 'Глава 3' },
    })
    wrapper.unmount()
  })

  it('keeps project sync visible and opens setup for the selected stage', async () => {
    const wrapper = mountWorkspace()
    await flushPromises()

    await wrapper.get('.project-sync-button').trigger('click')
    await flushPromises()

    expect(integrationsApi.getProjectSyncs).toHaveBeenCalledWith('project-id')
    expect(pushRoute).toHaveBeenCalledWith({
      name: 'integrations',
      query: { projectId: 'project-id', stageId: 'stage-id' },
    })
  })

  it('keeps source synchronization available in the shared project', async () => {
    const source = projectFixture({
      id: 'source-id',
      name: 'Источник 1',
      infinite: true,
      goal: null,
      parent_project_id: 'project-id',
      work_method: 'sync',
    })
    const addedSource = projectFixture({
      id: 'source-id-2',
      name: 'Источник 2',
      infinite: true,
      goal: null,
      parent_project_id: 'project-id',
      work_method: 'sync',
    })
    let sharedProject = projectFixture({
      id: 'project-id',
      name: 'Общий проект',
      infinite: true,
      goal: null,
      stages_enabled: true,
      stages: [source],
    })
    vi.mocked(projectsApi.get).mockImplementation(async () => sharedProject)
    vi.mocked(projectsApi.createStage).mockImplementation(async () => {
      sharedProject = { ...sharedProject, stages: [source, addedSource] }
      return sharedProject
    })
    vi.mocked(integrationsApi.getProjectSyncs).mockResolvedValue({
      project_id: 'project-id',
      syncs: [{
        project_id: 'project-id',
        stage_id: 'source-id',
        configured: false,
        type: null,
        path: null,
        item_id: null,
        last_synced_at: null,
        desktop_only: true,
      }],
    })
    const wrapper = mountWorkspace()
    await flushPromises()

    const addSourceButton = wrapper.get('.project-add-source-button')
    expect(addSourceButton.text()).toContain('Добавить источник')
    await addSourceButton.trigger('click')
    await wrapper.get('.stage-dialog-submit').trigger('click')
    await flushPromises()

    expect(projectsApi.createStage).toHaveBeenCalledWith('project-id', {
      name: 'Источник 2',
      infinite: true,
      total: 0,
    })

    await wrapper.get('.project-sync-button').trigger('click')
    await flushPromises()

    expect(pushRoute).toHaveBeenCalledWith({
      name: 'integrations',
      query: { projectId: 'project-id', stageId: 'source-id-2' },
    })
    wrapper.unmount()
  })

  it('allows deleting a shared-project source from its detail page', async () => {
    routeParams.stageId = 'source-id'
    const source = projectFixture({
      id: 'source-id',
      name: 'Источник 1',
      infinite: true,
      goal: null,
      parent_project_id: 'project-id',
      work_method: 'sync',
    })
    const sharedProject = projectFixture({
      id: 'project-id',
      name: 'Общий проект',
      infinite: true,
      goal: null,
      stages_enabled: true,
      stages: [source],
    })
    vi.mocked(projectsApi.get).mockResolvedValue(sharedProject)
    vi.mocked(projectsApi.removeStage).mockResolvedValue(undefined)
    vi.spyOn(window, 'confirm').mockReturnValue(true)
    const wrapper = mountWorkspace()
    await flushPromises()

    const deleteButton = wrapper.get('.project-delete-button')
    expect(deleteButton.text()).toContain('Удалить источник')
    await deleteButton.trigger('click')
    await flushPromises()

    expect(projectsApi.removeStage).toHaveBeenCalledWith('project-id', 'source-id')
    expect(replaceRoute).toHaveBeenCalledWith({
      name: 'project-detail',
      params: { projectId: 'project-id' },
    })
    wrapper.unmount()
  })

  it('runs an existing stage binding instead of offering a second connection', async () => {
    vi.mocked(integrationsApi.getProjectSyncs).mockResolvedValue({
      project_id: 'project-id',
      syncs: [{
        project_id: 'project-id',
        stage_id: 'stage-id',
        configured: true,
        type: 'scrivener',
        path: '/existing/book.scriv',
        item_id: 'binder-id',
        last_synced_at: null,
        desktop_only: true,
      }],
    })
    vi.mocked(integrationsApi.runProjectSyncs).mockResolvedValue({
      checked: 1,
      changed: 1,
      failed: 0,
      items: [{
        project_id: 'project-id',
        stage_id: 'stage-id',
        ok: true,
        changed: true,
        symbols: 100,
        progress: {
          project: projectFixture({ id: 'project-id' }),
          entry: {
            id: 'sync-entry',
            new_total: 100,
            new_total_symbols: 100,
            added: 100,
            added_symbols: 100,
            added_progress: 1,
            created_at: '2026-08-23T18:00:00',
          },
          added_symbols: 100,
          warning: null,
          game: {
            ok: true,
            message: 'Получена игровая награда за синхронизацию.',
            messages: ['Получена игровая награда за синхронизацию.'],
            result: { rewarded: true },
            state: gameStateFixture(),
          },
        },
        error: null,
      }],
    })
    const pinia = createPinia()
    const wrapper = mountWorkspace(pinia)
    await flushPromises()

    await wrapper.get('.project-sync-button').trigger('click')
    await flushPromises()

    expect(integrationsApi.runProjectSyncs).toHaveBeenCalledWith('project-id')
    expect(pushRoute).not.toHaveBeenCalled()
    expect(wrapper.text()).toContain('Синхронизация завершена')
    expect(useNotificationsStore(pinia).notifications).toContainEqual(expect.objectContaining({
      kind: 'success',
      message: 'Синхронизация завершена',
    }))
    expect(useNotificationsStore(pinia).notifications).toContainEqual(expect.objectContaining({
      kind: 'success',
      message: 'Получена игровая награда за синхронизацию.',
    }))
  })
})
