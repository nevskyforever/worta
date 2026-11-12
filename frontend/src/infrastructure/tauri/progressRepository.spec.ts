import { beforeEach, describe, expect, it, vi } from 'vitest'

import { invoke } from '@tauri-apps/api/core'
import { TauriProgressRepository } from './progressRepository'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))

describe('TauriProgressRepository', () => {
  beforeEach(() => vi.mocked(invoke).mockReset())

  it('uses separate narrow commands for project and stage progress', async () => {
    vi.mocked(invoke).mockResolvedValue({ project: {}, entry: {}, added_symbols: 1 })
    const repository = new TauriProgressRepository()

    await repository.add({ kind: 'manual', projectId: 'p/1', newTotal: 10 })
    expect(invoke).toHaveBeenLastCalledWith('add_project_progress', { command: {
      projectId: 'p/1',
      newTotal: 10,
    } })

    await repository.add({ kind: 'manual', projectId: 'p/1', stageId: 's/1', newTotal: 12 })
    expect(invoke).toHaveBeenLastCalledWith('add_stage_progress', { command: {
      projectId: 'p/1',
      stageId: 's/1',
      newTotal: 12,
    } })
  })

  it('wraps the project and stage deletion arguments in the native command', async () => {
    vi.mocked(invoke).mockResolvedValue({})
    const repository = new TauriProgressRepository()
    await repository.remove({ projectId: 'p', entryId: 'entry/1' })
    expect(invoke).toHaveBeenLastCalledWith('delete_progress', {
      command: { projectId: 'p', entryId: 'entry/1', stageId: null },
    })

    await repository.remove({ projectId: 'p', entryId: 'entry/2', stageId: 'stage' })
    expect(invoke).toHaveBeenLastCalledWith('delete_progress', {
      command: { projectId: 'p', entryId: 'entry/2', stageId: 'stage' },
    })
  })
})
