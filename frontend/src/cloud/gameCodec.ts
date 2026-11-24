import { normalizeAuthenticatedFrame } from './frameCompression'
import { canonical, timestamp } from './projectMetadataCodec'
import { progressMicros, type ProgressFact } from './progressCodec'

/** Frozen C18.5.06 frames; migration and reader activation belong to the runtime. */
export const GAME_CODEC = { project: 12, account: 13 } as const
export const GAME_LIMITS = { frameBytes: 1024 * 1024, parents: 64, inventory: 512, claims: 4096, days: 4096 } as const
export type GameRule = 'legacy-game-v1' | 'native-game-v1' | 'python-game-v1'
export interface StreakBase {
  history: Array<{ day: string; frozen: boolean }>
  maximum: number
  freezes: number
  enabled: boolean
  last_reward_day: string | null
  lost_day: string | null
  lost_length: number
}
export interface GameReward { coins: string; experience: string }
export interface AccountGameBase {
  coins: string; experience: string; level: number; available_skill_points: number
  skill_points_awarded_for_level: number
  skills: { productivity: number; profitability: number; endurance: number }
  inspiration: string
  inventory: Array<{ category: string; item_id: string; count: number }>
  completion_claims: string[]
  global_streak: StreakBase
  health: string; max_health: string; coin_coefficient: string; experience_coefficient: string; health_recovery_coefficient: string
  writing_bonus: string; productive_actions: number
  creative_event_pending: 'absent' | 'none' | 'unexpected_idea'
}
export interface GameHeader {
  account_id: string; device_id: string; event_id: string; entity_id: string
  parents: string[]; revision: number; updated_at: string; rule: GameRule
}
export interface ProjectGameHeader extends GameHeader {
  scope: 'project'; project_id: string; stage_id: string | null
  bootstrap_id: string; metadata_event_id: string; stage_event_ids: string[]
}
export interface AccountGameHeader extends GameHeader { scope: 'account'; entity_type: 'account_game' }
export type ProjectGameAction =
  | { kind: 'genesis' | 'adopt_local'; base: StreakBase; completion_claimed: boolean }
  | { kind: 'writing'; progress_event_id: string; progress_entity_id: string; fact: ProgressFact
      inspiration: string; writing_bonus: string; coin_coefficient: string; experience_coefficient: string; reward: GameReward }
  | { kind: 'completion'; completion_id: string; progress_event_id: string; progress_entity_id: string; total_symbols: string; reward: GameReward }
  | { kind: 'streak'; writing_day: string; progress_event_ids: string[]; before: StreakBase; after: StreakBase; reward: GameReward }
  | { kind: 'freeze'; writing_day: string; account_action_id: string; before: StreakBase; after: StreakBase }
  | { kind: 'compensation'; target_action_id: string; reward: GameReward }
  | { kind: 'resolution'; selected_event_id: string }
export type AccountGameAction =
  | { kind: 'genesis' | 'adopt_local'; base: AccountGameBase }
  | { kind: 'reward'; reward_id: string; project_id: string; project_action_id: string; reward: GameReward }
  | { kind: 'inventory'; operation: 'buy' | 'sell' | 'use'; category: string; item_id: string; count: number; unit_price: string; before_count: number; after_count: number; coins_delta: string }
  | { kind: 'global_streak'; writing_day: string; project_action_ids: string[]; before: StreakBase; after: StreakBase; reward: GameReward }
  | { kind: 'freeze'; writing_day: string; project_id: string | null; project_action_id: string | null; before_count: number; after_count: number; before: StreakBase | null; after: StreakBase | null }
  | { kind: 'compensation'; target_action_id: string; reward: GameReward }
  | { kind: 'resolution'; selected_event_id: string }
