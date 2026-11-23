<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'

import { useLocaleStore } from '@/stores/locale'
import AnimatedNumber from '@/components/ui/AnimatedNumber.vue'
import type { BankState, GameBuffs, GameProfile, StreakFreezesState } from '@/types/game'

const props = defineProps<{
  profile: GameProfile
  bank: BankState
  buffs: GameBuffs
  streakFreezes: StreakFreezesState
  busy: boolean
}>()

const emit = defineEmits<{
  applyFreeze: [target: 'global' | 'project', projectId?: string]
  effectsExpired: []
}>()

const locale = useLocaleStore()
const t = locale.translate

const levelProgress = computed(() => {
  if (props.profile.next_level_experience === null) return 100
  if (props.profile.next_level_experience <= 0) return 0
  return Math.min(100, (props.profile.experience / props.profile.next_level_experience) * 100)
})

const pendingBonuses = computed(() =>
  Object.entries(props.profile.pending_bonuses).filter(([, value]) => value > 0),
)
const freezeTarget = ref('global')
const clock = ref(Date.now())
let clockTimer: ReturnType<typeof setInterval> | undefined
let expiredSignature = ''
const serverOffset = ref(0)
const effectsReceivedAt = ref(clock.value)

function remainingSeconds(buff: GameBuffs['positive'][number]): number | null {
  if (!buff.expires_at) return null
  if (typeof buff.remaining_seconds === 'number') {
    return Math.ceil(buff.remaining_seconds - Math.max(0, clock.value - effectsReceivedAt.value) / 1_000)
  }
  const endsAt = Date.parse(buff.expires_at)
  return Number.isNaN(endsAt) ? null : Math.ceil((endsAt - (clock.value + serverOffset.value)) / 1_000)
}

const activeBuffs = computed(() => ({
  positive: props.buffs.positive.filter((buff) => (remainingSeconds(buff) ?? 1) > 0),
  negative: props.buffs.negative.filter((buff) => (remainingSeconds(buff) ?? 1) > 0),
}))

const selectedFreezeProject = computed(() =>
  props.streakFreezes.projects.find((project) => project.project_id === freezeTarget.value),
)

interface BuffSummary {
  target: string
  value: number
  kind: 'positive' | 'negative'
}

const buffSummaries = computed<BuffSummary[]>(() => {
  const totals = new Map<string, { positive: number; negative: number }>()
  for (const [kind, items] of [
    ['positive', activeBuffs.value.positive],
    ['negative', activeBuffs.value.negative],
  ] as const) {
    for (const buff of items) {
      const target = buff.target || 'параметр'
      const current = totals.get(target) ?? { positive: 0, negative: 0 }
      const amount = Math.abs(buff.value) * Math.max(1, buff.stacks)
      current[kind] += amount
      totals.set(target, current)
    }
  }
  return [...totals.entries()].flatMap(([target, values]) => [
    ...(values.positive > 0 ? [{ target, value: values.positive, kind: 'positive' as const }] : []),
    ...(values.negative > 0 ? [{ target, value: values.negative, kind: 'negative' as const }] : []),
  ])
})

function bonusName(key: string): string {
  const names: Record<string, string> = {
    writing: 'Запись текста',
    session: 'Писательская сессия',
    challenge: 'Испытание',
    manuscript: 'Рубеж рукописи',
  }
  return t(names[key] ?? key)
}

function targetName(key: string): string {
  const names: Record<string, string> = {
    coins: 'Монеты',
    exp: 'Опыт',
    health_recovery: 'Восстановление здоровья',
    inspiration: 'Вдохновение',
  }
  return t(names[key] ?? key)
}

function remainingLabel(buff: GameBuffs['positive'][number]): string | null {
  const seconds = remainingSeconds(buff)
  if (seconds === null || seconds <= 0) return null
  const hours = Math.floor(seconds / 3_600)
  const minutes = Math.floor((seconds % 3_600) / 60)
  const rest = seconds % 60
  return hours > 0
    ? `${String(hours).padStart(2, '0')}:${String(minutes).padStart(2, '0')}:${String(rest).padStart(2, '0')}`
    : `${String(minutes).padStart(2, '0')}:${String(rest).padStart(2, '0')}`
}

