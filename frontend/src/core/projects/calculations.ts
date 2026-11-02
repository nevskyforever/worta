import type { FiniteGoal, ProgressUnit } from './types'
import { convertUnits, roundHalfEven } from './units'

function localDate(value: string): Date | null {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(value)) return null
  const [year, month, day] = value.split('-').map(Number)
  const result = new Date(year!, month! - 1, day!)
  return result.getFullYear() === year && result.getMonth() === month! - 1
    && result.getDate() === day ? result : null
}

export function planningDate(value: string | undefined, fallback: Date): Date {
  return value ? localDate(value) ?? fallback : fallback
}

function startOfDay(value: Date): Date {
  return new Date(value.getFullYear(), value.getMonth(), value.getDate())
}

function toIsoDate(value: Date): string {
  return `${value.getFullYear()}-${String(value.getMonth() + 1).padStart(2, '0')}-${String(value.getDate()).padStart(2, '0')}`
}

function roundedPlanValue(value: number, unit: ProgressUnit): number {
  return unit === 'author_list' ? Math.round(value * 10) / 10 : Math.ceil(value)
}

export { convertUnits }
export { roundHalfEven }

export function progressPercentage({ goal, total, infinite }: FiniteGoal): number {
  if (infinite || goal === null || goal <= 0) return 0
  return total / goal * 100
}

export function remainingToGoal({ goal, total, infinite }: FiniteGoal): number | null {
  if (infinite || goal === null) return null
  return Math.max(0, goal - total)
}

export function automaticDailyGoal(
  goal: number, total: number, deadline: string, unit: ProgressUnit,
  today: Date, addedToday = 0,
): number | null {
  const due = localDate(deadline)
  if (!due || !Number.isFinite(goal) || !Number.isFinite(total)) return null
  const baseTotal = Math.max(0, total - Math.max(0, addedToday))
  const remaining = Math.max(0, goal - baseTotal)
  if (remaining === 0) return 0
  const days = Math.floor((due.getTime() - startOfDay(today).getTime()) / 86_400_000) + 1
  return roundedPlanValue(remaining / Math.max(1, days), unit)
}

export function automaticDeadline(goal: number, total: number, dailyGoal: number, today: Date): string | null {
  if (!Number.isFinite(goal) || !Number.isFinite(total) || !Number.isFinite(dailyGoal)
    || dailyGoal <= 0 || goal <= total) return null
  const due = startOfDay(today)
  due.setDate(due.getDate() + Math.ceil((goal - total) / dailyGoal) - 1)
  return toIsoDate(due)
}

export interface EditedDeadlineOptions {
  goal: number
  total: number
  dailyGoal: number
  todayGoal: number | null
  previousDailyGoal: number | null
  recalculate: boolean
  today: Date
}

export function automaticEditedDeadline(options: EditedDeadlineOptions): string | null {
  if (options.recalculate || options.todayGoal === null || options.previousDailyGoal === null
    || options.todayGoal >= options.goal) {
    return automaticDeadline(options.goal, options.total, options.dailyGoal, options.today)
  }
  if (options.dailyGoal <= 0 || !Number.isFinite(options.goal)) return null
  const newTodayGoal = options.todayGoal - options.previousDailyGoal + options.dailyGoal
  const daysAfterToday = Math.ceil(Math.max(0, options.goal - newTodayGoal) / options.dailyGoal)
  const due = startOfDay(options.today)
  due.setDate(due.getDate() + daysAfterToday)
  return toIsoDate(due)
}

export function automaticDeadlineAfterGoalChange(options: Omit<EditedDeadlineOptions, 'previousDailyGoal'>): string | null {
  if (options.recalculate || options.todayGoal === null || options.todayGoal >= options.goal) {
    return automaticDeadline(options.goal, options.total, options.dailyGoal, options.today)
  }
  if (options.dailyGoal <= 0 || !Number.isFinite(options.goal)) return null
  const due = startOfDay(options.today)
  due.setDate(due.getDate() + Math.ceil(Math.max(0, options.goal - options.todayGoal) / options.dailyGoal))
  return toIsoDate(due)
}

export function todayIsoDate(today: Date): string { return toIsoDate(startOfDay(today)) }

export function writingDayIsoDate(startTime: unknown, now: Date): string {
  const match = typeof startTime === 'string' ? /^(\d{2}):(\d{2})/.exec(startTime) : null
  const hour = match ? Number(match[1]) : 0
  const minute = match ? Number(match[2]) : 0
  const writingDay = startOfDay(now)
  if (Number.isInteger(hour) && Number.isInteger(minute) && hour >= 0 && hour <= 23
    && minute >= 0 && minute <= 59 && now.getHours() * 60 + now.getMinutes() < hour * 60 + minute) {
    writingDay.setDate(writingDay.getDate() - 1)
  }
  return toIsoDate(writingDay)
}
