import { apiRequest } from './client'
import { currentPlatform } from '@/platform/runtime'
import type {
  ScrivenerItem,
  ProjectSyncs,
  SyncBatchResult,
  SyncConfigure,
  SyncRunResult,
  SyncSummary,
  WordCountResult,
  WordImportResult,
} from '@/types/integrations'

function projectPath(projectId: string): string {
  return `/api/projects/${encodeURIComponent(projectId)}`
}

function projectSyncPath(projectId: string): string {
  return `${projectPath(projectId)}/sync`
}

function stageQuery(stageId?: string | null): string {
  if (!stageId) return ''
  return `?${new URLSearchParams({ stage_id: stageId }).toString()}`
}

async function nativeInvoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import('@tauri-apps/api/core')
  return invoke<T>(command, args)
}

export const integrationsApi = {
  getSync(projectId: string, stageId?: string | null): Promise<SyncSummary> {
    if (currentPlatform() === 'tauri') {
      return nativeInvoke<SyncSummary>('get_document_sync', { command: { projectId, stageId: stageId ?? null } })
    }
    return apiRequest<SyncSummary>(`${projectSyncPath(projectId)}${stageQuery(stageId)}`)
  },

  getProjectSyncs(projectId: string): Promise<ProjectSyncs> {
    if (currentPlatform() === 'tauri') {
      return nativeInvoke<ProjectSyncs>('get_project_document_syncs', { projectId })
    }
    return apiRequest<ProjectSyncs>(`${projectSyncPath(projectId)}/all`)
  },

  configureSync(projectId: string, payload: SyncConfigure): Promise<SyncSummary> {
    if (currentPlatform() === 'tauri') {
      return nativeInvoke<SyncSummary>('configure_document_sync', { command: {
        projectId,
        stageId: payload.stage_id ?? null,
        type: payload.type,
        path: payload.path,
        itemId: payload.item_id ?? null,
      } })
    }
    return apiRequest<SyncSummary>(projectSyncPath(projectId), {
      method: 'PUT',
      body: payload,
    })
  },

  removeSync(projectId: string, stageId?: string | null): Promise<SyncSummary> {
    if (currentPlatform() === 'tauri') {
      return nativeInvoke<SyncSummary>('remove_document_sync', { command: { projectId, stageId: stageId ?? null } })
    }
    return apiRequest<SyncSummary>(`${projectSyncPath(projectId)}${stageQuery(stageId)}`, {
      method: 'DELETE',
    })
  },

  confirmSync(projectId: string, stageId?: string | null): Promise<SyncRunResult> {
    if (currentPlatform() !== 'tauri') return Promise.reject(new Error('Доступно только в desktop-приложении.'))
    return nativeInvoke<SyncRunResult>('confirm_external_progress', { projectId, stageId: stageId ?? null })
  },
  runSync(projectId: string, stageId?: string | null): Promise<SyncRunResult> {
    if (currentPlatform() === 'tauri') {
      return nativeInvoke<SyncRunResult>('refresh_external_progress', { projectId, stageId: stageId ?? null })
    }
    return apiRequest<SyncRunResult>(
      `${projectSyncPath(projectId)}/run${stageQuery(stageId)}`,
      { method: 'POST' },
    )
  },

  runProjectSyncs(projectId: string): Promise<SyncBatchResult> {
    if (currentPlatform() === 'tauri') {
      return nativeInvoke<SyncBatchResult>('run_project_document_syncs', { projectId })
    }
    return apiRequest<SyncBatchResult>(`${projectSyncPath(projectId)}/run-all`, {
      method: 'POST',
    })
  },

  runAllSync(): Promise<SyncBatchResult> {
    if (currentPlatform() === 'tauri') {
      return nativeInvoke<SyncBatchResult>('run_all_document_sync')
    }
    return apiRequest<SyncBatchResult>('/api/integrations/sync/run-all', {
      method: 'POST',
    })
  },

  inspectScrivener(path: string): Promise<ScrivenerItem[]> {
    if (currentPlatform() === 'tauri') {
      return nativeInvoke<ScrivenerItem[]>('inspect_scrivener', { command: { path } })
    }
    const params = new URLSearchParams({ path })
    return apiRequest<ScrivenerItem[]>(`/api/integrations/scrivener/items?${params.toString()}`)
  },

  countWord(file: File): Promise<WordCountResult> {
    if (currentPlatform() === 'tauri') {
      return file.arrayBuffer().then((buffer) => nativeInvoke<number>('count_word_document', {
        bytes: Array.from(new Uint8Array(buffer)),
        filename: file.name,
      }).then((symbols) => ({ symbols })))
    }
    const body = new FormData()
    body.set('file', file, file.name)
    return apiRequest<WordCountResult>('/api/integrations/word/count', {
      method: 'POST',
      rawBody: body,
    })
  },

  importWord(
    projectId: string,
    file: File,
    stageId?: string | null,
  ): Promise<WordImportResult> {
    if (currentPlatform() === 'tauri') {
      return file.arrayBuffer().then((buffer) => nativeInvoke<WordImportResult>('import_word_document', {
        projectId,
        stageId: stageId ?? null,
        bytes: Array.from(new Uint8Array(buffer)),
        filename: file.name,
      }))
    }
    const body = new FormData()
    body.set('file', file, file.name)
    return apiRequest<WordImportResult>(
      `${projectPath(projectId)}/imports/word${stageQuery(stageId)}`,
      { method: 'POST', rawBody: body },
    )
  },
}
