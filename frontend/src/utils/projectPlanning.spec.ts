import { describe, expect, it, vi } from 'vitest'

import {
  automaticDailyGoal,
  automaticDeadline,
  automaticDeadlineAfterGoalChange,
  automaticEditedDeadline,
  convertProjectUnit,
  writingDayIsoDate,
} from './projectPlanning'

describe('project planning calculations', () => {
  it('derives a daily goal from the deadline and remaining work', () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-08-26T12:00:00'))
    expect(automaticDailyGoal(1_000, 100, '2026-08-28', 'symbols')).toBe(300)
    expect(automaticDailyGoal(1, 0, '2026-08-28', 'author_list')).toBe(0.3)
    vi.useRealTimers()
  })

  it('derives an inclusive deadline from a daily goal', () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-08-26T12:00:00'))
    expect(automaticDeadline(1_000, 100, 300)).toBe('2026-08-28')
    vi.useRealTimers()
  })

  it('does not count progress already added today twice when a deadline changes', () => {
    const today = new Date('2026-08-26T12:00:00')
    expect(automaticDailyGoal(1_000, 400, '2026-08-28', 'symbols', today, 100)).toBe(234)
  })

  it('preserves the legacy current-day plan when an edited daily goal changes', () => {
    expect(automaticEditedDeadline({
      goal: 2_000,
      total: 400,
      dailyGoal: 300,
      todayGoal: 600,
      previousDailyGoal: 200,
      recalculate: false,
      today: new Date('2026-08-26T12:00:00'),
    })).toBe('2026-08-31')
  })

  it('restarts the deadline from current progress when today is clamped to the goal', () => {
    expect(automaticEditedDeadline({
      goal: 16_000,
      total: 14_464,
      dailyGoal: 250,
      todayGoal: 16_000,
      previousDailyGoal: 500,
      recalculate: false,
      today: new Date('2026-09-19T12:00:00'),
    })).toBe('2026-09-25')
  })

  it('uses the current cumulative target when an edited total goal changes', () => {
    expect(automaticDeadlineAfterGoalChange({
      goal: 2_000,
      total: 400,
      dailyGoal: 200,
      todayGoal: 600,
      recalculate: false,
      today: new Date('2026-08-26T12:00:00'),
    })).toBe('2026-09-02')
  })

  it('restarts a plan from the current total only when explicitly requested', () => {
    expect(automaticEditedDeadline({
      goal: 2_000,
      total: 400,
      dailyGoal: 200,
      todayGoal: 600,
      previousDailyGoal: 200,
      recalculate: true,
      today: new Date('2026-08-26T12:00:00'),
    })).toBe('2026-09-02')
  })

  it('converts all supported project units using legacy rounding', () => {
    expect(convertProjectUnit(3_600, 'symbols', 'A4')).toBe(2)
    expect(convertProjectUnit(1, 'author_list', 'symbols')).toBe(40_000)
    expect(convertProjectUnit(20_000, 'symbols', 'author_list')).toBe(0.5)
    expect(convertProjectUnit(4_501, 'symbols', 'ficbook_pages')).toBe(2)
  })

  it('uses the configured writing-day boundary for planning dates', () => {
    expect(writingDayIsoDate('04:00:00', new Date('2026-08-26T02:30:00')))
      .toBe('2026-08-25')
    expect(writingDayIsoDate('04:00:00', new Date('2026-08-26T05:00:00')))
      .toBe('2026-08-26')
  })
})