onMounted(() => { clockTimer = setInterval(() => { clock.value = Date.now() }, 1_000) })
onBeforeUnmount(() => clearInterval(clockTimer))
watch(() => props.buffs, (buffs) => {
  effectsReceivedAt.value = Date.now()
  const parsed = Date.parse(buffs.server_time)
  serverOffset.value = Number.isNaN(parsed) ? 0 : parsed - effectsReceivedAt.value
  expiredSignature = ''
}, { immediate: true })
watch(clock, () => {
  const expired = [...props.buffs.positive, ...props.buffs.negative]
    .filter((buff) => (remainingSeconds(buff) ?? 1) <= 0)
    .map((buff) => `${buff.name}:${buff.started_at}`)
    .sort()
    .join('|')
  if (!expired || expired === expiredSignature) return
  expiredSignature = expired
  emit('effectsExpired')
})
</script>

<template>
  <section class="overview" :aria-label="t('Состояние героя')">
    <div class="resource-grid">
      <article class="level-card resource-card">
        <p>{{ t('Уровень') }}</p>
        <strong><AnimatedNumber :value="profile.level" /></strong>
        <progress
          :value="levelProgress"
          max="100"
          :aria-label="t('Опыт до следующего уровня')"
        />
        <small v-if="profile.next_level_experience !== null">
          {{ locale.formatNumber(profile.experience, 0) }} /
          {{ locale.formatNumber(profile.next_level_experience, 0) }} {{ t('опыта') }}
        </small>
        <small v-else>{{ t('Максимальный уровень') }}</small>
      </article>

      <article class="resource-card">
        <p>{{ t('Монеты') }}</p>
        <strong><AnimatedNumber :value="profile.coins" /></strong>
        <small v-if="bank.credit">
          {{ t('Кредит') }}: {{ locale.formatNumber(bank.credit.remaining) }}
        </small>
        <small v-if="bank.deposit">
          {{ t('Вклад') }}: {{ locale.formatNumber(bank.deposit.total) }}
        </small>
      </article>

      <article class="resource-card">
        <p>{{ t('Здоровье') }}</p>
        <strong><AnimatedNumber :value="profile.health" /> / <AnimatedNumber :value="profile.max_health" /></strong>
        <progress
          :value="profile.health"
          :max="profile.max_health || 1"
          :aria-label="t('Запас здоровья')"
        />
      </article>

      <article class="resource-card">
        <p>{{ t('Вдохновение') }}</p>
        <strong>
          <AnimatedNumber :value="profile.inspiration" /> /
          <AnimatedNumber :value="profile.max_inspiration" />
        </strong>
        <progress
          :value="profile.inspiration"
          :max="profile.max_inspiration || 1"
          :aria-label="t('Запас вдохновения')"
        />
      </article>

      <article class="resource-card">
        <p>{{ t('Стрик сессий') }}</p>
        <strong><AnimatedNumber :value="profile.writing_session_streak" /></strong>
        <small>
          {{ t('Щиты') }}: {{ profile.session_streak_shields }} ·
          {{ t('Медали качества') }}: {{ profile.session_grade_boosts }}
        </small>
      </article>
    </div>

    <div v-if="pendingBonuses.length" class="summary-panel">
      <h2>{{ t('Ожидающие усиления') }}</h2>
      <ul class="tag-list">
        <li v-for="[key, value] in pendingBonuses" :key="key">
          {{ bonusName(key) }} +{{ locale.formatNumber(value * 100) }}%
        </li>
      </ul>
    </div>

    <section class="summary-panel freeze-panel" aria-labelledby="streak-freeze-title">
      <div>
        <h2 id="streak-freeze-title">{{ t('Заморозка стрика') }}</h2>
        <p>
          {{ t('Сохраните серию на сегодня, если не получится написать текст.') }}
          {{ t('В инвентаре') }}: <strong>{{ streakFreezes.inventory_count }}</strong>
        </p>
      </div>
      <label>
        <span>{{ t('Стрик') }}</span>
        <select v-model="freezeTarget" class="freeze-target-select">
          <option value="global">{{ t('Общий стрик') }}</option>
          <option
            v-for="project in streakFreezes.projects"
            :key="project.project_id"
            :value="project.project_id"
          >
            {{ project.name }} · {{ project.source_count }}
          </option>
        </select>
      </label>
      <button
        class="nf-button"
        type="button"
        :disabled="
          busy ||
          (freezeTarget === 'global'
            ? !streakFreezes.global_available
            : (selectedFreezeProject?.source_count ?? 0) < 1)
        "
        @click="
          freezeTarget === 'global'
            ? emit('applyFreeze', 'global')
            : emit('applyFreeze', 'project', freezeTarget)
        "
      >
        {{ t('Применить заморозку') }}
      </button>
    </section>

    <section
      v-if="buffSummaries.length"
      class="buff-summary-grid"
      :aria-label="t('Сводка эффектов')"
    >
      <article
        v-for="summary in buffSummaries"
        :key="`${summary.kind}:${summary.target}`"
        class="buff-summary-card"
        :class="`buff-summary-card--${summary.kind}`"
      >
        <strong>{{ summary.kind === 'positive' ? '+' : '−' }}{{ locale.formatNumber(summary.value, 3) }}</strong>
        <span>{{ t('к параметру') }} {{ targetName(summary.target) }}</span>
      </article>
    </section>

    <div class="buff-columns">
      <section class="summary-panel buff-panel">
        <h2>{{ t('Активные усиления') }}</h2>
        <p v-if="activeBuffs.positive.length === 0" class="empty-copy">
          {{ t('Сейчас активных усилений нет.') }}
        </p>
        <ul v-else class="buff-list">
          <li v-for="buff in activeBuffs.positive" :key="`${buff.name}:${buff.started_at}`">
            <strong>{{ t(buff.name) }}<span v-if="buff.stacks > 1"> ×{{ buff.stacks }}</span></strong>
            <span>{{ t(buff.description) }}</span>
            <time v-if="remainingLabel(buff)" class="buff-timer">{{ remainingLabel(buff) }}</time>
          </li>
        </ul>
      </section>

      <section class="summary-panel buff-panel">
        <h2>{{ t('Активные ограничения') }}</h2>
        <p v-if="activeBuffs.negative.length === 0" class="empty-copy">
          {{ t('Сейчас активных ограничений нет.') }}
        </p>
        <ul v-else class="buff-list">
          <li v-for="buff in activeBuffs.negative" :key="`${buff.name}:${buff.started_at}`">
            <strong>{{ t(buff.name) }}<span v-if="buff.stacks > 1"> ×{{ buff.stacks }}</span></strong>
            <span>{{ t(buff.description) }}</span>
            <time v-if="remainingLabel(buff)" class="buff-timer">{{ remainingLabel(buff) }}</time>
          </li>
        </ul>
      </section>
    </div>
  </section>
