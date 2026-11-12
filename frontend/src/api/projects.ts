import { requireDesktopRuntime } from '@/platform/runtime'
import { diagnostics } from '@/diagnostics/service'
import { apiRequest } from './client'
import { invoke } from '@tauri-apps/api/core'
import { getProjectReadRepository } from '@/infrastructure/projects/projectReadRepository'
import { TauriProgressRepository } from '@/infrastructure/tauri/progressRepository'
import { settingsApi } from './settings'
import type { ProjectMetadataPatch } from '@/core/repositories/projectMetadata'
import type {
  ProgressCreate,
  ProgressResult,
  Project,
  ProjectCreate,
  ProjectListQuery,
  ProjectFolder,
  ProjectUpdate,
  EntityUpdate,
  GlobalStreakSummary,
  StageCreate,
  Statistics,
  TodaySummary,
} from '@/types/api'

function projectPath(projectId: string): string {
  return `/api/projects/${encodeURIComponent(projectId)}`
}

function stagePath(projectId: string, stageId: string): string {
  return `${projectPath(projectId)}/stages/${encodeURIComponent(stageId)}`
}

function queryString(query: ProjectListQuery): string {
  const params = new URLSearchParams()
  if (query.status) params.set('status', query.status)
  if (query.search?.trim()) params.set('search', query.search.trim())
  if (query.sort) params.set('sort', query.sort)
  const encoded = params.toString()
  return encoded ? `?${encoded}` : ''
}

function desktopRuntime(): boolean {
  return typeof window !== 'undefined' && window.__TAURI_INTERNALS__ !== undefined
}

function desktopProjectCreate(payload: ProjectCreate): Record<string, unknown> {
  return {
    name: payload.name, goal: payload.goal, infinite: payload.infinite ?? false,
    total: payload.total ?? 0, deadline: payload.deadline ?? null,
    personalGoal: payload.personal_goal ?? 0, streakEnabled: payload.streak_enabled ?? true,
    autoFreeze: payload.auto_freeze ?? true, workMethod: payload.work_method ?? 'manual',
    unit: payload.unit ?? 'symbols', stagesEnabled: payload.stages_enabled ?? false,
    combineStageMindmaps: payload.combine_stage_mindmaps ?? false,
    coverImage: payload.cover_image ?? null, folderId: payload.folder_id ?? null,
    stages: (payload.stages ?? []).map((stage) => ({
      name: stage.name, goal: stage.goal, infinite: stage.infinite ?? false,
      total: stage.total ?? 0, deadline: stage.deadline ?? null,
      personalGoal: stage.personal_goal ?? 0, streakEnabled: stage.streak_enabled ?? true,
      autoFreeze: stage.auto_freeze ?? true, workMethod: stage.work_method ?? 'manual',
    })),
  }
}

function desktopStageCreate(payload: StageCreate): Record<string, unknown> {
  return {
    name: payload.name, goal: payload.goal, infinite: payload.infinite ?? false,
    total: payload.total ?? 0, deadline: payload.deadline ?? null,
    personalGoal: payload.personal_goal ?? 0, streakEnabled: payload.streak_enabled ?? true,
    autoFreeze: payload.auto_freeze ?? true, workMethod: payload.work_method ?? 'manual',
  }
}

function desktopEntityPatch(payload: EntityUpdate): Record<string, unknown> {
  return {
    ...((payload as ProjectUpdate).folder_id !== undefined ? { folderId: (payload as ProjectUpdate).folder_id } : {}),
    ...(payload.name !== undefined ? { name: payload.name } : {}),
    ...(payload.goal !== undefined ? { goal: payload.goal } : {}),
    ...(payload.infinite !== undefined ? { infinite: payload.infinite } : {}),
    ...(payload.total !== undefined ? { total: payload.total } : {}),
    ...(payload.deadline !== undefined ? { deadline: payload.deadline } : {}),
    ...(payload.personal_goal !== undefined ? { personalGoal: payload.personal_goal } : {}),
    ...(payload.streak_enabled !== undefined ? { streakEnabled: payload.streak_enabled } : {}),
    ...(payload.auto_freeze !== undefined ? { autoFreeze: payload.auto_freeze } : {}),
    ...(payload.work_method !== undefined ? { workMethod: payload.work_method } : {}),
    ...(payload.recalculate_plan !== undefined ? { recalculatePlan: payload.recalculate_plan } : {}),
    ...(payload.confirm_daily_goal_increase !== undefined ? { confirmDailyGoalIncrease: payload.confirm_daily_goal_increase } : {}),
  }
}

