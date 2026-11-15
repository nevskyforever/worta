import { createPinia } from 'pinia'
import { mount } from '@vue/test-utils'
import { describe, expect, it, vi } from 'vitest'

import { projectFixture } from '@/test/fixtures'

import ProjectCard from './ProjectCard.vue'

describe('ProjectCard', () => {
  it.each([['Роман', 'Этапов'], ['Общий проект', 'Источников']])('names nested items for %s', (name, label) => {
    const wrapper = mount(ProjectCard, {
      props: { project: projectFixture({ name, stages_enabled: true, stages: [projectFixture({ id: 'stage' })] }) },
      global: { plugins: [createPinia()], stubs: { IonIcon: true, RouterLink: { template: '<a><slot /></a>' } } },
    })
    expect(wrapper.text()).toContain(`${label}: 1`)
  })

  it('renders API project progress as an accessible project link', () => {
    const wrapper = mount(ProjectCard, {
      props: { project: projectFixture() },
      global: {
        plugins: [createPinia()],
        stubs: {
          IonIcon: true,
          RouterLink: {
            props: ['to'],
            template: '<a href="/projects/project-id"><slot /></a>',
          },
        },
      },
    })

    expect(wrapper.get('h2').text()).toBe('Дом у моря')
    expect(wrapper.get('[role="img"]').attributes('aria-label')).toContain('Дом у моря')
    expect(wrapper.get('[role="img"]').attributes('aria-label')).toContain('25%')
    expect(wrapper.get('a').attributes('aria-label')).toContain('Открыть проект')
  })

  it('keeps the shared project ring filled as in the legacy desktop UI', () => {
    const wrapper = mount(ProjectCard, {
      props: { project: projectFixture({ name: 'Общий проект', infinite: true, goal: null }) },
      global: {
        plugins: [createPinia()],
        stubs: {
          IonIcon: true,
          RouterLink: { template: '<a><slot /></a>' },
        },
      },
    })

    expect(wrapper.get('[role="img"]').attributes('aria-label')).toContain('100%')
    expect(wrapper.get('.progress-ring').classes()).not.toContain('progress-ring--infinite')
  })

  it('places a saved portrait cover above the project information', () => {
    const cover = 'data:image/jpeg;base64,/9j/2Q=='
    const wrapper = mount(ProjectCard, {
      props: { project: projectFixture({ cover_image: cover }) },
      global: {
        plugins: [createPinia()],
        stubs: { IonIcon: true, RouterLink: { template: '<a><slot /></a>' } },
      },
    })

    expect(wrapper.get('.project-card__cover').attributes('src')).toBe(cover)
    expect(wrapper.get('.project-card__cover').element.nextElementSibling?.className)
      .toContain('project-card__content')
    expect(wrapper.get('.project-card__cover-progress').attributes('aria-label')).toContain('Дом у моря')
    expect(wrapper.find('.progress-ring').exists()).toBe(false)
    expect(wrapper.get('.project-card').classes()).toContain('project-card--with-cover')
  })

  it('shows the project streak only when the global mode and legacy deadline rule allow it', () => {
    const wrapper = mount(ProjectCard, {
      props: {
        project: projectFixture({ streak_length: 6, streak_status: 'Go' }),
        streaksEnabled: true,
      },
      global: {
        plugins: [createPinia()],
        stubs: { IonIcon: true, RouterLink: { template: '<a><slot /></a>' } },
      },
    })

    expect(wrapper.get('.project-card__streak').attributes('aria-label')).toContain('6 дн.')
    expect(wrapper.get('.project-card__streak').attributes('aria-label')).toContain('продлён сегодня')
  })

  it('enables native dragging while the project order is being edited', () => {
    const wrapper = mount(ProjectCard, {
      props: { project: projectFixture(), draggable: true },
      global: {
        plugins: [createPinia()],
        stubs: { IonIcon: true, RouterLink: { template: '<a><slot /></a>' } },
      },
    })

    expect(wrapper.get('.project-card').attributes('draggable')).toBe('true')
  })

  it('uses the server writing day when marking a deadline overdue', () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-08-26T01:30:00'))
    const wrapper = mount(ProjectCard, {
      props: {
        project: projectFixture({ deadline: '2026-08-26', planning_date: '2026-08-25' }),
      },
      global: {
        plugins: [createPinia()],
        stubs: { IonIcon: true, RouterLink: { template: '<a><slot /></a>' } },
      },
    })

    expect(wrapper.find('.project-card__deadline--overdue').exists()).toBe(false)
    vi.useRealTimers()
  })
})