export interface ProjectGameEvent { version: 1; header: ProjectGameHeader; action: ProjectGameAction }
export interface AccountGameEvent { version: 1 | 2; header: AccountGameHeader; action: AccountGameAction }
export type GameEvent = ProjectGameEvent | AccountGameEvent
export class GameCodecError extends Error { constructor(readonly code: string) { super(code) } }
function fail(code = 'invalid_game_payload'): never { throw new GameCodecError(code) }
const encoder = new TextEncoder(), decoder = new TextDecoder('utf-8', { fatal: true })
const uuid = (v: unknown): v is string => typeof v === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(v)
const identity = (v: unknown): v is string => typeof v === 'string' && !!v && encoder.encode(v).length <= 512 && !v.includes('\0') && !/[\uD800-\uDFFF]/u.test(v)
const integer = (v: unknown, max = 1000000): v is number => Number.isSafeInteger(v) && Number(v) >= 0 && Number(v) <= max
function exact(v: unknown, keys: string[]): Record<string, unknown> {
  if (!v || typeof v !== 'object' || Array.isArray(v)) fail()
  const o = v as Record<string, unknown>
  if (Object.keys(o).length !== keys.length || keys.some(k => !Object.hasOwn(o, k))) fail()
  return o
}
function amount(v: unknown, signed = false): bigint {
  let n: bigint
  try { n = progressMicros(v) } catch { fail() }
  if (!signed && n! < 0n) fail()
  return n!
}
function day(v: unknown): v is string { return typeof v === 'string' && /^\d{4}-\d{2}-\d{2}$/.test(v) && timestamp(`${v}T00:00:00.000000Z`) }
function heads(v: unknown, nonempty = false): asserts v is string[] {
  if (!Array.isArray(v) || v.length > GAME_LIMITS.parents || nonempty && !v.length || v.some((s, i) => !uuid(s) || i > 0 && String(v[i - 1]) >= String(s))) fail()
}
function reward(v: unknown, signed = false) { const r = exact(v, ['coins', 'experience']); amount(r.coins, signed); amount(r.experience, signed) }
function validateLoss(a: Record<string, unknown>) {
  const before = a.before as StreakBase, after = a.after as StreakBase
  const last = before.history.at(-1)?.day
  if (!last || !before.enabled || (a.project_action_ids as unknown[]).length || canonical(a.reward) !== canonical({coins:'0.000000',experience:'0.000000'})) fail()
  const lossDay = new Date(Date.parse(last + 'T00:00:00Z') + 2 * 86400000).toISOString().slice(0,10)
  if (String(a.writing_day) < lossDay || before.lost_day !== null && before.lost_day >= last) fail()
  if (canonical(after) !== canonical({...before, history:[], freezes:0, maximum:Math.max(before.maximum,before.history.length), lost_day:lossDay, lost_length:before.history.length})) fail()
}

