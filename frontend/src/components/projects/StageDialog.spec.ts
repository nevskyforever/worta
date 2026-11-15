import { createPinia } from 'pinia'
import { mount } from '@vue/test-utils'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { projectFixture } from '@/test/fixtures'

import StageDialog from './StageDialog.vue'

const ionicStubs = {
  IonModal: { template: '<div><slot /></div>' },
  IonHeader: { template: '<header><slot /></header>' },
  IonContent: { template: '<main><slot /></main>' },
  IonIcon: true,
  IonSpinner: true,
}

describe('StageDialog', () => {
  afterEach(() => vi.useRealTimers())

  it.each([
    { sharedSource: false, heading: 'Добавить этап' },
    { sharedSource: true, heading: 'Добавить источник' },
  ])('names the creation dialog for sharedSource=$sharedSource', ({ sharedSource, heading }) => {
    const wrapper = mount(StageDialog, {
      props: { open: true, projectUnit: 'symbols', sharedSource },
      global: { plugins: [createPinia()], stubs: ionicStubs },
    })
    expect(wrapper.get('h2').text()).toBe(heading)
  })

  it('keeps deadline and daily goal calculations live for a stage', async () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-08-26T12:00:00'))
    const wrapper = mount(StageDialog, {
      props: { open: true, projectUnit: 'symbols' },
      global: { plugins: [createPinia()], stubs: ionicStubs },
    })

    await wrapper.get('#stage-goal').setValue('1000')
    await wrapper.get('#stage-total').setValue('100')
    await wrapper.get('input[name="no_deadline"]').setValue(false)
    await wrapper.get('#stage-deadline').setValue('2026-08-28')
    expect((wrapper.get('#stage-personal-goal').element as HTMLInputElement).value).toBe('300')
  })

  it('preserves the current-day stage plan when its daily goal changes', async () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-08-26T12:00:00'))
    const wrapper = mount(StageDialog, {
      props: {
        open: true,
        projectUnit: 'symbols',
        stage: projectFixture({
          goal: 2_000, total: 400, personal_goal: 200,
          plan_daily_goal: 200, today_goal: 600, deadline: '2026-09-02',
        }),
      },
      global: { plugins: [createPinia()], stubs: ionicStubs },
    })

    await wrapper.get('#stage-personal-goal').setValue('300')
    expect((wrapper.get('#stage-deadline').element as HTMLInputElement).value)
      .toBe('2026-08-31')
  })

  it('recalculates from current progress when the stage deadline is today', async () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-09-19T12:00:00'))
    const wrapper = mount(StageDialog, {
      props: {
        open: true,
        projectUnit: 'symbols',
        stage: projectFixture({
          goal: 16_000, total: 14_464, personal_goal: 500,
          plan_daily_goal: 500, today_goal: 16_000, deadline: '2026-09-19',
          planning_date: '2026-09-19',
        }),
      },
      global: { plugins: [createPinia()], stubs: ionicStubs },
    })

    await wrapper.get('#stage-personal-goal').setValue('250')

    expect((wrapper.get('#stage-deadline').element as HTMLInputElement).value)
      .toBe('2026-09-25')
  })

  it('carries an edited current value without implicitly recalculating the plan', async () => {
    const wrapper = mount(StageDialog, {
      props: {
        open: true,
        projectUnit: 'symbols',
        stage: projectFixture({ id: 'stage-1', total: 200 }),
      },
      global: { plugins: [createPinia()], stubs: ionicStubs },
    })

    await wrapper.get('#stage-total').setValue('350')
    await wrapper.get('form').trigger('submit')

    expect(wrapper.emitted('submit')?.[0]?.[0]).toMatchObject({ total: 350 })
    expect(wrapper.emitted('submit')?.[0]?.[0]).not.toHaveProperty('recalculate_plan')
  })

  it('enables and persists local streak tracking for stages', async () => {
    const wrapper = mount(StageDialog, {
      props: {
        open: true,
        projectUnit: 'symbols',
        globalStreakEnabled: true,
        gameModeEnabled: true,
        stage: projectFixture({ id: 'stage-1', streak_enabled: true, auto_freeze: true }),
      },
      global: { plugins: [createPinia()], stubs: ionicStubs },
    })

    expect((wrapper.get('#stage-streak-enabled').element as HTMLInputElement).checked).toBe(true)
    expect((wrapper.get('#stage-auto-freeze').element as HTMLInputElement).checked).toBe(true)
    await wrapper.get('#stage-streak-enabled').setValue(false)
    await wrapper.get('form').trigger('submit')

    expect(wrapper.emitted('submit')?.[0]?.[0]).toMatchObject({ streak_enabled: false })
  })

  it('creates a shared-project source as an infinite zero-baseline stage', async () => {
    const wrapper = mount(StageDialog, {
      props: {
        open: true,
        projectUnit: 'symbols',
        sharedSource: true,
        defaultName: 'Источник 2',
      },
      global: { plugins: [createPinia()], stubs: ionicStubs },
    })

    await wrapper.get('form').trigger('submit')

    expect(wrapper.emitted('submit')?.[0]?.[0]).toMatchObject({
      name: 'Источник 2', infinite: true, total: 0,
    })
    expect(wrapper.text()).toContain('Источники синхронизации')
    expect(wrapper.text()).toContain('Название источника синхронизации:')
  })
})
