import { invoke } from '@tauri-apps/api/core'

import type {
  ManualProgressInput,
  ProgressRepository,
  RemoveProgressInput,
} from '@/core/repositories/progress'
import type { ProgressResult, Project } from '@/types/api'

export class TauriProgressRepository implements ProgressRepository {
  add(input: ManualProgressInput): Promise<ProgressResult> {
    const command = input.stageId ? 'add_stage_progress' : 'add_project_progress'
    return invoke<ProgressResult>(command, { command: {
      projectId: input.projectId,
      ...(input.stageId ? { stageId: input.stageId } : {}),
      newTotal: input.newTotal,
      ...(input.expectedHeads===undefined?{}:{expectedHeads:input.expectedHeads}),
    } })
  }

  remove(input: RemoveProgressInput): Promise<Project> {
    return invoke<Project>('delete_progress', {
      command: {
        projectId: input.projectId,
        entryId: input.entryId,
        stageId: input.stageId ?? null,
      },
    })
  }
}
