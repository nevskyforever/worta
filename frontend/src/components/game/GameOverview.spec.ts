import { createPinia } from 'pinia'
import { nextTick } from 'vue'
import { mount } from '@vue/test-utils'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { useLocaleStore } from '@/stores/locale'
import { gameStateFixture } from '@/test/gameFixtures'
import type { GameBuff, GameBuffs } from '@/types/game'

import GameOverview from './GameOverview.vue'

function buff(overrides: Partial<GameBuff> = {}): GameBuff {
  return {
    name: 'Эффект',
    description: 'Описание эффекта',
    type: 'positive',
    target: 'exp',
    value: 1,
    stacks: 1,
    duration_minutes: null,
    started_at: null,
    expires_at: null,
    remaining_seconds: null,
    source: null,
    stackable: false,
    ...overrides,
  }
}

function mountOverview(buffs: GameBuffs) {
  const pinia = createPinia()
  useLocaleStore(pinia).language = 'ru'
  const state = gameStateFixture()
  return mount(GameOverview, {
    props: {
      profile: state.profile,
      bank: state.bank,
      buffs,
      streakFreezes: state.streak_freezes,
      busy: false,
    },
    global: {
      plugins: [pinia],
      stubs: { AnimatedNumber: true },
    },
  })
}

afterEach(() => {
  vi.useRealTimers()
})

describe('GameOverview effects', () => {
  it('does not render a timer for a permanent specialization buff', () => {
    const wrapper = mountOverview({
      server_time: '2026-09-06T15:00:00Z',
      positive: [buff({
        name: '⭐️ Квестовая специализация: опыт',
        description: 'Постоянный бонус к коэффициенту опыта за писательские и учебные квесты. +0.02 к параметру за завершение квеста.',
        value: 0.465,
        started_at: '2026-07-14T23:47:49.846175',
        remaining_seconds: 0,
        source: 'Квест',
        stackable: true,
      })],
      negative: [],
    })

    expect(wrapper.text()).toContain('⭐️ Квестовая специализация: опыт')
    expect(wrapper.find('.buff-timer').exists()).toBe(false)
    wrapper.unmount()
  })

  it('renders the legacy consumable description and a total-hours countdown', async () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-09-06T15:00:00Z'))
    const wrapper = mountOverview({
      server_time: '2026-09-06T15:00:00Z',
      positive: [buff({
        name: '🧪⚡️ Супер бустер опыта',
        description: 'Применено зелье познания',
        value: 10,
        duration_minutes: 60,
        started_at: '2026-09-04T04:06:31.149420Z',
        expires_at: '2026-09-11T01:08:03Z',
        remaining_seconds: 382083,
      })],
      negative: [],
    })

    expect(wrapper.text()).toContain('Применено зелье познания')
    expect(wrapper.get('.buff-timer').text()).toBe('106:08:03')

    vi.advanceTimersByTime(1_000)
    await nextTick()
    expect(wrapper.get('.buff-timer').text()).toBe('106:08:02')
    wrapper.unmount()
  })

  it('removes expired effects and retains permanent effects while the overview is open', async () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-09-06T15:00:00Z'))
    const timing = { expires_at: '2026-09-06T19:00:01', remaining_seconds: 1 }
    const wrapper = mountOverview({
      server_time: '2026-09-06T15:00:00Z',
      positive: [buff({ name: 'Постоянный эффект' }), buff({ ...timing, name: 'Временное усиление' })],
      negative: [buff({ ...timing, name: 'Временное ограничение', type: 'negative' })],
    })
    expect(wrapper.findAll('.buff-timer').map((timer) => timer.text())).toEqual(['00:01', '00:01'])
    vi.advanceTimersByTime(1_000)
    await nextTick()

    expect(wrapper.text()).toContain('Постоянный эффект')
    expect(wrapper.text()).not.toContain('Временное усиление')
    expect(wrapper.text()).not.toContain('Временное ограничение')
    expect(wrapper.emitted('effectsExpired')).toHaveLength(1)
    wrapper.unmount()
  })

  it('keeps every active modifier in the overview summary', () => {
    const wrapper = mountOverview({
      server_time: '2026-09-06T15:00:00Z',
      positive: [
        buff({ name: 'Опыт', target: 'exp', value: 14.515 }),
        buff({ name: 'Здоровье', target: 'health_recovery', value: 1.3 }),
        buff({ name: 'Монеты', target: 'coins', value: 2.5 }),
      ],
      negative: [],
    })

    expect(wrapper.findAll('.buff-summary-card')).toHaveLength(3)
    expect(wrapper.text()).toContain('+14,515')
    expect(wrapper.text()).toContain('+1,3')
    expect(wrapper.text()).toContain('+2,5')
    wrapper.unmount()
  })
})
