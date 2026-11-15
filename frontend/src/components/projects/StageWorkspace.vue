<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, onUpdated, ref, watch } from 'vue'
import { IonIcon } from '@ionic/vue'
import { addOutline } from 'ionicons/icons'

import ContextActionMenu, { type ContextAction } from '@/components/ui/ContextActionMenu.vue'
import { useLocaleStore } from '@/stores/locale'
import type { Project } from '@/types/api'
import AnimatedNumber from '@/components/ui/AnimatedNumber.vue'
import ProgressRing from '@/components/ui/ProgressRing.vue'
import ProgressShareMenu from './ProgressShareMenu.vue'
import StreakBadge from './StreakBadge.vue'

export type StageSort = 'manual' | 'progress' | 'updated' | 'deadline' | 'name'

const props = withDefaults(defineProps<{
  project: Project
  busy: boolean
  sharing?: boolean
  streaksEnabled?: boolean
  stageSort?: StageSort
}>(), { sharing: false, streaksEnabled: false, stageSort: 'manual' })

const emit = defineEmits<{
  add: []
  edit: [stage: Project]
  remove: [stage: Project]
  complete: [stage: Project]
  reorder: [stageIds: string[]]
  copy: [stage: Project]
  save: [stage: Project]
  open: [stage: Project]
  sync: [stage: Project]
  sort: [sort: StageSort]
}>()

const locale = useLocaleStore()
const t = locale.translate
const readOnly = computed(() => props.project.status === 'завершен')
const sharedProject = computed(() => props.project.name === 'Общий проект')
const sort = ref<StageSort>(props.stageSort)
const stageOrderEditing = ref(false)
const manualStageIds = ref(props.project.stages.map((stage) => stage.id))
const draggedStageId = ref<string | null>(null)
const stageDragMimeType = 'application/x-nfprogress-stage-id'
const pointerStageDrag = ref<{
  stageId: string
  pointerId: number
  startX: number
  startY: number
  active: boolean
} | null>(null)
const contextStage = ref<Project | null>(null)
const contextPosition = ref({ x: 0, y: 0 })
const stagesSection = ref<HTMLElement | null>(null)
let stageSizeObserver: ResizeObserver | undefined
let stageMeasureFrame: number | undefined
const fractionDigits = computed(() => props.project.unit === 'symbols' ? 0 : 2)
const addButtonLabel = computed(() => t(sharedProject.value ? 'Добавить источник' : 'Добавить этап'))
const emptyActionLabel = computed(() => t(sharedProject.value ? 'Создать первый источник' : 'Создать первый этап'))
const removeButtonLabel = computed(() => sharedProject.value ? t('Удалить источник') : t('Удалить'))

const sortedStages = computed(() => [...props.project.stages].sort((left, right) => {
  if (sort.value === 'manual') {
    return manualStageIds.value.indexOf(left.id) - manualStageIds.value.indexOf(right.id)
  }
  if (sort.value === 'name') return left.name.localeCompare(right.name, locale.localeTag)
  if (sort.value === 'progress') return right.progress - left.progress
  if (sort.value === 'updated') return String(right.updated_at ?? '').localeCompare(String(left.updated_at ?? ''))
  return String(left.deadline ?? '9999-12-31').localeCompare(String(right.deadline ?? '9999-12-31'))
}))

