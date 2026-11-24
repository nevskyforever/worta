export type JsonValue =
  | string
  | number
  | boolean
  | null
  | JsonValue[]
  | { [key: string]: JsonValue }

export interface RewardSummary {
  coins: number
  experience: number
  inspiration?: number
}

export interface GameProfile {
  level: number
  experience: number
  next_level_experience: number | null
  coins: number
  inflation: number
  health: number
  max_health: number
  inspiration: number
  max_inspiration: number
  writing_session_streak: number
  session_streak_shields: number
  session_grade_boosts: number
  pending_bonuses: {
    writing: number
    session: number
    challenge: number
    manuscript: number
  }
}

export interface GameSkill {
  key: string
  name: string
  points: number
  target: string
  bonus: number
}

export interface GameCoefficient {
  key: string
  name: string
  description: string
  value: number
  base_value: number
}

export interface GameSkills {
  available_points: number
  points_per_level: number
  items: GameSkill[]
  coefficients: GameCoefficient[]
}

export interface GameBuff {
  name: string
  description: string
  type: string
  target: string
  value: number
  stacks: number
  duration_minutes: number | null
  started_at: string | null
  expires_at: string | null
  remaining_seconds: number | null
  source: string | null
  stackable: boolean
}

export interface GameBuffs {
  server_time: string
  positive: GameBuff[]
  negative: GameBuff[]
}

export interface ProjectStreakFreezeTarget {
  project_id: string
  name: string
  source_count: number
  max_streak: number
  sources: Array<{
    id: string
    name: string
    is_stage: boolean
    streak_length: number
  }>
}

export interface StreakFreezesState {
  date: string
  inventory_count: number
  global_available: boolean
  projects: ProjectStreakFreezeTarget[]
}

export interface GameItem {
  id: string
  key: string
  category: string
  name: string
  description: string | null
  effect?: string | null
  level?: number
  price?: number
  sell_price?: number
  count: number
  sellable: boolean
  credit_allowed?: boolean
  usable: boolean
  buy: boolean
  maximum_quantity?: number | null
  available_for_level?: boolean
  buffs?: GameBuff[]
  can_buy?: boolean
  known?: boolean
}

export interface GameItemCategory {
  key: string
  name: string
  items: GameItem[]
}

export interface GameInventory {
  categories: GameItemCategory[]
}

export interface GameNotification {
  id: string
  text: string
  tag: string | null
  created_at: string | null
  status: 'new' | 'read'
  reward?: { kind: string; coins: number; experience: number }
}

export interface GameNotifications {
  unread: GameNotification[]
  read: GameNotification[]
  unread_count: number
}

export interface GameQuest {
  id: string
  name: string
  description: string
  status: 'available' | 'active' | 'completed' | string
  required_level: number
  started_at: string | null
  finished_at: string | null
  reward: {
    coins: number
    experience: number
    items: JsonValue
    buffs: GameBuff[]
  }
}

export interface GameQuests {
  items: GameQuest[]
  by_status: Record<string, GameQuest[]>
}

export interface DailyChallengeOption {
  option_id: string
  date: string
  type: string
  name: string
  description: string
  difficulty: string
  difficulty_name: string
  target: number
  progress: number
  completed: boolean
  reward: RewardSummary
}

export interface DailyChallengeState {
  change_cost: number
  current: DailyChallengeOption | null
  options: DailyChallengeOption[]
  history: JsonValue[]
}

export interface WeeklyChallenge {
  key: string
  name: string
  description: string
  target: number
  progress?: number
  week_start?: string
  writing_days?: string[]
  completed?: boolean
  reward: RewardSummary
}

export interface WeeklyChallengeState {
  current: WeeklyChallenge | null
  catalog: WeeklyChallenge[]
}

export interface ActiveWritingSession {
  started_at: string | null
  ends_at: string | null
  duration_minutes: number
  target_symbols: number
  progress: number
  intention: string
  mode: WritingSessionModeKey
  remaining_seconds: number
}

export type WritingSessionModeKey = 'flow' | 'sprint' | 'deep' | 'editing'

export interface WritingSessionMode {
  key: WritingSessionModeKey
  name: string
  description: string
  reward_bonus: number
}

export interface WritingSessionIntention {
  key: string
  name: string
  description: string
}

export interface WritingSessionGrade {
  key: string
  name: string
  target_ratio: number
  reward_multiplier: number
}

export interface WritingSessionHistoryEntry {
  started_at?: string
  finished_at?: string
  duration_minutes?: number
  target_symbols?: number
  progress?: number
  intention?: string
  mode?: WritingSessionModeKey
  grade?: string
  grade_name?: string
  successful?: boolean
  coins?: number
  experience?: number
  [key: string]: JsonValue | undefined
}

export interface WritingSessionState {
  server_time: string
  active: ActiveWritingSession | null
  streak: number
  history: WritingSessionHistoryEntry[]
  modes: WritingSessionMode[]
  intentions: WritingSessionIntention[]
  grades: WritingSessionGrade[]
  allowed_durations_minutes: number[]
}

export interface InspirationAbility {
  key: string
  name: string
  description: string
  cost: number
  bonus: number
  active: boolean
}

