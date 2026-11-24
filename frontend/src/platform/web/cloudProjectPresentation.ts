import { PROJECT_STATUSES, UNIT_CODES, type Project, type ProjectStatus } from '@/types/api'
import { todayIsoDate } from '@/utils/projectPlanning'
import type { BrowserProgressView } from './progressView'

type CloudProjectSettings = {
  name: string
  goal?: number | null
  infinite?: boolean
  unit?: string
  status?: string
  deadline?: string | null
  stages_enabled?: boolean
  created_at?: string | null
}

export function cloudProjectStatus(status?: string): ProjectStatus {
  if (status === 'completed') return 'завершен'
  if (status === 'archived') return 'в архиве'
  return PROJECT_STATUSES.find(value => value === status) ?? 'активен'
}

/** The shared cards consume the Desktop view shape. Callers hide progress and
 * stage counts when no authenticated projection is available. */
export function cloudProjectPresentation(id: string, settings: CloudProjectSettings, progress?: BrowserProgressView,
  stages: Project[] = []): Project {
  return {
    id, name: settings.name, goal: settings.goal ?? null, infinite: settings.infinite ?? false,
    status: cloudProjectStatus(settings.status),
    unit: UNIT_CODES.find(unit => unit === settings.unit) ?? 'symbols',
    deadline: settings.deadline ?? null, created_at: settings.created_at ?? null, updated_at: null,
    total: progress?.total ?? 0, progress: progress?.progress ?? 0, remaining: progress?.remaining ?? null,
    planning_date: todayIsoDate(), added_today: 0, personal_goal: 0, today_goal: null, plan_daily_goal: null,
    notes_updated_at: null, mindmap_updated_at: null, completed_at: null, progress_entries: [],
    streak_enabled: false, streak_status: null, streak_length: 0, max_streak: 0, auto_freeze: false,
    project_notes: [], mindmap: null, stages, stages_enabled: settings.stages_enabled ?? stages.length > 0,
    combine_stage_mindmaps: false, parent_project_id: null, cover_image: null, folder_id: null,
    sync_available: false, work_method: 'manual',
  }
}