function stageProgress(stage: Project): number {
  return Math.min(100, Math.max(0, stage.progress || 0))
}
function canComplete(stage: Project): boolean {
  return stage.status !== 'завершен' && !stage.infinite && stage.goal !== null && stage.total >= stage.goal
}
function showStageStreak(stage: Project): boolean {
  return props.streaksEnabled && props.project.deadline === null && stage.deadline !== null && stage.streak_enabled
}
function requestRemove(stage: Project): void {
  const confirmed = window.confirm(t(sharedProject.value
    ? 'Удалить источник «{name}» и всю его историю прогресса? Это действие нельзя отменить.'
    : 'Удалить этап «{name}» и всю его историю прогресса? Это действие нельзя отменить.', { name: stage.name }))
  if (confirmed) emit('remove', stage)
}
function startDrag(event: DragEvent, stage: Project): void {
  if (readOnly.value || sort.value !== 'manual' || !stageOrderEditing.value) { event.preventDefault(); return }
  draggedStageId.value = stage.id
  if (event.dataTransfer) {
    event.dataTransfer.effectAllowed = 'move'
    event.dataTransfer.setData(stageDragMimeType, stage.id)
    event.dataTransfer.setData('text/plain', stage.id)
  }
}
function dropStage(event: DragEvent, targetStage: Project): void {
  const sourceId = event.dataTransfer?.getData(stageDragMimeType)
    || event.dataTransfer?.getData('text/plain')
    || draggedStageId.value
  draggedStageId.value = null
  if (sourceId) reorderStage(sourceId, targetStage)
}
function reorderStage(sourceId: string, targetStage: Project): void {
  if (sourceId === targetStage.id || sort.value !== 'manual' || !stageOrderEditing.value) return
  const ids = sortedStages.value.map((stage) => stage.id)
  const from = ids.indexOf(sourceId)
  const to = ids.indexOf(targetStage.id)
  if (from < 0 || to < 0) return
  ids.splice(from, 1)
  const targetIndex = ids.indexOf(targetStage.id)
  ids.splice(from < to ? targetIndex + 1 : targetIndex, 0, sourceId)
  manualStageIds.value = ids
}
function beginStageOrderEditing(): void {
  manualStageIds.value = props.project.stages.map((stage) => stage.id)
  stageOrderEditing.value = true
}
function saveStageOrder(): void {
  if (!stageOrderEditing.value || props.busy) return
  stageOrderEditing.value = false
  emit('reorder', [...manualStageIds.value])
}
function clearStagePointerDrag(): void {
  pointerStageDrag.value = null
  window.removeEventListener('pointermove', moveStagePointer)
  window.removeEventListener('pointerup', finishStagePointer)
  window.removeEventListener('pointercancel', finishStagePointer)
}
function moveStagePointer(event: PointerEvent): void {
  const drag = pointerStageDrag.value
  if (!drag || event.pointerId !== drag.pointerId) return
  if (!drag.active && Math.hypot(event.clientX - drag.startX, event.clientY - drag.startY) < 8) return
  drag.active = true
  event.preventDefault()
}
function finishStagePointer(event: PointerEvent): void {
  const drag = pointerStageDrag.value
  if (!drag || event.pointerId !== drag.pointerId) return
  clearStagePointerDrag()
  if (!drag.active) return
  const targetId = document.elementFromPoint(event.clientX, event.clientY)
    ?.closest<HTMLElement>('[data-stage-id]')?.dataset.stageId
  const targetStage = props.project.stages.find((stage) => stage.id === targetId)
  if (!targetStage) return
  window.addEventListener('click', (clickEvent) => {
    clickEvent.preventDefault()
    clickEvent.stopImmediatePropagation()
  }, { capture: true, once: true })
  reorderStage(drag.stageId, targetStage)
}
function startStagePointer(event: PointerEvent, stage: Project): void {
  if (props.busy || readOnly.value || sort.value !== 'manual' || !stageOrderEditing.value || event.button !== 0) return
  pointerStageDrag.value = {
    stageId: stage.id, pointerId: event.pointerId, startX: event.clientX, startY: event.clientY, active: false,
  }
  window.addEventListener('pointermove', moveStagePointer, { passive: false })
  window.addEventListener('pointerup', finishStagePointer)
  window.addEventListener('pointercancel', finishStagePointer)
}
function openContext(event: MouseEvent, stage: Project): void {
  contextStage.value = stage
  contextPosition.value = { x: event.clientX, y: event.clientY }
}
const contextActions = computed<ContextAction[]>(() => {
  const stage = contextStage.value
  if (!stage) return []
  const actions: ContextAction[] = []
  if (!readOnly.value && !sharedProject.value && stage.status !== 'завершен') actions.push({ id: 'edit', label: t('Изменить') })
  if (canComplete(stage) && !sharedProject.value) actions.push({ id: 'complete', label: t('Завершить') })
  if (stage.work_method !== 'app' && stage.status !== 'завершен') actions.push({ id: 'sync', label: t('Синхронизировать') })
  if (!readOnly.value) actions.push({ id: 'delete', label: removeButtonLabel.value, danger: true, separator: true })
  return actions
})
function selectContextAction(action: ContextAction): void {
  const stage = contextStage.value
  contextStage.value = null
  if (!stage) return
  if (action.id === 'edit') emit('edit', stage)
  if (action.id === 'complete') emit('complete', stage)
  if (action.id === 'sync') emit('sync', stage)
  if (action.id === 'delete') requestRemove(stage)
}