export interface CreativeEvent {
  key: string
  name: string
  description: string
  safe_description: string
  risk_description: string
  safe: JsonValue
  risk: JsonValue
}

export interface InspirationState {
  abilities: InspirationAbility[]
  creative_event: CreativeEvent | null
  creative_event_history: JsonValue[]
}

export interface SpecializationAbility {
  name: string
  description: string
  cooldown_hours: number
  remaining_seconds: number
  pending: boolean
}

export interface Specialization {
  key: string
  name: string
  description: string
  selected: boolean
  mastery_experience: number
  mastery_rank: number
  passive_bonus: number
  ability: SpecializationAbility
}

export interface SpecializationsState {
  selected: string | null
  unlocks_at_level: number
  change_cooldown_days: number
  change_days_remaining: number
  mastery_thresholds: number[]
  items: Specialization[]
}

export interface ManuscriptJourney {
  owner_key: string
  owner_name: string | null
  received_milestones: number[]
}

export interface ManuscriptMilestone {
  progress: number
  name: string
  coins: number
  exp: number
  inspiration: number
}

export interface CabinetRelic {
  key: string
  unlocked: boolean
  name: string | null
  description: string | null
  condition: string
  progress: number
  required: number
  effect_type: string | null
  bonus: number | null
  effect_description: string | null
}

export interface CabinetSet {
  key: string
  name: string
  description: string
  relics: string[]
  unlocked: boolean
  effect_type: string
  bonus: number
}

export interface ManuscriptsState {
  journeys: ManuscriptJourney[]
  milestones: ManuscriptMilestone[]
  cabinet: {
    relics: CabinetRelic[]
    sets: CabinetSet[]
  }
}

export interface BankCredit {
  principal: number
  interest_rate: number
  interest: number
  total: number
  remaining: number
  daily_payment: number
  status: string
  opened_at: string | null
  return_date: string | null
  paid_amount: number
  overdue_days: number
}

export interface BankDeposit {
  principal: number
  interest_rate: number
  interest: number
  total: number
  available_interest: number
  allow_interest_withdrawal: boolean
  status: string
  opened_at: string | null
  return_date: string | null
}

export interface BankState {
  credit_score: number | null
  credit_limit?: number
  max_credit_days?: number
  credit_rate?: number
  deposit_rate?: number
  estimated_daily_income?: JsonValue
  can_open_credit?: boolean
  can_open_deposit?: boolean
  credit: BankCredit | null
  deposit: BankDeposit | null
  credit_history_count?: number
  deposit_history_count?: number
  overdue_days_total?: number
}

export interface CustomAward {
  id: string
  name: string
  description: string
  base_price: number
  apply_inflation: boolean
  price: number
  sell_price: number
  count: number
  available_in_shop: boolean
  sellable: boolean
  usable: boolean
  can_buy: boolean
}

export interface CustomAwardsState {
  items: CustomAward[]
}

export interface ShopCatalog {
  enabled?: boolean
  categories: GameItemCategory[]
  custom_awards: CustomAwardsState
}

export interface GameState {
  enabled: boolean
  server_time: string
  profile: GameProfile
  skills: GameSkills
  buffs: GameBuffs
  streak_freezes: StreakFreezesState
  notifications: GameNotifications
  inventory: GameInventory
  quests: GameQuests
  daily_challenge: DailyChallengeState
  weekly_challenge: WeeklyChallengeState
  writing_session: WritingSessionState
  inspiration: InspirationState
  specializations: SpecializationsState
  manuscripts: ManuscriptsState
  bank: BankState
  custom_awards: CustomAwardsState
  shop: ShopCatalog
}

export interface GameCommandResponse {
  ok: boolean
  message: string | null
  messages: string[]
  result: { [key: string]: JsonValue } | null
  state: GameState
}

export interface DeveloperModeState {
  state: GameState
  test_date_enabled: boolean
  test_datetime: string | null
}

export interface DeveloperProfileUpdate {
  level: number
  health: number
  coins: number
  exp: number
  test_date_enabled: boolean
  test_datetime: string | null
}

export type DeveloperStreakType = 'global' | 'project' | 'stage'

export interface DeveloperStreakTarget {
  id: string
  type: DeveloperStreakType
  name: string
  project_id?: string
  stage_id?: string
  status: string
  length: number
  max_length: number
  last_effective_day: string | null
}

export interface DeveloperStreakState {
  logical_day: string
  targets: DeveloperStreakTarget[]
}

export interface DeveloperStreakRequest {
  type: DeveloperStreakType
  project_id?: string
  stage_id?: string
  length?: number
}

export type ProfileTransferDirection = 'real_to_test' | 'test_to_real'

export interface ProfileTransferRequestResult {
  message: string
  restart_required: boolean
}

export interface ProfileTransferResult {
  status: 'complete' | 'error'
  direction?: ProfileTransferDirection
  backup?: string
  error?: string
}

export interface WritingSessionStart {
  duration_minutes: 15 | 25 | 45 | 60
  target_symbols: number
  intention: string
  mode: WritingSessionModeKey
}

export interface InventoryCommand {
  category: string
  item_id: string
  count: number
}

export interface BankProductRequest {
  product_type: 'credit' | 'deposit'
  amount: number
  days: number
  allow_interest_withdrawal: boolean
}
