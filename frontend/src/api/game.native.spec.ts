import { beforeEach, describe, expect, it, vi } from 'vitest'

import { invoke } from '@tauri-apps/api/core'

import { gameApi } from './game'

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}))

vi.mock('@/platform/runtime', () => ({
  currentPlatform: () => 'tauri',
}))

describe('native Game repository adapter', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset()
    vi.mocked(invoke).mockResolvedValue({})
  })

  it.each(['sellItem', 'useItem'] as const)('wraps %s inventory arguments', async (method) => {
    await gameApi[method]({ category: 'Предметы', item_id: 'Лотерейный билет', count: 1 })
    expect(invoke).toHaveBeenLastCalledWith(method === 'sellItem' ? 'game_sell_item' : 'game_use_item', {
      payload: { category: 'Предметы', itemId: 'Лотерейный билет', count: 1 },
    })
  })

  it('reads and mutates Game through typed Tauri commands without HTTP', async () => {
    const fetchSpy = vi.spyOn(globalThis, 'fetch')

    await gameApi.state()
    await gameApi.buyItem({ category: 'Предметы', item_id: 'Лотерейный билет', count: 1 })

    expect(fetchSpy).not.toHaveBeenCalled()
    expect(invoke).toHaveBeenNthCalledWith(1, 'game_state', undefined)
    expect(invoke).toHaveBeenNthCalledWith(2, 'game_buy_item', {
      payload: { category: 'Предметы', itemId: 'Лотерейный билет', count: 1 },
    })
    fetchSpy.mockRestore()
  })
})

describe('developer streak native argument regression', () => {
  it.each([
    {type:'global' as const}, {type:'project' as const, project_id:'p'}, {type:'stage' as const, project_id:'p', stage_id:'s'},
  ])('wraps $type target in the required payload argument', async target => {
    const id = '123e4567-e89b-42d3-a456-426614174000'
    await gameApi.developerRestoreStreak(target,id)
    const nativeTarget = {type:target.type,...('project_id' in target ? {projectId:target.project_id}:{}),...('stage_id' in target ? {stageId:target.stage_id}:{})}
    expect(invoke).toHaveBeenLastCalledWith('game_developer_restore_streak',{payload:nativeTarget,correlationId:id})
    await gameApi.developerCreateStreakSeries({...target,length:5})
    expect(invoke).toHaveBeenLastCalledWith('game_developer_create_streak_series',{payload:{...nativeTarget,length:5}})
  })
})
