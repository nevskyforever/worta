<script setup lang="ts">
import { computed } from 'vue'
import { IonIcon } from '@ionic/vue'
import { calendarOutline, documentTextOutline } from 'ionicons/icons'
import ProgressRing from '@/components/ui/ProgressRing.vue'
import ProgressBar from '@/components/ui/ProgressBar.vue'
import { useProjectPresentation } from '@/composables/useProjectPresentation'
import { useLocaleStore } from '@/stores/locale'
import type { Project } from '@/types/api'
import type { BrowserProgressView } from '@/platform/web/progressView'

const props = defineProps<{ project: Project; progress?: BrowserProgressView; parent?: string }>()
const locale = useLocaleStore(), t = locale.translate
const presentation = useProjectPresentation(() => props.project)
const goal = computed(() => props.progress?.goal ?? props.project.goal)
const infinite = computed(() => props.progress ? props.progress.goal === null : props.project.infinite)
function quantity(value: number): string {
  return `${locale.formatNumber(value, props.project.unit === 'symbols' ? 0 : 2)} ${locale.formatUnit(props.project.unit, value)}`
}
</script>

<template>
  <div class="cloud-overview" data-testid="cloud-project-details">
    <header class="cloud-overview__heading">
      <div>
        <p v-if="parent" class="cloud-overview__parent">{{ parent }}</p>
        <span class="cloud-overview__status">{{ presentation.statusLabel }}</span>
        <h3>{{ project.name }}</h3>
      </div>
      <ProgressRing v-if="progress" size="large" :value="progress.progress" :infinite="infinite"
        :label="`${t('Прогресс')} · ${presentation.progressLabel}`" />
    </header>
    <slot />
    <div v-if="progress || goal !== null || infinite" class="cloud-overview__measure">
      <div v-if="progress"><span>{{ t('Написано') }}</span><strong>{{ quantity(progress.total) }}</strong></div>
      <div><span>{{ t('Цель') }}</span><strong>{{ infinite ? t('Без лимита') : goal === null ? '—' : quantity(goal) }}</strong></div>
      <ProgressBar v-if="progress && !infinite" class="cloud-overview__bar" :value="progress.progress" :label="t('Прогресс')" />
    </div>
    <div class="cloud-overview__metrics">
      <div>
        <IonIcon :icon="calendarOutline" aria-hidden="true" />
        <p><span>{{ t('Срок') }}</span><strong>{{ locale.formatDate(project.deadline) }}</strong></p>
      </div>
      <div v-if="progress">
        <IonIcon :icon="documentTextOutline" aria-hidden="true" />
        <p><span>{{ t('Записей прогресса') }}</span><strong>{{ locale.formatNumber(progress.entries.length, 0) }}</strong></p>
      </div>
    </div>
  </div>
</template>

<style scoped>
.cloud-overview { display: grid; gap: var(--nf-space-4); margin-block: var(--nf-space-3); }
.cloud-overview__heading { display: flex; align-items: center; justify-content: space-between; gap: var(--nf-space-4); min-width: 0; }
.cloud-overview__heading > div:first-child { min-width: 0; }
.cloud-overview .cloud-overview__heading h3 { margin: var(--nf-space-2) 0 0; font-size: clamp(1.9rem, 5vw, 3rem); line-height: 1.1; overflow-wrap: anywhere; }
.cloud-overview__parent { margin: 0 0 var(--nf-space-2); color: var(--nf-color-text-muted); }
.cloud-overview__status { display: inline-flex; padding: .25rem .65rem; border-radius: 999px; background: var(--nf-color-primary-soft); color: var(--nf-color-primary); font-weight: 700; font-size: .78rem; }
.cloud-overview__measure, .cloud-overview__metrics > div { border: 1px solid var(--nf-color-border); border-radius: var(--nf-radius-lg); background: var(--nf-color-surface); padding: var(--nf-space-4); }
.cloud-overview__measure { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, 12rem), 1fr)); gap: var(--nf-space-4); box-shadow: var(--nf-shadow-card); }
.cloud-overview__bar { grid-column: 1 / -1; }
.cloud-overview__measure span, .cloud-overview__metrics span { display: block; color: var(--nf-color-text-muted); font-size: .8rem; }
.cloud-overview__measure strong { display: block; margin-top: .2rem; font-size: clamp(1.05rem, 3vw, 1.35rem); }
.cloud-overview__metrics { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, 13rem), 1fr)); gap: var(--nf-space-3); }
.cloud-overview__metrics > div { display: flex; align-items: center; gap: var(--nf-space-3); border-radius: var(--nf-radius-md); }
.cloud-overview__metrics ion-icon { color: var(--nf-color-primary); font-size: 1.2rem; }
.cloud-overview__metrics p { margin: 0; }
@media (max-width: 37.5rem) { .cloud-overview__heading { align-items: flex-start; flex-wrap: wrap; } }
</style>
