<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue'
import { IonIcon, IonSpinner } from '@ionic/vue'
import { addCircleOutline, chevronDownOutline, layersOutline, trashOutline } from 'ionicons/icons'

import FriendlyStatus from '@/components/settings/FriendlyStatus.vue'
import { useLocaleStore } from '@/stores/locale'
import { normalizeProgressTotal } from '@/core/progress/calculations'
import type { ProgressCreate, ProgressEntry, Project } from '@/types/api'
import type { SyncSummary } from '@/types/integrations'
import { convertProjectUnit } from '@/utils/projectPlanning'

const props = withDefaults(
  defineProps<{
    project: Project
    busy: boolean
    submitting?: boolean
    fixedStageId?: string | null
    syncs?: SyncSummary[]
    textSymbols?: Record<string, number>
    error?: string | null
    success?: string | null
  }>(),
  { submitting: false, syncs: () => [], textSymbols: () => ({}), error: null, success: null, fixedStageId: null },
)

const emit = defineEmits<{
  record: [payload: ProgressCreate]
  remove: [entryId: string, stageId?: string]
}>()

const selectedEntityId = defineModel<string>({ default: '' })
const locale = useLocaleStore()
const t = locale.translate
const newTotal = ref('')
const validationError = ref<string | null>(null)
const validationSummary = ref<HTMLElement | null>(null)

const selectedEntity = computed<Project>(() => {
  if (props.fixedStageId) {
    return props.project.stages.find((stage) => stage.id === props.fixedStageId) ?? props.project
  }
  if (!selectedEntityId.value) return props.project
  return props.project.stages.find((stage) => stage.id === selectedEntityId.value) ?? props.project
})
const sharedProject = computed(() => props.project.name === 'Общий проект')
const lifecycleReadOnly = computed(
  () => sharedProject.value
    || props.project.status === 'завершен'
    || selectedEntity.value.status === 'завершен',
)
const entries = computed(() => [...selectedEntity.value.progress_entries].reverse())
const fractionDigits = computed(() => (props.project.unit === 'symbols' ? 0 : 2))
const selectedStageId = computed(() =>
  selectedEntity.value.id === props.project.id ? undefined : selectedEntity.value.id,
)
const manualEntryLocked = computed(() => lifecycleReadOnly.value || selectedEntity.value.work_method !== 'manual')
const textSymbols = computed(() => props.textSymbols[selectedEntity.value.id] ?? 0)
const applicationMethod = computed(() => selectedEntity.value.work_method === 'app')
const textTotal = computed(() => convertProjectUnit(textSymbols.value, 'symbols', selectedEntity.value.unit))
const hasTextSource = computed(() => applicationMethod.value
  && textSymbols.value > 0
  && Math.abs(textTotal.value - selectedEntity.value.total) >= 0.009)

function numberFrom(value: string | number): number {
  return Number(String(value).replace(',', '.'))
}

async function record(): Promise<void> {
  const rawTotal = numberFrom(newTotal.value)
  const total = Number.isFinite(rawTotal) && rawTotal >= 0
    ? normalizeProgressTotal(rawTotal, selectedEntity.value.unit)
    : rawTotal
  validationError.value = null
  if (!Number.isFinite(total) || total < 0) {
    validationError.value = t('Новое общее значение не может быть отрицательным.')
  } else if (Math.abs(total - selectedEntity.value.total) < 0.000001) {
    validationError.value = t('Введите значение, отличающееся от текущего.')
  }
  if (validationError.value) {
    await nextTick()
    validationSummary.value?.focus()
    return
  }
  emit('record', { new_total: total, stage_id: selectedStageId.value ?? null,...(selectedEntity.value.progress_heads===undefined?{}:{expected_heads:[...selectedEntity.value.progress_heads]}) })
}

function requestRemove(entry: ProgressEntry): void {
  if (!window.confirm(t('Удалить эту запись прогресса? Итоговое значение будет пересчитано.'))) return
  emit('remove', entry.id, selectedStageId.value)
}

watch(
  () => [selectedEntity.value.id, selectedEntity.value.total] as const,
  ([, total]) => {
    newTotal.value = String(total)
    validationError.value = null
  },
  { immediate: true },
)
</script>