export function globalFreezeAfter(before: StreakBase, writingDay: string): StreakBase {
  streak(before)
  const last = before.history.at(-1)?.day
  if (!day(writingDay) || !last || !before.enabled
    || Date.parse(writingDay + 'T00:00:00Z') - Date.parse(last + 'T00:00:00Z') !== 86_400_000) {
    fail('game_invalid_rule')
  }
  const after = { ...before, history: [...before.history, { day: writingDay, frozen: true }],
    freezes: before.history.filter(entry => entry.frozen).length + 1 }
  streak(after)
  return after
}
function streak(v: unknown) {
  const s = exact(v, ['history', 'maximum', 'freezes', 'enabled', 'last_reward_day', 'lost_day', 'lost_length'])
  if (!Array.isArray(s.history) || s.history.length > GAME_LIMITS.days) fail('game_resource_limit')
  let previous = ''
  for (const entry of s.history) { const e = exact(entry, ['day', 'frozen']); if (!day(e.day) || typeof e.frozen !== 'boolean' || e.day <= previous) fail(); previous = e.day }
  if (!integer(s.maximum) || !integer(s.freezes) || !integer(s.lost_length) || typeof s.enabled !== 'boolean' || s.last_reward_day !== null && !day(s.last_reward_day) || s.lost_day !== null && !day(s.lost_day)) fail()
}
function base(v: unknown) {
  const b = exact(v, ['coins', 'experience', 'level', 'available_skill_points', 'skill_points_awarded_for_level', 'skills', 'inspiration', 'inventory', 'completion_claims', 'global_streak', 'health', 'max_health', 'coin_coefficient', 'experience_coefficient', 'health_recovery_coefficient', 'writing_bonus', 'productive_actions', 'creative_event_pending'])
  amount(b.coins); amount(b.experience)
  if (!integer(b.level, 99) || Number(b.level) < 1 || !integer(b.available_skill_points) || !integer(b.skill_points_awarded_for_level, 99) || Number(b.skill_points_awarded_for_level) < 1 || amount(b.inspiration) > 100000000n) fail()
  const skills = exact(b.skills, ['productivity', 'profitability', 'endurance']); if (Object.values(skills).some(v => !integer(v))) fail()
  if (!Array.isArray(b.inventory) || b.inventory.length > GAME_LIMITS.inventory || !Array.isArray(b.completion_claims) || b.completion_claims.length > GAME_LIMITS.claims) fail('game_resource_limit')
  let previous = ''
  for (const item of b.inventory) { const i = exact(item, ['category', 'item_id', 'count']); const key = `${i.category}\0${i.item_id}`; if (!identity(i.category) || !identity(i.item_id) || !integer(i.count, 10000) || key <= previous) fail(); previous = key }
  const claims = b.completion_claims
  if (claims.some((v, i) => !identity(v) || i > 0 && String(claims[i - 1]) >= v)) fail()
  streak(b.global_streak)
  for (const key of ['health', 'max_health', 'coin_coefficient', 'experience_coefficient', 'health_recovery_coefficient', 'writing_bonus']) amount(b[key])
  if (amount(b.health) > amount(b.max_health) || !integer(b.productive_actions) || !['absent', 'none', 'unexpected_idea'].includes(String(b.creative_event_pending))) fail()
}
const COMMON = ['account_id', 'device_id', 'event_id', 'entity_id', 'parents', 'revision', 'updated_at', 'rule', 'scope']
export function validateGameEvent(value: unknown): asserts value is GameEvent {
  const e = exact(value, ['version', 'header', 'action']), raw = e.header as Record<string, unknown>
  const project = raw?.scope === 'project'
  const h = exact(raw, project ? [...COMMON, 'project_id', 'stage_id', 'bootstrap_id', 'metadata_event_id', 'stage_event_ids'] : [...COMMON, 'entity_type'])
  if (!(e.version === 1 || e.version === 2 && h.scope === 'account' && (e.action as Record<string, unknown>)?.kind === 'global_streak' && h.rule === 'native-game-v1') || !uuid(h.account_id) || !uuid(h.device_id) || !uuid(h.event_id) || !identity(h.entity_id) || !timestamp(h.updated_at) || !integer(h.revision, Number.MAX_SAFE_INTEGER) || Number(h.revision) < 1 || !['legacy-game-v1', 'native-game-v1', 'python-game-v1'].includes(String(h.rule))) fail()
  heads(h.parents)
  if (h.parents.includes(h.event_id)) fail()
  if (project) {
    heads(h.stage_event_ids)
    if (!identity(h.project_id) || h.stage_id !== null && !identity(h.stage_id) || !uuid(h.bootstrap_id) || !uuid(h.metadata_event_id) || (h.stage_id === null) !== (h.stage_event_ids.length === 0) || h.entity_id !== `game:${h.stage_id === null ? 'project' : `stage:${h.stage_id}`}:${h.event_id}`) fail()
  } else if (h.scope !== 'account' || h.entity_type !== 'account_game' || h.entity_id !== `game:${h.event_id}`) fail()
  if (!e.action || typeof e.action !== 'object' || Array.isArray(e.action)) fail()
  const a = e.action as Record<string, unknown>
  const genesis = a.kind === 'genesis', adoption = a.kind === 'adopt_local', resolution = a.kind === 'resolution'
  if (genesis ? h.parents.length !== 0 || h.revision !== 1 : !h.parents.length || Number(h.revision) < 2) fail()
  if ((genesis || adoption) !== (h.rule === 'legacy-game-v1')) fail()
  if (!genesis && !adoption && !resolution && h.parents.length !== 1) fail()
  if (resolution) { exact(a, ['kind', 'selected_event_id']); if (!uuid(a.selected_event_id) || !h.parents.includes(a.selected_event_id)) fail(); return }
  if (a.kind === 'compensation') { exact(a, ['kind', 'target_action_id', 'reward']); if (!uuid(a.target_action_id)) fail(); reward(a.reward, true); return }
  if (project) {
    switch (a.kind) {
      case 'genesis': case 'adopt_local': exact(a, ['kind', 'base', 'completion_claimed']); streak(a.base); if (typeof a.completion_claimed !== 'boolean') fail(); break
      case 'writing': {
        exact(a, ['kind', 'progress_event_id', 'progress_entity_id', 'fact', 'inspiration', 'writing_bonus', 'coin_coefficient', 'experience_coefficient', 'reward'])
        if (!uuid(a.progress_event_id) || a.progress_entity_id !== (h.stage_id === null ? 'project' : `stage:${h.stage_id}`)) fail()
        const f = exact(a.fact, ['entry_id', 'new_total', 'delta', 'unit', 'occurred_at', 'writing_time', 'writing_day'])
        if (!identity(f.entry_id) || !['symbols', 'A4', 'author_list', 'ficbook_pages'].includes(String(f.unit)) || !day(f.writing_day) || !timestamp(f.occurred_at) || f.writing_time !== null || amount(f.delta) <= 0n) fail()
        amount(f.new_total); if (amount(a.inspiration) > 100000000n) fail(); amount(a.writing_bonus); amount(a.coin_coefficient); amount(a.experience_coefficient); reward(a.reward); break
      }
      case 'completion': exact(a, ['kind', 'completion_id', 'progress_event_id', 'progress_entity_id', 'total_symbols', 'reward']); if (!uuid(a.progress_event_id) || a.progress_entity_id !== (h.stage_id === null ? 'project' : `stage:${h.stage_id}`) || a.completion_id !== `completion:${canonical([h.project_id, h.stage_id])}`) fail(); amount(a.total_symbols); reward(a.reward); break
      case 'streak': exact(a, ['kind', 'writing_day', 'progress_event_ids', 'before', 'after', 'reward']); if (!day(a.writing_day)) fail(); heads(a.progress_event_ids, true); streak(a.before); streak(a.after); reward(a.reward); break
      case 'freeze': exact(a, ['kind', 'writing_day', 'account_action_id', 'before', 'after']); if (!day(a.writing_day) || !uuid(a.account_action_id)) fail(); streak(a.before); streak(a.after); break
      default: fail('game_action_unsupported')
    }
  } else {
    switch (a.kind) {
      case 'genesis': case 'adopt_local': exact(a, ['kind', 'base']); base(a.base); break
      case 'reward': exact(a, ['kind', 'reward_id', 'project_id', 'project_action_id', 'reward']); if (!identity(a.project_id) || !uuid(a.project_action_id) || a.reward_id !== `reward:${a.project_action_id}`) fail(); reward(a.reward); break
      case 'inventory': exact(a, ['kind', 'operation', 'category', 'item_id', 'count', 'unit_price', 'before_count', 'after_count', 'coins_delta']); if (!['buy', 'sell', 'use'].includes(String(a.operation)) || !identity(a.category) || !identity(a.item_id) || !integer(a.count, 10000) || Number(a.count) < 1 || !integer(a.before_count, 10000) || !integer(a.after_count, 10000) || a.after_count !== Number(a.before_count) + (a.operation === 'buy' ? Number(a.count) : -Number(a.count))) fail(); amount(a.unit_price); amount(a.coins_delta, true); break
      case 'global_streak': exact(a, ['kind', 'writing_day', 'project_action_ids', 'before', 'after', 'reward']); if (!day(a.writing_day)) fail(); heads(a.project_action_ids, e.version !== 2); streak(a.before); streak(a.after); reward(a.reward); if (e.version === 2) validateLoss(a); break
      case 'freeze': exact(a, ['kind', 'writing_day', 'project_id', 'project_action_id', 'before_count', 'after_count', 'before', 'after']); if (!day(a.writing_day) || !integer(a.before_count, 10000) || Number(a.before_count) < 1 || a.after_count !== Number(a.before_count) - 1) fail(); if (a.project_id === null ? a.project_action_id !== null || a.before === null || a.after === null : !identity(a.project_id) || !uuid(a.project_action_id) || a.before !== null || a.after !== null) fail(); if (a.project_id === null) { streak(a.before); streak(a.after) } break
      default: fail('game_action_unsupported')
    }
  }
}
export function frameGameEvent(event: GameEvent): Uint8Array {
  validateGameEvent(event)
  const payload = encoder.encode(canonical(event))
  if (payload.length + 20 > GAME_LIMITS.frameBytes) fail('game_resource_limit')
  const frame = new Uint8Array(20 + payload.length)
  frame.set(encoder.encode('WORTA-C1')); frame.set([1, GAME_CODEC[event.header.scope], 1, 0], 8)
  const view = new DataView(frame.buffer); view.setUint32(12, payload.length); view.setUint32(16, payload.length)
  frame.set(payload, 20); return frame
}
export function unframeGameEvent(frame: Uint8Array, scope: 'project' | 'account'): GameEvent {
  if (frame.length >= 20 && frame[11] !== 0 && ([GAME_CODEC[scope]] as number[]).includes(frame[9]!) && [1].includes(frame[10]!)) frame = normalizeAuthenticatedFrame(frame, [GAME_CODEC[scope]], [1], GAME_LIMITS.frameBytes-20)

  if (frame.length > GAME_LIMITS.frameBytes) fail('game_resource_limit')
  if (frame.length < 20 || !encoder.encode('WORTA-C1').every((b, i) => frame[i] === b) || frame[8] !== 1 || frame[9] !== GAME_CODEC[scope] || frame[10] !== 1 || frame[11] !== 0) fail('game_codec_unsupported')
  const view = new DataView(frame.buffer, frame.byteOffset, frame.byteLength)
  if (view.getUint32(12) !== frame.length - 20 || view.getUint32(16) !== frame.length - 20) fail()
  let raw: string, event: unknown
  try { raw = decoder.decode(frame.subarray(20)); event = JSON.parse(raw) } catch { fail() }
  validateGameEvent(event)
  if (event.header.scope !== scope || canonical(event) !== raw!) fail()
  return event
}
