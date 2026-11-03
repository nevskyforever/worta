<script setup lang="ts">
import { computed } from 'vue'
import { IonIcon } from '@ionic/vue'
import {
  flameOutline,
  heartDislikeOutline,
  hourglassOutline,
  moonOutline,
  powerOutline,
  rocketOutline,
  snowOutline,
  trophyOutline,
} from 'ionicons/icons'

import { useLocaleStore } from '@/stores/locale'

type StreakScope = 'global' | 'project' | 'stage'

const props = withDefaults(defineProps<{
  length: number
  status?: string | null
  maxLength?: number
  scope?: StreakScope
  compact?: boolean
  showMax?: boolean
}>(), {
  status: 'No',
  maxLength: 0,
  scope: 'project',
  compact: false,
  showMax: false,
})

const locale = useLocaleStore()
const t = locale.translate

const title = computed(() => {
  if (props.scope === 'global') return t('Глобальный стрик')
  if (props.scope === 'stage') return t('Стрик этапа')
  return t('Стрик проекта')
})

const statusKind = computed(() => {
  const status = props.status ?? 'No'
  if (status === 'Freeze') return 'frozen'
  if (status.startsWith('Lose ')) return 'lost'
  if (status === 'Complete') return 'complete'
  if (status === 'No' || status === 'Off') return 'inactive'
  return 'active'
})

const statusLabel = computed(() => {
  const status = props.status ?? 'No'
  if (status === 'Start') return t('начат сегодня')
  if (status === 'Go') return t('продлён сегодня')
  if (status === 'Freeze') return t('заморожен')
  if (status === 'Active') return t('ещё не продлён сегодня')
  if (status === 'Complete') return t('завершён')
  if (status === 'Off') return t('выключен')
  if (status.startsWith('Lose ')) {
    const lostDays = status.split(' ')[1]
    return status.endsWith(' Start')
      ? t('потерян ({days} дн.) и начат заново', { days: lostDays ?? 0 })
      : t('потерян ({days} дн.)', { days: lostDays ?? 0 })
  }
  return t('ещё не начат')
})

// Mirror the distinct legacy streak states with vector icons rather than
// relying on one flame for every state or on emoji glyphs.
const statusIcon = computed(() => {
  const status = props.status ?? 'No'
  if (status === 'Go') return rocketOutline
  if (status === 'Freeze') return snowOutline
  if (status === 'Active') return hourglassOutline
  if (status === 'Complete') return trophyOutline
  if (status === 'Off') return powerOutline
  if (status.startsWith('Lose ')) return heartDislikeOutline
  if (status === 'No') return moonOutline
  return flameOutline
})

const accessibleLabel = computed(() => {
  const parts = [
    `${title.value}: ${locale.formatNumber(props.length, 0)} ${t('дн.')}`,
    statusLabel.value,
  ]
  if (props.showMax) {
    parts.push(`${t('Максимум')}: ${locale.formatNumber(props.maxLength, 0)} ${t('дн.')}`)
  }
  return parts.join('. ')
})
</script>

<template>
  <span
    class="streak-badge"
    :class="[
      `streak-badge--${statusKind}`,
      { 'streak-badge--compact': compact },
    ]"
    role="status"
    :aria-label="accessibleLabel"
  >
    <IonIcon :icon="statusIcon" aria-hidden="true" />
    <span class="streak-badge__copy">
      <span class="streak-badge__title">{{ title }}</span>
      <span class="streak-badge__value">
        <strong>{{ locale.formatNumber(length, 0) }}</strong> {{ t('дн.') }}
      </span>
    </span>
    <span class="streak-badge__status">{{ statusLabel }}</span>
    <span v-if="showMax" class="streak-badge__maximum">
      {{ t('Максимум') }}: {{ locale.formatNumber(maxLength, 0) }}
    </span>
  </span>
</template>

<style scoped>
.streak-badge {
  --streak-color: var(--nf-color-accent);
  display: inline-grid;
  grid-template-columns: auto auto minmax(0, 1fr);
  gap: 0.1rem var(--nf-space-2);
  align-items: center;
  min-width: 0;
  padding: 0.55rem 0.75rem;
  border: 1px solid color-mix(in srgb, var(--streak-color) 28%, var(--nf-color-border));
  border-radius: var(--nf-radius-pill);
  background: color-mix(in srgb, var(--streak-color) 8%, var(--nf-color-surface));
  color: var(--nf-color-text);
}

.streak-badge > ion-icon {
  grid-row: 1 / 3;
  color: var(--streak-color);
  font-size: 1.2rem;
}

.streak-badge__copy {
  display: flex;
  gap: var(--nf-space-1);
  align-items: baseline;
  min-width: max-content;
}

.streak-badge__title,
.streak-badge__status,
.streak-badge__maximum {
  color: var(--nf-color-text-muted);
  font-size: 0.72rem;
}

.streak-badge__title { font-weight: 750; }
.streak-badge__value { color: var(--streak-color); font-size: 0.76rem; }
.streak-badge__value strong { font-size: 0.98rem; }
.streak-badge__status { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.streak-badge__maximum { grid-column: 2 / -1; }

.streak-badge--frozen { --streak-color: var(--nf-color-primary); }
.streak-badge--lost { --streak-color: var(--nf-color-danger); }
.streak-badge--complete { --streak-color: var(--nf-color-success); }
.streak-badge--inactive { --streak-color: var(--nf-color-text-muted); }

.streak-badge--compact {
  grid-template-columns: auto auto;
  padding: 0.38rem 0.62rem;
}

.streak-badge--compact > ion-icon { grid-row: auto; font-size: 1rem; }
.streak-badge--compact .streak-badge__copy { gap: 0.3rem; }
.streak-badge--compact .streak-badge__title { display: none; }
.streak-badge--compact .streak-badge__status,
.streak-badge--compact .streak-badge__maximum { display: none; }
</style>