function measureStageCardHeight(): void {
  const list = stagesSection.value?.querySelector<HTMLOListElement>('.stage-list')
  if (!list) return
  list.style.removeProperty('--stage-card-height')
  const heights = [...list.querySelectorAll<HTMLElement>('.stage-card')]
    .map((card) => card.getBoundingClientRect().height)
  const maxHeight = Math.max(0, ...heights)
  if (maxHeight > 0) list.style.setProperty('--stage-card-height', `${Math.ceil(maxHeight)}px`)
}

function scheduleStageCardMeasurement(): void {
  if (stageMeasureFrame !== undefined) window.cancelAnimationFrame(stageMeasureFrame)
  stageMeasureFrame = window.requestAnimationFrame(() => {
    stageMeasureFrame = undefined
    measureStageCardHeight()
  })
}

function observeStageCards(): void {
  const list = stagesSection.value?.querySelector<HTMLOListElement>('.stage-list')
  if (!list || !stageSizeObserver) return
  stageSizeObserver.disconnect()
  stageSizeObserver.observe(list)
  list.querySelectorAll<HTMLElement>('.stage-card').forEach((card) => stageSizeObserver?.observe(card))
  scheduleStageCardMeasurement()
}

watch(() => props.stageSort, (value) => { if (value !== sort.value) sort.value = value })
watch(() => props.project.stages.map((stage) => stage.id), (ids) => {
  if (!stageOrderEditing.value) manualStageIds.value = ids
})
watch(sort, (value) => {
  stageOrderEditing.value = false
  manualStageIds.value = props.project.stages.map((stage) => stage.id)
  emit('sort', value)
})
onMounted(() => {
  void nextTick(() => {
    if (typeof ResizeObserver === 'undefined') return
    stageSizeObserver = new ResizeObserver(() => scheduleStageCardMeasurement())
    observeStageCards()
  })
})
onUpdated(observeStageCards)
onBeforeUnmount(clearStagePointerDrag)
onBeforeUnmount(() => {
  if (stageMeasureFrame !== undefined) window.cancelAnimationFrame(stageMeasureFrame)
  stageSizeObserver?.disconnect()
  stageSizeObserver = undefined
})
</script>

