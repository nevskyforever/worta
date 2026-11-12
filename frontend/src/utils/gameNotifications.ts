import type { GameCommandResponse } from '@/types/game'

export function gameResponseMessages(
  response: Pick<GameCommandResponse, 'message' | 'messages'>,
): string[] {
  const messages = response.messages.filter(
    (message): message is string => typeof message === 'string' && message.trim().length > 0,
  )
  if (messages.length) return messages

  return response.message && response.message.trim() ? [response.message] : []
}

const progressRewardMessages: Record<string, string> = {
  writing: 'Вы получили награду {0} монет и {1} оп.',
  local_streak_started: 'Получен бонус {0} монет за старт стрика в проекте.',
  local_streak_restarted: 'Получен бонус {0} монет за старт стрика в проекте (после потери).',
  local_streak_continued: 'Получен бонус {0} монет и {1} оп. за продление стрика в проекте.',
  global_streak_started: 'Получен бонус {0} монет за старт глобального стрика.',
  global_streak_continued: 'Получен бонус {0} монет и {1} оп. за продление глобального стрика.',
}

export function progressGameMessages(
  response: GameCommandResponse,
  translate: (source: string, parameters?: Record<string, string | number>) => string,
  formatNumber: (value: number) => string,
): string[] {
  const messages = gameResponseMessages(response).map(message => translate(message))
  const rewards = response.result?.progress_rewards
  if (!Array.isArray(rewards)) return messages

  for (const reward of rewards) {
    if (!reward || typeof reward !== 'object' || Array.isArray(reward)) continue
    const { kind, coins, experience } = reward
    if (typeof kind !== 'string' || typeof coins !== 'number' || typeof experience !== 'number') continue
    if (Object.hasOwn(progressRewardMessages, kind)) {
      messages.push(translate(progressRewardMessages[kind]!, { 0: formatNumber(coins), 1: formatNumber(experience) }))
    }
  }
  return messages
}