</template>

<style scoped>
.overview,
.buff-columns {
  display: grid;
  gap: var(--nf-space-4);
}

.resource-grid {
  display: grid;
  grid-template-columns: repeat(5, minmax(9rem, 1fr));
  gap: var(--nf-space-3);
}

.resource-card,
.summary-panel {
  border: 1px solid var(--nf-color-border);
  border-radius: var(--nf-radius-md);
  background: var(--nf-color-surface);
  box-shadow: var(--nf-shadow-card);
}

.freeze-panel {
  display: grid;
  grid-template-columns: minmax(0, 1fr) minmax(12rem, auto) auto;
  gap: var(--nf-space-4);
  align-items: end;
}

.freeze-panel p {
  margin: 0;
  color: var(--nf-color-text-muted);
  line-height: 1.5;
}

.freeze-panel label {
  display: grid;
  gap: var(--nf-space-1);
  color: var(--nf-color-text-muted);
  font-size: 0.8rem;
  font-weight: 700;
}

.freeze-target-select {
  box-sizing: border-box;
  width: 13rem;
  height: 2.75rem;
  padding: 0.6rem 0.7rem;
  border: 1px solid var(--nf-color-border);
  border-radius: var(--nf-radius-sm);
  background: var(--nf-color-surface-raised);
  color: var(--nf-color-text);
  font: inherit;
  font-weight: 700;
}