<template>
  <section ref="stagesSection" class="stages-section" aria-labelledby="stages-heading">
    <div class="section-heading stage-section-heading">
      <div><p>{{ t('Структура рукописи') }}</p><h2 id="stages-heading">{{ t(sharedProject ? 'Источники' : 'Этапы') }}</h2></div>
      <div class="stage-heading-actions">
        <label class="stage-sort" for="stage-sort">
          <span class="visually-hidden">{{ t('Сортировка') }}</span>
          <select id="stage-sort" v-model="sort" :disabled="busy || stageOrderEditing">
            <option value="manual">{{ t('Свободный порядок') }}</option>
            <option value="progress">{{ t('По прогрессу') }}</option>
            <option value="updated">{{ t('Недавно изменённые') }}</option>
            <option value="deadline">{{ t('По сроку') }}</option>
            <option value="name">{{ t('По названию') }}</option>
          </select>
        </label>
        <button
          v-if="sort === 'manual' && !readOnly"
          class="stage-order-toggle"
          type="button"
          :disabled="busy"
          :aria-label="stageOrderEditing ? t('Сохранить') : t('Изменить')"
          @click="stageOrderEditing ? saveStageOrder() : beginStageOrderEditing()"
        >
          <span v-if="!stageOrderEditing" aria-hidden="true">✎</span>
          {{ stageOrderEditing ? t('Сохранить') : t('Изменить') }}
        </button>
        <button v-if="!readOnly" class="nf-button nf-button--secondary" type="button" :disabled="busy || stageOrderEditing" @click="emit('add')">
          <IonIcon :icon="addOutline" aria-hidden="true" />{{ addButtonLabel }}
        </button>
      </div>
    </div>

    <TransitionGroup
      v-if="project.stages.length"
      name="stage-list"
      move-class="stage-list-move"
      tag="ol"
      class="stage-list"
    >
      <li
        v-for="(stage, index) in sortedStages"
        :key="stage.id"
        class="stage-card"
        :class="{ 'stage-card--sortable': !busy && !readOnly && sort === 'manual' && stageOrderEditing }"
        :data-stage-id="stage.id"
        draggable="false"
        @dragstart="startDrag($event, stage)"
        @dragend="draggedStageId = null"
        @dragover.prevent
        @drop.prevent="dropStage($event, stage)"
        @contextmenu.prevent="openContext($event, stage)"
      >
        <button class="stage-open-button" type="button" :aria-label="`${t(sharedProject ? 'Источники' : 'Этапы')}: ${stage.name}`" @click="emit('open', stage)">
          <span class="stage-index" aria-hidden="true">{{ index + 1 }}</span>
          <div class="stage-title-row">
            <div>
              <h3>{{ stage.name }}</h3>
              <span v-if="stage.status === 'завершен'" class="stage-completed">{{ t('Завершён') }}</span>
              <p><AnimatedNumber :value="stage.total" :digits="fractionDigits" /> / <template v-if="stage.infinite || stage.goal === null">{{ t('Без лимита') }}</template><AnimatedNumber v-else :value="stage.goal" :digits="fractionDigits" /></p>
              <StreakBadge v-if="showStageStreak(stage)" class="stage-streak" :length="stage.streak_length" :status="stage.streak_status" scope="stage" compact />
            </div>
            <ProgressRing :value="sharedProject && stage.infinite ? 100 : stageProgress(stage)" :infinite="stage.infinite" :full="sharedProject && stage.infinite" :label="`${t(sharedProject ? 'Прогресс источника' : 'Прогресс этапа')} ${stage.name}`" />
          </div>
        </button>
        <div class="stage-actions">
          <button
            v-if="stageOrderEditing"
            class="stage-drag-handle"
            type="button"
            :aria-label="`${t('Свободный порядок')}: ${stage.name}`"
            :title="t('Свободный порядок')"
            @click.stop.prevent
            @contextmenu.stop
            @pointerdown.stop.prevent="startStagePointer($event, stage)"
          >
            <span aria-hidden="true">⠿</span>
          </button>
          <ProgressShareMenu :label="t('Поделиться прогрессом «{name}»', { name: stage.name })" :title="stage.infinite ? t('Для проекта без цели нельзя создать картинку прогресса') : undefined" :disabled="busy || sharing || sharedProject || stage.infinite" @copy="emit('copy', stage)" @save="emit('save', stage)" />
        </div>
      </li>
    </TransitionGroup>

    <div v-else class="stages-empty">
      <p>{{ sharedProject ? t('Подключите первый источник синхронизации.') : t('Разбейте рукопись на главы или другие рабочие этапы.') }}</p>
      <button v-if="!readOnly" class="nf-button nf-button--secondary" type="button" :disabled="busy" @click="emit('add')"><IonIcon :icon="addOutline" aria-hidden="true" />{{ emptyActionLabel }}</button>
    </div>

    <ContextActionMenu :open="contextStage !== null" :x="contextPosition.x" :y="contextPosition.y" :label="contextStage ? `${t(sharedProject ? 'Действия источника' : 'Действия этапа')}: ${contextStage.name}` : ''" :actions="contextActions" @close="contextStage = null" @select="selectContextAction" />
  </section>
</template>