<template>
  <section class="progress-workspace" aria-labelledby="progress-workspace-heading">
    <div class="workspace-section-heading">
      <div>
        <p>{{ t('Рабочий ритм') }}</p>
        <h2 id="progress-workspace-heading">{{ t('Запись прогресса') }}</h2>
      </div>
    </div>

    <p v-if="manualEntryLocked" class="read-only-note">
      {{ sharedProject
        ? t('Прогресс Общего проекта обновляется через синхронизацию.')
        : selectedEntity.work_method === 'sync'
          ? t('Включена синхронизация. Ручная запись прогресса недоступна.')
          : selectedEntity.work_method === 'app'
            ? t('Прогресс добавляется из текста во встроенном редакторе.')
          : t(sharedProject ? 'Завершённый проект или источник доступен только для просмотра.' : 'Завершённый проект или этап доступен только для просмотра.') }}
    </p>

    <div class="progress-entry-layout">
      <form v-if="selectedEntity.work_method === 'manual'" class="progress-entry-form" novalidate @submit.prevent="record">
        <div class="progress-entry-heading">
          <h3>{{ t('Новая запись:') }}</h3>
        </div>
        <div class="progress-entry-fields">
          <div>
            <span>{{ t('Текущее значение') }}</span>
            <strong>{{ locale.formatNumber(selectedEntity.total, fractionDigits) }}</strong>
          </div>
          <label v-if="project.stages.length && !fixedStageId" class="progress-stage-select" for="progress-entity">
            <span>{{ t(sharedProject ? 'Источник' : 'Этап') }}</span>
            <span class="progress-stage-select__control">
              <IonIcon :icon="layersOutline" aria-hidden="true" />
              <select id="progress-entity" v-model="selectedEntityId" :disabled="busy || lifecycleReadOnly">
                <option v-for="stage in project.stages" :key="stage.id" :value="stage.id">
                  {{ stage.name }}{{ stage.status === 'завершен' ? ` — ${t('завершён')}` : '' }}
                </option>
              </select>
              <IonIcon class="progress-stage-select__arrow" :icon="chevronDownOutline" aria-hidden="true" />
            </span>
          </label>
          <label for="progress-new-total">
            <span>{{ t('Новое общее значение') }}</span>
            <input
              id="progress-new-total"
              v-model="newTotal"
              type="number"
              min="0"
              step="any"
              inputmode="decimal"
              :disabled="busy || manualEntryLocked"
            />
          </label>
          <button class="nf-button" type="submit" :disabled="busy || manualEntryLocked">
            <IonSpinner v-if="submitting" name="crescent" aria-hidden="true" />
            <IonIcon v-else :icon="addCircleOutline" aria-hidden="true" />
            {{ submitting ? t('Сохраняем…') : t('Записать') }}
          </button>
        </div>
      </form>
      <div v-else-if="applicationMethod" class="progress-entry-form">
        <div class="progress-entry-heading"><h3>{{ t('Текст документа') }}</h3></div>
        <p>{{ t('В документе') }}: <strong>{{ locale.formatNumber(textSymbols, 0) }} {{ locale.formatUnit('symbols', textSymbols) }}</strong></p>
        <p class="read-only-note">{{ hasTextSource ? t('Добавление записи доступно в редакторе текста.') : t('Текущий объём текста уже записан в прогрессе.') }}</p>
      </div>

      <div class="progress-feedback" aria-live="polite">
        <p v-if="validationError" ref="validationSummary" class="feedback-error" role="alert" tabindex="-1">
          {{ validationError }}
        </p>
        <FriendlyStatus v-else-if="error?.startsWith('progress_')" :code="error" subsystem="sync" />
        <p v-else-if="error" class="feedback-error" role="alert">{{ error }}</p>
        <p v-else-if="success" class="feedback-success">{{ success }}</p>
      </div>
    </div>

    <div class="history-heading">
      <h3>{{ t('История') }}</h3>
      <span>{{ t('Записей') }}: {{ locale.formatNumber(entries.length, 0) }}</span>
    </div>
    <div v-if="entries.length" class="history-table-wrap">
      <table class="history-table">
        <caption class="visually-hidden">{{ t('История прогресса для «{name}»', { name: selectedEntity.name }) }}</caption>
        <thead>
          <tr>
            <th scope="col">{{ t('Дата') }}</th>
            <th scope="col">{{ t('Изменение') }}</th>
            <th scope="col">{{ t('Итого') }}</th>
            <th scope="col"><span class="visually-hidden">{{ t('Действия') }}</span></th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="entry in entries" :key="entry.id">
            <td>{{ locale.formatDate(entry.created_at) }}</td>
            <td :class="entry.added < 0 ? 'negative-value' : 'positive-value'">
              {{ entry.added > 0 ? '+' : '' }}{{ locale.formatNumber(entry.added, fractionDigits) }}
            </td>
            <td>{{ locale.formatNumber(entry.new_total, fractionDigits) }}</td>
            <td class="history-action-cell">
              <button
                class="history-delete"
                type="button"
                :aria-label="t('Удалить запись от {date}', { date: locale.formatDate(entry.created_at) })"
                :disabled="busy || lifecycleReadOnly"
                @click="requestRemove(entry)"
              >
                <IonIcon :icon="trashOutline" aria-hidden="true" />
              </button>
            </td>
          </tr>
        </tbody>
      </table>
    </div>
    <p v-else class="empty-history">{{ t('Записей прогресса пока нет.') }}</p>
  </section>
