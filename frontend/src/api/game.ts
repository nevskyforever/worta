import { apiRequest } from './client'
import { invoke } from '@tauri-apps/api/core'
import { currentPlatform } from '@/platform/runtime'
import type {
  BankProductRequest,
  GameCommandResponse,
  DeveloperModeState,
  DeveloperProfileUpdate,
  DeveloperStreakRequest,
  DeveloperStreakState,
  GameNotifications,
  GameState,
  InventoryCommand,
  ProfileTransferDirection,
  ProfileTransferRequestResult,
  ProfileTransferResult,
  ShopCatalog,
  WritingSessionStart,
} from '@/types/game'

const GAME_PATH = '/api/game'

const nativeGame = <T>(commandName: string, payload?: Record<string, unknown>): Promise<T> =>
  invoke<T>(commandName, payload === undefined ? undefined : payload)

function isDesktopGame(): boolean {
  return currentPlatform() === 'tauri'
}

function command(path: string, body?: unknown, method = 'POST'): Promise<GameCommandResponse> {
  return apiRequest<GameCommandResponse>(`${GAME_PATH}${path}`, { method, body })
}

function awardPath(awardId: string): string {
  return `/custom-awards/${encodeURIComponent(awardId)}`
}

export const gameApi = {
  state(signal?: AbortSignal): Promise<GameState> {
    if (isDesktopGame()) return nativeGame<GameState>('game_state')
    return apiRequest<GameState>(`${GAME_PATH}/state`, { signal })
  },

  notifications(signal?: AbortSignal): Promise<GameNotifications> {
    if (isDesktopGame()) return nativeGame<GameNotifications>('game_notifications')
    return apiRequest<GameNotifications>(`${GAME_PATH}/notifications`, { signal })
  },

  markNotificationRead(notificationId: string): Promise<GameNotifications> {
    if (isDesktopGame()) return nativeGame<GameNotifications>('mark_game_notification_read', { notificationId })
    return apiRequest<GameNotifications>(
      `${GAME_PATH}/notifications/${encodeURIComponent(notificationId)}/read`,
      { method: 'POST' },
    )
  },

  markAllNotificationsRead(): Promise<GameNotifications> {
    if (isDesktopGame()) return nativeGame<GameNotifications>('mark_all_game_notifications_read')
    return apiRequest<GameNotifications>(`${GAME_PATH}/notifications/read-all`, {
      method: 'POST',
    })
  },

  catalog(signal?: AbortSignal): Promise<ShopCatalog> {
    if (isDesktopGame()) return nativeGame<ShopCatalog>('game_catalog')
    return apiRequest<ShopCatalog>(`${GAME_PATH}/catalog`, { signal })
  },

  developerState(): Promise<DeveloperModeState> {
    if (isDesktopGame()) return nativeGame<DeveloperModeState>('game_developer_state')
    return apiRequest<DeveloperModeState>(`${GAME_PATH}/developer`)
  },

  updateDeveloperProfile(payload: DeveloperProfileUpdate): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeCommand('game_update_developer_profile', {
      level: payload.level, health: payload.health, coins: payload.coins, exp: payload.exp,
      testDateEnabled: payload.test_date_enabled, testDatetime: payload.test_datetime,
    })
    return command('/developer/profile', payload, 'PUT')
  },

  grantDeveloperInventoryItem(
    category: string,
    itemId: string,
    count: number,
  ): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeCommand('game_grant_developer_inventory_item', {
      category, itemId, count,
    })
    return command('/developer/inventory', {
      category,
      item_id: itemId,
      count,
    })
  },

  developerStreakState(): Promise<DeveloperStreakState> {
    if (isDesktopGame()) return nativeGame<DeveloperStreakState>('game_developer_streak_state')
    return apiRequest<DeveloperStreakState>(`${GAME_PATH}/developer/streaks`)
  },

  developerRestoreStreak(payload: DeveloperStreakRequest, correlationId?: string): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeCommand('game_developer_restore_streak', { payload: nativeDeveloperStreakPayload(payload), ...(correlationId ? { correlationId } : {}) })
    return command('/developer/streaks/restore', payload)
  },

  developerCreateStreakSeries(payload: DeveloperStreakRequest): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeCommand('game_developer_create_streak_series', { payload: nativeDeveloperStreakPayload(payload) })
    return command('/developer/streaks/series', payload)
  },

  requestProfileTransfer(direction: ProfileTransferDirection): Promise<ProfileTransferRequestResult> {
    if (!isDesktopGame()) {
      return Promise.reject(new Error('Управление persistent test_data доступно только в Tauri.'))
    }
    return nativeGame<ProfileTransferRequestResult>('request_profile_transfer', { direction })
  },

  takeProfileTransferResult(): Promise<ProfileTransferResult | null> {
    if (!isDesktopGame()) return Promise.resolve(null)
    return nativeGame<ProfileTransferResult | null>('take_profile_transfer_result')
  },

  startWritingSession(payload: WritingSessionStart): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeCommand('game_start_writing_session', {
      durationMinutes: payload.duration_minutes,
      targetSymbols: payload.target_symbols,
      intention: payload.intention,
      mode: payload.mode,
    })
    return command('/writing-sessions/start', payload)
  },

  finishWritingSession(): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeCommand('game_finish_writing_session')
    return command('/writing-sessions/finish')
  },

  cancelWritingSession(): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeCommand('game_cancel_writing_session')
    return command('/writing-sessions/cancel')
  },

  applyStreakFreeze(
    target: 'global' | 'project',
    projectId?: string,
  ): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeCommand('game_apply_streak_freeze', { target, projectId })
    return command('/streak-freezes/apply', {
      target,
      ...(projectId ? { project_id: projectId } : {}),
    })
  },

  selectDailyChallenge(optionId: string): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeCommand('game_select_daily_challenge', { optionId })
    return command('/daily-challenge/select', { option_id: optionId })
  },

  startWeeklyChallenge(challengeId: string): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeCommand('game_start_weekly_challenge', { challengeId })
    return command('/weekly-challenge/start', { challenge_id: challengeId })
  },

  activateInspirationAbility(abilityId: string): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeCommand('game_activate_inspiration_ability', { abilityId })
    return command(`/inspiration-abilities/${encodeURIComponent(abilityId)}/activate`)
  },

  resolveCreativeEvent(choice: 'safe' | 'risk'): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeCommand('game_resolve_creative_event', { choice })
    return command('/creative-events/resolve', { choice })
  },

  selectSpecialization(specializationId: string): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeCommand('game_select_specialization', { specializationId })
    return command('/specialization/select', { specialization_id: specializationId })
  },

  activateSpecializationAbility(): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeCommand('game_activate_specialization_ability')
    return command('/specialization/ability/activate')
  },

  increaseSkill(skillId: string, points = 1): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeCommand('game_increase_skill', { skillId, points })
    return command(`/skills/${encodeURIComponent(skillId)}/increase`, { points })
  },

  startQuest(questId: string): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeCommand('game_start_quest', { questId })
    return command(`/quests/${encodeURIComponent(questId)}/start`)
  },

  abandonQuest(questId: string): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeCommand('game_abandon_quest', { questId })
    return command(`/quests/${encodeURIComponent(questId)}/abandon`)
  },

  buyItem(payload: InventoryCommand): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeCommand('game_buy_item', { payload: nativeInventoryPayload(payload) })
    return command('/inventory/buy', payload)
  },

  sellItem(payload: InventoryCommand): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeCommand('game_sell_item', { payload: nativeInventoryPayload(payload) })
    return command('/inventory/sell', payload)
  },

  useItem(payload: InventoryCommand): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeCommand('game_use_item', { payload: nativeInventoryPayload(payload) })
    return command('/inventory/use', payload)
  },

  createCustomAward(
    name: string, price: number, applyInflation: boolean,
  ): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeCommand('game_create_custom_award', { name, price, applyInflation })
    return command('/custom-awards', { name, price, apply_inflation: applyInflation })
  },

  updateCustomAward(
    awardId: string,
    payload: { name?: string; price?: number; apply_inflation?: boolean },
  ): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeCommand('game_update_custom_award', {
      awardId,
      ...(payload.name !== undefined ? { name: payload.name } : {}),
      ...(payload.price !== undefined ? { price: payload.price } : {}),
      ...(payload.apply_inflation !== undefined ? { applyInflation: payload.apply_inflation } : {}),
    })
    return command(awardPath(awardId), payload, 'PATCH')
  },

  deleteCustomAward(awardId: string): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeCommand('game_delete_custom_award', { awardId })
    return command(awardPath(awardId), undefined, 'DELETE')
  },

  buyCustomAward(awardId: string, count = 1): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeCommand('game_buy_custom_award', { awardId, count })
    return command(`${awardPath(awardId)}/buy`, { count })
  },

  sellCustomAward(awardId: string, count = 1): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeCommand('game_sell_custom_award', { awardId, count })
    return command(`${awardPath(awardId)}/sell`, { count })
  },

  useCustomAward(awardId: string, count = 1): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeCommand('game_use_custom_award', { awardId, count })
    return command(`${awardPath(awardId)}/use`, { count })
  },

  previewBankProduct(payload: BankProductRequest): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeCommand('game_preview_bank_product', {
      productType: payload.product_type,
      amount: payload.amount,
      days: payload.days,
      allowInterestWithdrawal: payload.allow_interest_withdrawal,
    })
    return command('/bank/preview', payload)
  },

  openBankCredit(amount: number, days: number): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeCommand('game_open_bank_credit', { amount, days })
    return command('/bank/credit', { amount, days })
  },

  openBankDeposit(
    amount: number,
    days: number,
    allowInterestWithdrawal: boolean,
  ): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeCommand('game_open_bank_deposit', {
      amount, days, allowInterestWithdrawal,
    })
    return command('/bank/deposit', {
      amount,
      days,
      allow_interest_withdrawal: allowInterestWithdrawal,
    })
  },

  processBankEvents(autoPay = true): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeGame<GameCommandResponse>('game_process_bank_events', { autoPay })
    return command('/bank/process', { auto_pay: autoPay })
  },

  makeBankLoanPayment(): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeCommand('game_make_bank_loan_payment')
    return command('/bank/credit/payment')
  },

  partiallyRepayBankCredit(amount: number): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeCommand('game_partially_repay_bank_credit', { amount })
    return command('/bank/credit/partial-repayment', { amount })
  },

  repayBankCredit(): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeCommand('game_repay_bank_credit')
    return command('/bank/credit/repay')
  },

  topUpBankDeposit(amount: number): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeCommand('game_top_up_bank_deposit', { amount })
    return command('/bank/deposit/top-up', { amount })
  },

  withdrawBankDeposit(allowEarly = false): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeCommand('game_withdraw_bank_deposit', { allowEarly })
    return command('/bank/deposit/withdraw', { allow_early: allowEarly })
  },

  withdrawBankDepositInterest(): Promise<GameCommandResponse> {
    if (isDesktopGame()) return nativeCommand('game_withdraw_bank_interest')
    return command('/bank/deposit/interest/withdraw')
  },
}

function nativeCommand(
  commandName: string,
  payload?: Record<string, unknown>,
): Promise<GameCommandResponse> {
  return nativeGame<GameCommandResponse>(commandName, payload)
}

function nativeInventoryPayload(payload: InventoryCommand): Record<string, unknown> {
  return { category: payload.category, itemId: payload.item_id, count: payload.count }
}

function nativeDeveloperStreakPayload(payload: DeveloperStreakRequest): Record<string, unknown> {
  return {
    type: payload.type,
    ...(payload.project_id ? { projectId: payload.project_id } : {}),
    ...(payload.stage_id ? { stageId: payload.stage_id } : {}),
    ...(payload.length === undefined ? {} : { length: payload.length }),
  }
}