<style scoped>
.stages-section { margin-top: var(--nf-space-7); }
.section-heading { display: flex; gap: var(--nf-space-4); align-items: flex-end; justify-content: space-between; margin-bottom: var(--nf-space-4); }
.section-heading p { margin: 0 0 var(--nf-space-1); color: var(--nf-color-accent); font-size: .72rem; font-weight: 800; letter-spacing: .08em; text-transform: uppercase; }
.section-heading h2 { margin: 0; font-family: var(--nf-font-serif); font-size: clamp(1.7rem, 4vw, 2.3rem); }
.stage-section-heading { align-items: center; }
.stage-heading-actions { display: flex; flex-wrap: wrap; gap: var(--nf-space-2); align-items: center; }
.stage-sort { display: inline-flex; min-height: 2.75rem; align-items: center; border: 1px solid var(--nf-color-border); border-radius: var(--nf-radius-sm); background: var(--nf-color-surface-raised); }
.stage-sort select { min-height: 2.65rem; padding: 0 2rem 0 .75rem; border: 0; background: transparent; color: var(--nf-color-text); font: inherit; font-size: .82rem; font-weight: 700; }
.stage-order-toggle,
.stage-drag-handle { display: inline-grid; min-height: 2.75rem; padding: 0 .8rem; place-items: center; border: 1px solid var(--nf-color-border); border-radius: var(--nf-radius-sm); background: var(--nf-color-surface-raised); color: var(--nf-color-primary); font: inherit; font-size: .8rem; font-weight: 750; cursor: pointer; }
.stage-order-toggle { grid-auto-flow: column; gap: var(--nf-space-1); }
.stage-drag-handle { min-width: 2.75rem; padding: 0; font-size: 1.2rem; cursor: grab; touch-action: none; user-select: none; }
.stage-drag-handle:active { cursor: grabbing; }
.stage-list { --stage-card-height: 0px; display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, 18rem), 1fr)); gap: var(--nf-space-3); margin: 0; padding: 0; list-style: none; }
.stage-card { display: grid; grid-template-rows: minmax(0, 1fr) auto; min-height: var(--stage-card-height); gap: var(--nf-space-3); padding: var(--nf-space-4); border: 1px solid var(--nf-color-border); border-radius: var(--nf-radius-md); background: var(--nf-color-surface); box-shadow: var(--nf-shadow-card); transition: border-color 160ms ease, transform 160ms ease, box-shadow 160ms ease; }
.stage-card:hover { border-color: color-mix(in srgb, var(--nf-color-primary) 45%, var(--nf-color-border)); transform: translateY(-2px); box-shadow: 0 18px 42px rgb(43 55 50 / 13%); }
.stage-card--sortable { cursor: grab; user-select: none; }
.stage-open-button { display: grid; grid-template-columns: auto minmax(0, 1fr); gap: var(--nf-space-3); align-self: stretch; width: 100%; min-width: 0; padding: 0; border: 0; background: transparent; color: inherit; text-align: left; cursor: pointer; }
.stage-open-button:focus-visible { border-radius: var(--nf-radius-sm); outline: 3px solid var(--nf-color-primary-soft); outline-offset: 3px; }
.stage-open-button:hover h3 { color: var(--nf-color-primary); }
.stage-index { display: grid; width: 2.5rem; height: 2.5rem; place-items: center; border-radius: 50%; background: var(--nf-color-primary-soft); color: var(--nf-color-primary); font-family: var(--nf-font-serif); font-weight: 800; }
.stage-title-row { display: flex; gap: var(--nf-space-3); align-items: flex-start; justify-content: space-between; }
.stage-title-row > div { min-width: 0; }
.stage-title-row h3 { overflow-wrap: anywhere; margin: 0; font-size: 1rem; }
.stage-title-row p { margin: var(--nf-space-2) 0 0; color: var(--nf-color-text-muted); font-size: .78rem; }
.stage-completed { display: inline-block; margin-top: var(--nf-space-1); color: var(--nf-color-success); font-size: .75rem; font-weight: 700; }
.stage-streak { margin-top: var(--nf-space-3); }
.stage-actions { display: flex; gap: var(--nf-space-2); align-items: center; justify-content: space-between; min-height: 2.75rem; margin-top: var(--nf-space-2); }
.stages-empty { display: grid; justify-items: start; gap: var(--nf-space-3); padding: var(--nf-space-5); border: 1px dashed var(--nf-color-border); border-radius: var(--nf-radius-md); color: var(--nf-color-text-muted); }
.stages-empty p { margin: 0; }
.stage-list-move, .stage-list-enter-active, .stage-list-leave-active { transition: transform 360ms ease, opacity 220ms ease; }
.stage-list-enter-from, .stage-list-leave-to { opacity: 0; transform: translateY(.75rem) scale(.98); }
@media (max-width: 37.5rem) { .section-heading { align-items: stretch; flex-direction: column; } }
</style>