.freeze-panel > .nf-button {
  width: 13rem;
  height: 2.75rem;
}

.resource-card {
  display: grid;
  gap: var(--nf-space-2);
  min-height: 8.5rem;
  padding: var(--nf-space-4);
  align-content: start;
}

.resource-card p,
.resource-card small,
.empty-copy {
  margin: 0;
  color: var(--nf-color-text-muted);
}

.resource-card strong {
  color: var(--nf-color-text);
  font-family: var(--nf-font-serif);
  font-size: clamp(1.35rem, 3vw, 2rem);
}

progress {
  width: 100%;
  height: 0.55rem;
  overflow: hidden;
  border: 0;
  border-radius: var(--nf-radius-pill);
  accent-color: var(--nf-color-primary);
}

.summary-panel {
  padding: var(--nf-space-5);
}

.buff-summary-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(10rem, 1fr));
  gap: var(--nf-space-2);
}

.buff-summary-card {
  display: flex;
  min-height: 3.25rem;
  gap: var(--nf-space-2);
  align-items: baseline;
  padding: var(--nf-space-3);
  border: 1px solid var(--nf-color-border);
  border-radius: var(--nf-radius-sm);
  background: var(--nf-color-surface);
  box-shadow: var(--nf-shadow-card);
}

.buff-summary-card strong {
  font-size: 1.05rem;
  white-space: nowrap;
}

.buff-summary-card span {
  color: var(--nf-color-text-muted);
  font-size: 0.8rem;
}

.buff-summary-card--positive strong {
  color: var(--nf-color-success);
}

.buff-summary-card--negative strong {
  color: var(--nf-color-danger);
}

.summary-panel h2 {
  margin: 0 0 var(--nf-space-3);
  font-family: var(--nf-font-serif);
  font-size: 1.2rem;
}

.tag-list,
.buff-list {
  padding: 0;
  margin: 0;
  list-style: none;
}

.tag-list {
  display: flex;
  flex-wrap: wrap;
  gap: var(--nf-space-2);
}

.tag-list li {
  padding: 0.4rem 0.7rem;
  border-radius: var(--nf-radius-pill);
  background: var(--nf-color-primary-soft);
  color: var(--nf-color-primary);
  font-size: 0.875rem;
  font-weight: 700;
}

.buff-columns {
  grid-template-columns: repeat(2, minmax(0, 1fr));
}

.buff-list {
  display: grid;
  max-height: 13rem;
  gap: var(--nf-space-3);
  overflow-y: auto;
  padding-right: var(--nf-space-2);
}

.buff-panel {
  min-height: 17rem;
  padding: var(--nf-space-4);
}

.buff-list li {
  display: grid;
  gap: var(--nf-space-1);
  padding-bottom: var(--nf-space-2);
  border-bottom: 1px solid var(--nf-color-border);
}

.buff-list li:last-child {
  padding-bottom: 0;
  border-bottom: 0;
}

.buff-list span {
  color: var(--nf-color-text-muted);
  line-height: 1.45;
}
.buff-list .buff-timer { color: var(--nf-color-primary); font-variant-numeric: tabular-nums; font-weight: 750; }

@media (max-width: 75rem) {
  .resource-grid {
    grid-template-columns: repeat(3, minmax(10rem, 1fr));
  }
}

@media (max-width: 42rem) {
  .resource-grid,
  .buff-columns {
    grid-template-columns: 1fr;
  }

  .resource-card {
    min-height: 0;
  }

  .freeze-panel {
    grid-template-columns: 1fr;
  }

  .freeze-panel .nf-button {
    width: 100%;
  }

  .freeze-target-select {
    width: 100%;
  }
}
</style>
