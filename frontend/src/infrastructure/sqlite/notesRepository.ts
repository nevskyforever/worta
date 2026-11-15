import { invoke } from '@tauri-apps/api/core'

import type { NotesRepository } from '@/core/repositories/notes'
import type {
  JsonObject,
  MindMapResponse,
  NoteOrderResponse,
  NotesResponse,
  NotesScope,
  ProjectNote,
  ProjectNotePatch,
  XMindImportResponse,
} from '@/types/notes'
import { normalizeMindMapData } from '@/core/mindmap/normalization'
import { announceLocalNoteWrite } from '@/services/localNoteWrites'

function nativeScope(scope: NotesScope): { projectId: string; stageId: string | null } {
  return { projectId: scope.projectId, stageId: scope.stageId ?? null }
}

export class SQLiteNotesRepository implements NotesRepository {
  async list(scope: NotesScope): Promise<NotesResponse> {
    return invoke<NotesResponse>('list_notes', nativeScope(scope))
  }

  async get(scope: NotesScope, noteId: string): Promise<ProjectNote> {
    const note = await invoke<ProjectNote | null>('get_note', { ...nativeScope(scope), noteId })
    if (!note) throw new Error('Заметка не найдена.')
    return note
  }

  async create(scope: NotesScope,contentFormat?:'html'|'plain'): Promise<ProjectNote> {
    const note = await invoke<ProjectNote>('create_note', {...nativeScope(scope),...(contentFormat?{contentFormat}:{})})
    announceLocalNoteWrite(scope.projectId)
    return note
  }

  async update(scope: NotesScope, noteId: string, patch: ProjectNotePatch): Promise<ProjectNote> {
    const note = await invoke<ProjectNote>('update_note', { ...nativeScope(scope), noteId, patch })
    announceLocalNoteWrite(scope.projectId)
    return note
  }

  async delete(scope: NotesScope, noteId: string): Promise<void> {
    await invoke('delete_note', { ...nativeScope(scope), noteId })
    announceLocalNoteWrite(scope.projectId)
  }

  async reorder(scope: NotesScope, noteIds: string[]): Promise<NoteOrderResponse> {
    const result = await invoke<NoteOrderResponse>('reorder_notes', { ...nativeScope(scope), noteIds })
    announceLocalNoteWrite(scope.projectId)
    return result
  }

  mindMap(scope: NotesScope): Promise<MindMapResponse> {
    return invoke<MindMapResponse>('load_map', nativeScope(scope))
  }

  async saveMindMap(scope: NotesScope, data: JsonObject, expectedHeads?: JsonObject): Promise<MindMapResponse> {
    const normalized = normalizeMindMapData(data)
    if (!normalized) throw new Error('Редактор вернул повреждённые данные карты.')
    return invoke<MindMapResponse>('save_map', {
      command: {
        ...nativeScope(scope),
        data: normalized,
        ...(expectedHeads ? { expectedHeads } : {}),
      },
    })
  }

  async importXMind(scope: NotesScope, file: File): Promise<XMindImportResponse> {
    const bytes = Array.from(new Uint8Array(await file.arrayBuffer()))
    return invoke<XMindImportResponse>('import_xmind', {
      command: { ...nativeScope(scope), bytes },
    })
  }
}
