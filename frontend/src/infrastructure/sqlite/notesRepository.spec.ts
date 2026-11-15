import { beforeEach, describe, expect, it, vi } from 'vitest'

const invoke = vi.hoisted(() => vi.fn())
vi.mock('@tauri-apps/api/core', () => ({ invoke }))
vi.mock('@tauri-apps/api/event', () => ({ emit: vi.fn().mockResolvedValue(undefined), listen: vi.fn() }))

import { SQLiteNotesRepository } from './notesRepository'

describe('SQLiteNotesRepository', () => {
  const repository = new SQLiteNotesRepository()
  const scope = { projectId: 'project-id', stageId: null }

  beforeEach(() => invoke.mockReset())

  it('uses typed note commands for desktop CRUD and ordering', async () => {
    invoke
      .mockResolvedValueOnce({ notes: [], read_only: false, context: { hasStages: false, stages: [] } })
      .mockResolvedValueOnce({ id: 'note-id' })
      .mockResolvedValueOnce({ id: 'note-id' })
      .mockResolvedValueOnce(undefined)
      .mockResolvedValueOnce({ changed: true, notes: [] })

    await repository.list(scope)
    await repository.create(scope)
    await repository.update(scope, 'note-id', { title: 'Новая' })
    await repository.delete(scope, 'note-id')
    await repository.reorder(scope, ['note-id'])

    expect(invoke).toHaveBeenNthCalledWith(1, 'list_notes', scope)
    expect(invoke).toHaveBeenNthCalledWith(2, 'create_note', scope)
    expect(invoke).toHaveBeenNthCalledWith(3, 'update_note', { ...scope, noteId: 'note-id', patch: { title: 'Новая' } })
    expect(invoke).toHaveBeenNthCalledWith(4, 'delete_note', { ...scope, noteId: 'note-id' })
    expect(invoke).toHaveBeenNthCalledWith(5, 'reorder_notes', { ...scope, noteIds: ['note-id'] })
  })

  it('creates plain notes through the production typed native command', async () => {
    invoke.mockResolvedValue({ id: 'plain' })
    await repository.create(scope, 'plain')
    expect(invoke).toHaveBeenCalledWith('create_note', { ...scope, contentFormat: 'plain' })
  })

  it('passes map conflict heads inside the native command envelope', async () => {
    const map = { nodeData: { id: 'root', topic: 'Root', children: [] } }
    const expectedHeads = { heads: ['head-1'] }
    invoke.mockResolvedValue({ project_id: 'project-id', stage_id: null, name: 'Root', data: map })
    await repository.saveMindMap(scope, map, expectedHeads)
    expect(invoke).toHaveBeenCalledWith('save_map', {
      command: { ...scope, data: map, expectedHeads },
    })
  })

  it('uses typed Tauri commands for maps and XMind without an API fallback', async () => {
    const map = { nodeData: { id: 'root', topic: 'Root', children: [] } }
    const file = new File([new Uint8Array([1, 2, 3])], 'map.xmind')
    invoke
      .mockResolvedValueOnce({ project_id: 'project-id', stage_id: null, name: 'Root', data: map })
      .mockResolvedValueOnce({ project_id: 'project-id', stage_id: null, name: 'Root', data: map })
      .mockResolvedValueOnce({ sheets: [] })

    await repository.mindMap(scope)
    await repository.saveMindMap(scope, map)
    await repository.importXMind(scope, file)

    expect(invoke).toHaveBeenNthCalledWith(1, 'load_map', scope)
    expect(invoke).toHaveBeenNthCalledWith(2, 'save_map', { command: { ...scope, data: map } })
    expect(invoke).toHaveBeenNthCalledWith(3, 'import_xmind', { command: { ...scope, bytes: [1, 2, 3] } })
  })
})
