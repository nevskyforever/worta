import { createPinia } from 'pinia'
import { mount } from '@vue/test-utils'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { projectFixture } from '@/test/fixtures'

import ProjectEditDialog from './ProjectEditDialog.vue'

const ionicStubs = {
  IonModal: { template: '<div><slot /></div>' },
  IonHeader: { template: '<header><slot /></header>' },
  IonContent: { template: '<main><slot /></main>' },
  IonIcon: true,
  IonSpinner: true,
}

describe('ProjectEditDialog', () => {
  afterEach(() => {
    vi.useRealTimers()
    vi.restoreAllMocks()
  })

  it('preserves the current-day plan when the daily goal changes', async () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-08-26T12:00:00'))
    const wrapper = mount(ProjectEditDialog, {
      props: {
        open: true,
        project: projectFixture({
          goal: 2_000, total: 400, personal_goal: 200,
          plan_daily_goal: 200, today_goal: 600, deadline: '2026-09-02',
        }),
      },
      global: { plugins: [createPinia()], stubs: ionicStubs },
    })

    await wrapper.get('#edit-project-personal-goal').setValue('300')
    expect((wrapper.get('#edit-project-deadline').element as HTMLInputElement).value)
      .toBe('2026-08-31')
  })

  it('recalculates the deadline for a project that already has stages', async () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-08-26T12:00:00'))
    const stage = projectFixture({ id: 'stage-1', name: 'Черновик' })
    const wrapper = mount(ProjectEditDialog, {
      props: {
        open: true,
        project: projectFixture({
          goal: 2_000, total: 400, personal_goal: 200,
          plan_daily_goal: 200, today_goal: 600, deadline: '2026-09-02',
          stages_enabled: true, stages: [stage],
        }),
      },
      global: { plugins: [createPinia()], stubs: ionicStubs },
    })

    await wrapper.get('#edit-project-personal-goal').setValue('300')

    expect((wrapper.get('#edit-project-deadline').element as HTMLInputElement).value)
      .toBe('2026-08-31')
  })

  it('converts values and recalculates the plan when the unit changes', async () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-08-26T12:00:00'))
    const wrapper = mount(ProjectEditDialog, {
      props: {
        open: true,
        project: projectFixture({
          goal: 40_000, total: 20_000, personal_goal: 10_000,
          deadline: '2026-08-27',
        }),
      },
      global: { plugins: [createPinia()], stubs: ionicStubs },
    })

    await wrapper.get('#edit-project-unit').setValue('author_list')
    expect((wrapper.get('#edit-project-goal').element as HTMLInputElement).value).toBe('1')
    expect((wrapper.get('#edit-project-total').element as HTMLInputElement).value).toBe('0.5')
    expect((wrapper.get('#edit-project-personal-goal').element as HTMLInputElement).value).toBe('0.3')
  })

  it('updates the current value without forcing a daily-plan reset', async () => {
    const wrapper = mount(ProjectEditDialog, {
      props: { open: true, project: projectFixture({ total: 1_200 }) },
      global: { plugins: [createPinia()], stubs: ionicStubs },
    })

    await wrapper.get('#edit-project-total').setValue('1500')
    await wrapper.get('form').trigger('submit')

    expect(wrapper.emitted('submit')?.[0]?.[0]).toMatchObject({ total: 1_500 })
    expect(wrapper.emitted('submit')?.[0]?.[0]).not.toHaveProperty('recalculate_plan')
  })

  it('keeps the cover control across both columns of the edit form', () => {
    const wrapper = mount(ProjectEditDialog, {
      props: { open: true, project: projectFixture() },
      global: { plugins: [createPinia()], stubs: ionicStubs },
    })

    expect(wrapper.get('.cover-editor').classes()).toContain('workspace-field--wide')
  })

  it('loads and updates local streak settings', async () => {
    const wrapper = mount(ProjectEditDialog, {
      props: {
        open: true,
        globalStreakEnabled: true,
        gameModeEnabled: true,
        project: projectFixture({ streak_enabled: true, auto_freeze: true }),
      },
      global: { plugins: [createPinia()], stubs: ionicStubs },
    })

    expect((wrapper.get('#edit-project-streak-enabled').element as HTMLInputElement).checked).toBe(true)
    expect((wrapper.get('#edit-project-auto-freeze').element as HTMLInputElement).checked).toBe(true)
    await wrapper.get('#edit-project-streak-enabled').setValue(false)
    await wrapper.get('form').trigger('submit')

    expect(wrapper.emitted('submit')?.[0]?.[0]).toMatchObject({ streak_enabled: false })
  })

  it('confirms and requests the preserving stages-to-single conversion', async () => {
    vi.spyOn(window, 'confirm').mockReturnValue(true)
    const stage = projectFixture({ id: 'stage-1', name: 'Черновик' })
    const wrapper = mount(ProjectEditDialog, {
      props: {
        open: true,
        project: projectFixture({ stages_enabled: true, stages: [stage] }),
      },
      global: { plugins: [createPinia()], stubs: ionicStubs },
    })

    const stagesToggle = wrapper.findAll('label').find((item) => item.text().includes('Проект с этапами'))
    await stagesToggle?.get('input').setValue(false)
    await wrapper.get('form').trigger('submit')

    expect(window.confirm).toHaveBeenCalledOnce()
    expect(wrapper.emitted('submit')?.[0]?.[0]).toMatchObject({ stages_enabled: false })
  })
})