function symbolsToUnit(symbols: number, unit: Project['unit']): number {
  const factors: Record<Project['unit'], number> = {
    symbols: 1, A4: 1800, author_list: 40000, ficbook_pages: 4500,
  }
  const value = symbols / factors[unit]
  return unit === 'symbols' ? value : unit === 'author_list' ? Math.round(value * 10) / 10 : Math.ceil(value)
}

async function nativeTodaySummary(projects: Project[]): Promise<TodaySummary> {
  const date = await invoke<string>('get_current_writing_day')
  const summaries = projects.flatMap((project) => {
    const entries = project.stages.length ? project.stages.flatMap(stage=>stage.progress_entries) : project.progress_entries
    const symbols = entries
      .filter((entry) => (entry.writing_day ?? entry.created_at.slice(0,10)) === date)
      .reduce((total, entry) => total + entry.added_symbols, 0)
    return symbols > 0 ? [{
      id: project.id, name: project.name, symbols,
      unit: project.unit, value: symbolsToUnit(symbols, project.unit),
    }] : []
  })
  return {
    date,
    symbols: summaries.reduce((total, project) => total + project.symbols, 0),
    projects: summaries,
  }
}

export const projectsApi = {
  list(query: ProjectListQuery = {}, signal?: AbortSignal): Promise<Project[]> {
    if (desktopRuntime()) return getProjectReadRepository().listProjects(query, signal)
    return apiRequest<Project[]>(`/api/projects${queryString(query)}`, { signal })
  },

  folders(signal?: AbortSignal): Promise<ProjectFolder[]> {
    if (desktopRuntime()) return invoke<ProjectFolder[]>('list_project_folders')
    return apiRequest<ProjectFolder[]>('/api/projects/folders', { signal })
  },

  createFolder(name: string): Promise<ProjectFolder> {
    if (desktopRuntime()) return invoke<ProjectFolder>('create_project_folder', { name })
    return apiRequest<ProjectFolder>('/api/projects/folders', { method: 'POST', body: { name } })
  },

  updateFolder(folderId: string, name: string): Promise<ProjectFolder> {
    if (desktopRuntime()) return invoke<ProjectFolder>('update_project_folder', { folderId, name })
    return apiRequest<ProjectFolder>(`/api/projects/folders/${encodeURIComponent(folderId)}`, {
      method: 'PATCH', body: { name },
    })
  },

  removeFolder(folderId: string): Promise<void> {
    if (desktopRuntime()) return invoke<void>('delete_project_folder', { folderId })
    return apiRequest<void>(`/api/projects/folders/${encodeURIComponent(folderId)}`, { method: 'DELETE' })
  },

  async reorderFolders(folderIds: string[]): Promise<void> {
    requireDesktopRuntime()
    return invoke('reorder_project_folders', { folderIds })
  },

  reorder(projectIds: string[]): Promise<Project[]> {
    if (desktopRuntime()) return invoke<Project[]>('reorder_projects', { projectIds })
    return apiRequest<Project[]>('/api/projects/order', {
      method: 'PUT', body: { project_ids: projectIds },
    })
  },

  get(projectId: string, signal?: AbortSignal): Promise<Project> {
    if (desktopRuntime()) {
      return getProjectReadRepository().getProject(projectId, signal).then((project) => {
        if (!project) throw new Error('Проект не найден.')
        return project
      })
    }
    return apiRequest<Project>(projectPath(projectId), { signal })
  },

  today(signal?: AbortSignal): Promise<TodaySummary> {
    if (desktopRuntime()) {
      return getProjectReadRepository().listProjects({}, signal).then(nativeTodaySummary)
    }
    return apiRequest<TodaySummary>('/api/projects/today', { signal })
  },

  async globalStreak(signal?: AbortSignal): Promise<GlobalStreakSummary> {
    if (desktopRuntime()) {
      if (signal?.aborted) throw new DOMException('Aborted', 'AbortError')
      const settings = await settingsApi.get(signal)
      if (settings.values.global_streak !== true) {
        return { enabled: false, status: 'Off', length: 0, max_length: 0 }
      }
      const state = await invoke<Record<string, unknown>>('game_state')
      const streak = state.global_streak as Record<string, unknown> | undefined
      return {
        enabled: true,
        status: typeof streak?.status === 'string' ? streak.status : 'No',
        length: typeof streak?.length === 'number' ? streak.length : 0,
        max_length: typeof streak?.max_length === 'number' ? streak.max_length : 0,
      }
    }
    return apiRequest<GlobalStreakSummary>('/api/projects/streaks/global', { signal })
  },

  create(payload: ProjectCreate): Promise<Project> {
    if (desktopRuntime()) return diagnostics.run('projects', 'create', () => invoke<Project>('create_project', { command: desktopProjectCreate(payload) }))
    return apiRequest<Project>('/api/projects', { method: 'POST', body: payload })
  },

  update(projectId: string, payload: ProjectUpdate): Promise<Project> {
    if (desktopRuntime()) return diagnostics.run('projects', 'update', () => invoke<Project>('update_project', { projectId, patch: desktopEntityPatch(payload) }))
    return apiRequest<Project>(projectPath(projectId), { method: 'PATCH', body: payload })
  },

  updateMetadata(projectId: string, patch: ProjectMetadataPatch): Promise<Project> {
    return apiRequest<Project>(`${projectPath(projectId)}/metadata`, { method: 'PATCH', body: patch })
  },

  remove(projectId: string): Promise<void> {
    if (desktopRuntime()) return invoke<void>('delete_project', { command: { projectId } })
    return apiRequest<void>(projectPath(projectId), { method: 'DELETE' })
  },

  setArchived(projectId: string, archived: boolean): Promise<Project> {
    if (desktopRuntime()) return invoke<Project>('set_project_archived', { command: { projectId, archived } })
    return apiRequest<Project>(`${projectPath(projectId)}/archive`, {
      method: 'POST',
      body: { archived },
    })
  },

  complete(projectId: string): Promise<Project> {
    if (desktopRuntime()) return invoke<Project>('complete_project', { command: { projectId } })
    return apiRequest<Project>(`${projectPath(projectId)}/complete`, { method: 'POST' })
  },

  createStage(projectId: string, payload: StageCreate): Promise<Project> {
    if (desktopRuntime()) return diagnostics.run('stages', 'create', () => invoke<Project>('create_stage', { projectId, command: desktopStageCreate(payload) }))
    return apiRequest<Project>(`${projectPath(projectId)}/stages`, {
      method: 'POST',
      body: payload,
    })
  },

  updateStage(projectId: string, stageId: string, payload: EntityUpdate): Promise<Project> {
    if (desktopRuntime()) return diagnostics.run('stages', 'update', () => invoke<Project>('update_stage', { projectId, stageId, patch: desktopEntityPatch(payload) }))
    return apiRequest<Project>(stagePath(projectId, stageId), {
      method: 'PATCH',
      body: payload,
    })
  },

  removeStage(projectId: string, stageId: string): Promise<void> {
    if (desktopRuntime()) return invoke<void>('delete_stage', { command: { projectId, stageId } })
    return apiRequest<void>(stagePath(projectId, stageId), { method: 'DELETE' })
  },

  reorderStages(projectId: string, stageIds: string[]): Promise<Project> {
    if (desktopRuntime()) return diagnostics.run('stages', 'reorder', () => invoke<Project>('reorder_stages', { command: { projectId, stageIds } }))
    return apiRequest<Project>(`${projectPath(projectId)}/stages/order`, {
      method: 'PUT',
      body: { stage_ids: stageIds },
    })
  },

  completeStage(projectId: string, stageId: string): Promise<Project> {
    if (desktopRuntime()) return invoke<Project>('complete_stage', { command: { projectId, stageId } })
    return apiRequest<Project>(`${stagePath(projectId, stageId)}/complete`, { method: 'POST' })
  },

  recordProgress(projectId: string, payload: ProgressCreate): Promise<ProgressResult> {
    if (desktopRuntime()) {
      return invoke<ProgressResult>(payload.stage_id ? 'add_stage_progress' : 'add_project_progress', { command: {
        projectId, ...(payload.stage_id ? { stageId: payload.stage_id } : {}), newTotal: payload.new_total, ...(payload.expected_heads===undefined?{}:{expectedHeads:payload.expected_heads}),
      } })
    }
    return apiRequest<ProgressResult>(`${projectPath(projectId)}/progress`, {
      method: 'POST',
      body: payload,
    })
  },

  deleteProgress(projectId: string, entryId: string, stageId?: string): Promise<Project> {
    if (desktopRuntime()) {
      return new TauriProgressRepository().remove({ projectId, entryId, stageId })
    }
    const params = new URLSearchParams()
    if (stageId) params.set('stage_id', stageId)
    const encoded = params.toString()
    const suffix = encoded ? `?${encoded}` : ''
    return apiRequest<Project>(
      `${projectPath(projectId)}/progress/${encodeURIComponent(entryId)}${suffix}`,
      { method: 'DELETE' },
    )
  },

  statistics(projectId: string, stageId?: string): Promise<Statistics> {
    const params = new URLSearchParams()
    if (stageId) params.set('stage_id', stageId)
    const encoded = params.toString()
    const suffix = encoded ? `?${encoded}` : ''
    return apiRequest<Statistics>(`${projectPath(projectId)}/statistics${suffix}`)
  },
}