</template>

<style scoped>
.progress-workspace { margin-top: var(--nf-space-7); }
.workspace-section-heading { display: flex; gap: var(--nf-space-4); align-items: flex-end; justify-content: space-between; margin-bottom: var(--nf-space-4); }
.workspace-section-heading p { margin: 0 0 var(--nf-space-1); color: var(--nf-color-accent); font-size: 0.72rem; font-weight: 800; letter-spacing: 0.08em; text-transform: uppercase; }
.workspace-section-heading h2 { margin: 0; color: var(--nf-color-text); font-family: var(--nf-font-serif); font-size: clamp(1.7rem, 4vw, 2.3rem); }
.read-only-note { padding: var(--nf-space-3); border-left: 0.25rem solid var(--nf-color-warning); border-radius: var(--nf-radius-sm); background: color-mix(in srgb, var(--nf-color-warning) 9%, var(--nf-color-surface)); color: var(--nf-color-text); }
.progress-entry-layout { display: grid; grid-template-columns: minmax(0, 1fr); gap: var(--nf-space-3); align-items: start; }
.progress-entry-form { display: grid; gap: var(--nf-space-3); padding: var(--nf-space-4); border: 1px solid color-mix(in srgb, var(--nf-color-primary) 38%, var(--nf-color-border)); border-radius: var(--nf-radius-md); background: linear-gradient(135deg, var(--nf-color-surface), color-mix(in srgb, var(--nf-color-primary-soft) 45%, var(--nf-color-surface))); box-shadow: var(--nf-shadow-card); }
.progress-entry-heading { display: flex; align-items: end; }
.progress-entry-form h3 { margin: 0; color: var(--nf-color-primary); font-family: var(--nf-font-serif); font-size: 1.2rem; }
.progress-entry-fields { display: grid; grid-template-columns: minmax(7rem, 0.8fr) minmax(9rem, 1fr) minmax(11rem, 1.3fr) auto; gap: var(--nf-space-3); align-items: end; }
.progress-entry-fields > div,
.progress-entry-fields label,
.progress-stage-select { display: grid; gap: var(--nf-space-1); }
.progress-entry-form span { color: var(--nf-color-text-muted); font-size: 0.75rem; font-weight: 700; }
.progress-stage-select { min-width: 0; }
.progress-stage-select__control { display: grid; position: relative; grid-template-columns: auto minmax(0, 1fr) auto; gap: var(--nf-space-2); align-items: center; min-height: 2.75rem; padding: 0 0.7rem; border: 1px solid var(--nf-color-border); border-radius: var(--nf-radius-sm); background: var(--nf-color-surface-raised); color: var(--nf-color-primary); transition: border-color 140ms ease, box-shadow 140ms ease; }
.progress-stage-select__control:focus-within { border-color: var(--nf-color-primary); box-shadow: 0 0 0 3px var(--nf-color-primary-soft); }
.progress-stage-select__control > :first-child { font-size: 1rem; }
.progress-stage-select select { width: 100%; min-width: 0; min-height: 2.5rem; padding: 0; border: 0; appearance: none; background: transparent; color: var(--nf-color-text); font: inherit; font-size: 0.9rem; font-weight: 700; outline: 0; }
.progress-stage-select select:disabled { color: var(--nf-color-text-muted); cursor: not-allowed; }
.progress-stage-select__arrow { color: var(--nf-color-text-muted); font-size: 0.9rem; pointer-events: none; }
.progress-entry-form strong { min-height: 3rem; padding: 0.75rem 0; color: var(--nf-color-text); font-size: 1.15rem; }
.progress-entry-form input { width: 100%; min-height: 3rem; padding: 0.65rem 0.8rem; border: 1px solid var(--nf-color-border); border-radius: var(--nf-radius-sm); background: var(--nf-color-surface-raised); color: var(--nf-color-text); }
.progress-entry-form input:focus-visible { border-color: var(--nf-color-primary); box-shadow: 0 0 0 3px var(--nf-color-primary-soft); outline: 0; }
.progress-feedback { min-height: 1rem; }
.progress-feedback p { margin: 0; padding: var(--nf-space-3); border-radius: var(--nf-radius-sm); font-size: 0.85rem; }
.feedback-error { background: color-mix(in srgb, var(--nf-color-danger) 10%, var(--nf-color-surface)); color: var(--nf-color-danger); }
.feedback-success { background: color-mix(in srgb, var(--nf-color-success) 10%, var(--nf-color-surface)); color: var(--nf-color-success); }
.history-heading { display: flex; align-items: baseline; justify-content: space-between; margin-top: var(--nf-space-5); }
.history-heading h3 { margin: 0; color: var(--nf-color-text); font-size: 1rem; }
.history-heading span { color: var(--nf-color-text-muted); font-size: 0.78rem; }
.history-table-wrap { max-height: min(24rem, 50vh); margin-top: var(--nf-space-3); overflow: auto; overscroll-behavior: contain; border: 1px solid var(--nf-color-border); border-radius: var(--nf-radius-md); background: var(--nf-color-surface); }
.history-table { width: 100%; min-width: 34rem; border-collapse: collapse; color: var(--nf-color-text); }
.history-table th,
.history-table td { padding: 0.8rem var(--nf-space-4); border-bottom: 1px solid var(--nf-color-border); text-align: left; }
.history-table th { position: sticky; top: 0; z-index: 1; background: var(--nf-color-surface); color: var(--nf-color-text-muted); font-size: 0.72rem; text-transform: uppercase; }
.history-table tbody tr:last-child td { border-bottom: 0; }
.positive-value { color: var(--nf-color-success); font-weight: 700; }
.negative-value { color: var(--nf-color-danger); font-weight: 700; }
.history-action-cell { width: 3.5rem; text-align: right !important; }
.history-delete { display: grid; width: 2.75rem; height: 2.75rem; margin-left: auto; padding: 0; place-items: center; border: 0; border-radius: var(--nf-radius-sm); background: transparent; color: var(--nf-color-danger); cursor: pointer; }
.history-delete:hover:not(:disabled) { background: color-mix(in srgb, var(--nf-color-danger) 10%, transparent); }
.history-delete:disabled { opacity: 0.4; cursor: not-allowed; }
.empty-history { margin: var(--nf-space-3) 0 0; padding: var(--nf-space-5); border: 1px dashed var(--nf-color-border); border-radius: var(--nf-radius-md); color: var(--nf-color-text-muted); }

@media (max-width: 48rem) {
  .workspace-section-heading { align-items: stretch; flex-direction: column; }
  .progress-entry-fields { grid-template-columns: 1fr; }
  .progress-stage-select { width: auto; }
}
</style>
