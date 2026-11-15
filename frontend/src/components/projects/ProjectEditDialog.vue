<script setup lang="ts">
import { computed, nextTick, reactive, ref, watch } from 'vue'
import { IonContent, IonFooter, IonHeader, IonIcon, IonModal, IonSpinner, IonToolbar } from '@ionic/vue'
import { closeOutline } from 'ionicons/icons'

import { useLocaleStore } from '@/stores/locale'
import ProjectCoverEditor from '@/components/projects/ProjectCoverEditor.vue'
import type { Project, ProjectUpdate, UnitCode, WorkMethod } from '@/types/api'
import {
  automaticDailyGoal,
  automaticDeadlineAfterGoalChange,
  automaticEditedDeadline,
  convertProjectUnit,
  planningDate,
  todayIsoDate,
} from '@/utils/projectPlanning'

const props = withDefaults(
  defineProps<{
    open: boolean
    project: Project
    submitting?: boolean
    apiError?: string | null
    globalStreakEnabled?: boolean
    gameModeEnabled?: boolean
  }>(),
  {
    submitting: false,
    apiError: null,
    globalStreakEnabled: false,
    gameModeEnabled: false,
  },
)

const emit = defineEmits<{
  close: []
  submit: [payload: ProjectUpdate]
}>()

const locale = useLocaleStore()
const t = locale.translate
const validationErrors = ref<string[]>([])
const errorSummary = ref<HTMLElement | null>(null)
const sourceUnit = ref<UnitCode>('symbols')
const planningUpdateInProgress = ref(false)
const minimumDeadline = computed(() => props.project.planning_date ?? todayIsoDate())
const calculationDate = computed(() => planningDate(props.project.planning_date))
const form = reactive({
  name: '',
  unit: 'symbols' as UnitCode,
  goal: '',
  total: '',
  deadline: '',
  noDeadline: false,
  personalGoal: '',
  infinite: false,
  stagesEnabled: false,
  streakEnabled: false,
  autoFreeze: true,
  combineStageMindmaps: false,
  recalculatePlan: false,
  coverImage: null as string | null,
  workMethod: 'manual' as WorkMethod,
})

const canRecalculatePlan = computed(() => {
  const personalGoal = numberFrom(form.personalGoal)
  return Boolean(form.deadline)
    && Number.isFinite(personalGoal)
    && personalGoal > 0
    && Math.abs(
      personalGoal - convertProjectUnit(props.project.personal_goal, props.project.unit, form.unit),
    ) < 0.000001
})

function fill(): void {
  const project = props.project
  form.name = project.name
  form.unit = project.unit
  sourceUnit.value = project.unit
  form.goal = project.goal === null ? '' : String(project.goal)
  form.total = String(project.total)
  form.deadline = project.deadline?.slice(0, 10) ?? ''
  form.noDeadline = !form.deadline
  // The legacy dialog treats a project without a deadline as having no daily plan.
  // Keep the form state in that same valid state when reopening such a project.
  form.personalGoal = form.noDeadline ? '0' : String(project.personal_goal)
  form.infinite = project.infinite
  form.stagesEnabled = project.stages_enabled
  form.streakEnabled = project.streak_enabled ?? project.streak_status !== 'Off'
  form.autoFreeze = project.auto_freeze ?? true
  form.combineStageMindmaps = project.combine_stage_mindmaps
  form.recalculatePlan = false
  form.coverImage = project.cover_image
  form.workMethod = project.work_method
  validationErrors.value = []
}

function numberFrom(value: string | number): number {
  return Number(String(value).replace(',', '.'))
}

function updateDailyGoal(): void {
  if (planningUpdateInProgress.value || form.infinite || form.noDeadline) return
  planningUpdateInProgress.value = true
  try {
  const addedToday = convertProjectUnit(
    props.project.added_today, props.project.unit, form.unit,
  )
  const dailyGoal = automaticDailyGoal(
    numberFrom(form.goal), numberFrom(form.total), form.deadline, form.unit,
    calculationDate.value, addedToday,
  )
  if (dailyGoal !== null) form.personalGoal = String(dailyGoal)
  } finally {
    planningUpdateInProgress.value = false
  }
}

function planValue(value: number | null): number | null {
  return value === null ? null : convertProjectUnit(value, props.project.unit, form.unit)
}

function updateDeadline(): void {
  if (planningUpdateInProgress.value || form.infinite || form.noDeadline) return
  planningUpdateInProgress.value = true
  try {
  const deadline = automaticEditedDeadline({
    goal: numberFrom(form.goal),
    total: numberFrom(form.total),
    dailyGoal: numberFrom(form.personalGoal),
    todayGoal: planValue(props.project.today_goal),
    previousDailyGoal: planValue(props.project.plan_daily_goal),
    recalculate: form.recalculatePlan,
    today: calculationDate.value,
  })
  if (deadline !== null) form.deadline = deadline
  } finally {
    planningUpdateInProgress.value = false
  }
}

function updatePlanFromTotal(): void {
  if (numberFrom(form.personalGoal) > 0) updateDeadline()
  else updateDailyGoal()
}

function updatePlanFromGoal(): void {
  const personalGoal = numberFrom(form.personalGoal)
  if (personalGoal <= 0) {
    updateDailyGoal()
    return
  }
  const deadline = automaticDeadlineAfterGoalChange({
    goal: numberFrom(form.goal),
    total: numberFrom(form.total),
    dailyGoal: personalGoal,
    todayGoal: planValue(props.project.today_goal),
    recalculate: form.recalculatePlan,
    today: calculationDate.value,
  })
  if (deadline !== null) form.deadline = deadline
}

function updateUnit(): void {
  const oldUnit = sourceUnit.value
  const newUnit = form.unit
  if (oldUnit === newUnit) return
  planningUpdateInProgress.value = true
  try {
    form.goal = String(convertProjectUnit(numberFrom(form.goal), oldUnit, newUnit))
    form.total = String(convertProjectUnit(numberFrom(form.total), oldUnit, newUnit))
    form.personalGoal = String(convertProjectUnit(numberFrom(form.personalGoal), oldUnit, newUnit))
    sourceUnit.value = newUnit
  } finally {
    planningUpdateInProgress.value = false
  }
  if (form.deadline) updateDailyGoal()
}

function toggleInfinite(): void {
  if (form.infinite) {
    form.noDeadline = true
    form.personalGoal = '0'
    form.deadline = ''
  } else {
    updatePlanFromGoal()
  }
}

function toggleNoDeadline(): void {
  if (form.noDeadline) {
    form.recalculatePlan = false
    form.deadline = ''
    form.personalGoal = '0'
    return
  }
  form.deadline = minimumDeadline.value
  updateDailyGoal()
}

async function submit(): Promise<void> {
  const errors: string[] = []
  const name = form.name.trim()
  const goal = numberFrom(form.goal)
  const total = numberFrom(form.total)
  const personalGoal = numberFrom(form.personalGoal)
  if (!name) errors.push(t('Введите название проекта.'))
  if (!props.project.stages.length && !form.infinite && (!Number.isFinite(goal) || goal <= 0)) {
    errors.push(t('Цель должна быть больше нуля.'))
  }
  if (!props.project.stages.length && (!Number.isFinite(total) || total < 0)) {
    errors.push(t('Текущее значение не может быть отрицательным.'))
  }
  if (!Number.isFinite(personalGoal) || personalGoal < 0) {
    errors.push(t('Цель на день не может быть отрицательной.'))
  }
  validationErrors.value = errors
  if (errors.length) {
    await nextTick()
    errorSummary.value?.focus()
    return
  }

  if (
    props.project.stages_enabled
    && !form.stagesEnabled
    && !window.confirm(t(
      'Все записи этапов будут перенесены в проект в хронологическом порядке. Цели и прогресс этапов сложатся и будут пересчитаны как записи одного проекта. Карты этапов не объединяются с картой проекта и будут удалены.',
    ))
  ) return
  if (
    form.unit !== props.project.unit
    && !window.confirm(t(
      'Изменение типа отслеживаемого значения приведет к необратимой конвертации текущей цели, прогресса и записей в новый тип (с округлением в большую сторону).\nПродолжить?',
    ))
  ) return

  const dailyGoalIncreased = personalGoal > props.project.personal_goal + 0.000001
  if (
    dailyGoalIncreased
    && !window.confirm(t(
      'Вы хотите увеличить цель на день, уменьшить ее не выйдет, пока она не будет выполнена.\nПродолжить?',
    ))
  ) return

  const payload: ProjectUpdate = {}
  if (name !== props.project.name) payload.name = name
  if (form.unit !== props.project.unit) payload.unit = form.unit
  const deadline = form.noDeadline ? null : (form.deadline || null)
  if (deadline !== props.project.deadline?.slice(0, 10) && !(deadline === null && props.project.deadline === null)) {
    payload.deadline = deadline
  }
  if (Math.abs(personalGoal - props.project.personal_goal) >= 0.000001) {
    payload.personal_goal = personalGoal
    if (dailyGoalIncreased) payload.confirm_daily_goal_increase = true
  }
  if (form.stagesEnabled !== props.project.stages_enabled) payload.stages_enabled = form.stagesEnabled
  if (form.coverImage !== props.project.cover_image) payload.cover_image = form.coverImage
  if (form.workMethod !== props.project.work_method) payload.work_method = form.workMethod
  if (form.streakEnabled !== props.project.streak_enabled) payload.streak_enabled = form.streakEnabled
  if (form.autoFreeze !== (props.project.auto_freeze ?? true)) payload.auto_freeze = form.autoFreeze
  if (
    form.stagesEnabled
    && form.combineStageMindmaps !== props.project.combine_stage_mindmaps
  ) payload.combine_stage_mindmaps = form.combineStageMindmaps
  if (!props.project.stages.length) {
    if (form.infinite !== props.project.infinite) payload.infinite = form.infinite
    if (!form.infinite && (props.project.goal === null || Math.abs(goal - props.project.goal) >= 0.000001)) {
      payload.goal = goal
    }
    if (Math.abs(total - props.project.total) >= 0.000001) payload.total = total
  }
  if (form.recalculatePlan && canRecalculatePlan.value) payload.recalculate_plan = true
  emit('submit', payload)
}

function requestClose(): void {
  if (!props.submitting) emit('close')
}

watch(
  () => props.open,
  (open) => {
    if (open) fill()
  },
  { immediate: true },
)

watch(() => form.deadline, (deadline) => {
  if (deadline) form.noDeadline = false
  updateDailyGoal()
}, { flush: 'sync' })
watch(() => form.personalGoal, updateDeadline, { flush: 'sync' })
watch(() => form.goal, updatePlanFromGoal, { flush: 'sync' })
watch(() => form.total, updatePlanFromTotal, { flush: 'sync' })
watch(() => form.recalculatePlan, updateDeadline, { flush: 'sync' })
</script>

<template>
  <IonModal
    :is-open="open"
    css-class="workspace-dialog-modal"
    :backdrop-dismiss="!submitting"
    :keyboard-close="!submitting"
    @did-dismiss="requestClose"
  >
    <IonHeader class="workspace-dialog-header ion-no-border">
      <div>
        <p>{{ t('Параметры проекта') }}</p>
        <h2>{{ t('Редактировать проект') }}</h2>
      </div>
      <button
        class="workspace-dialog-close"
        type="button"
        :aria-label="t('Закрыть')"
        :disabled="submitting"
        @click="requestClose"
      >
        <IonIcon :icon="closeOutline" aria-hidden="true" />
      </button>
    </IonHeader>

    <IonContent class="workspace-dialog-content">
      <form id="project-edit-form" class="workspace-form" novalidate @submit.prevent="submit">
        <div
          v-if="validationErrors.length"
          ref="errorSummary"
          class="workspace-form-error"
          role="alert"
          tabindex="-1"
        >
          <strong>{{ t('Проверьте форму') }}</strong>
          <ul><li v-for="error in validationErrors" :key="error">{{ error }}</li></ul>
        </div>
        <div v-if="apiError" class="workspace-form-error" role="alert">
          <strong>{{ t('Не удалось сохранить проект') }}</strong>
          <p>{{ apiError }}</p>
        </div>

        <label class="workspace-field workspace-field--wide" for="edit-project-name">
          <span>{{ t('Название') }}</span>
          <input id="edit-project-name" v-model="form.name" maxlength="300" autocomplete="off" />
        </label>
        <ProjectCoverEditor
          v-model="form.coverImage"
          workspace-wide
          :disabled="submitting"
        />
        <label class="workspace-field" for="edit-project-unit">
          <span>{{ t('Единица прогресса') }}</span>
          <select id="edit-project-unit" v-model="form.unit" @change="updateUnit">
            <option value="symbols">{{ t('Символы') }}</option>
            <option value="A4">{{ t('Листы A4') }}</option>
            <option value="author_list">{{ t('Авторские листы') }}</option>
            <option value="ficbook_pages">{{ t('Страницы Ficbook') }}</option>
          </select>
        </label>
        <div class="workspace-field">
          <label class="workspace-field__inner" for="edit-project-deadline">
            <span>{{ t('Срок') }}</span>
            <input id="edit-project-deadline" v-model="form.deadline" type="date" :min="minimumDeadline" :disabled="form.infinite || form.noDeadline" @input="updateDailyGoal" @change="updateDailyGoal" />
          </label>
          <label class="workspace-check workspace-check--compact">
            <input v-model="form.noDeadline" name="no_deadline" type="checkbox" :disabled="form.infinite" @change="toggleNoDeadline" />
            <span>{{ t('Нет дедлайна') }}</span>
          </label>
        </div>
        <label v-if="!project.stages.length" class="workspace-field" for="edit-project-goal">
          <span>{{ t('Общая цель') }}</span>
          <input
            id="edit-project-goal"
            v-model="form.goal"
            type="number"
            min="0"
            step="any"
            inputmode="decimal"
            :disabled="form.infinite"
            @input="updatePlanFromGoal"
            @change="updatePlanFromGoal"
          />
        </label>
        <label v-if="!project.stages.length" class="workspace-field" for="edit-project-total">
          <span>{{ t('Текущее значение') }}</span>
          <input
            id="edit-project-total"
            v-model="form.total"
            type="number"
            min="0"
            step="any"
            inputmode="decimal"
            @input="updatePlanFromTotal"
            @change="updatePlanFromTotal"
          />
        </label>
        <label class="workspace-field" :class="{ 'workspace-field--wide': project.stages.length }" for="edit-project-personal-goal">
          <span>{{ t('Цель на день') }}</span>
          <input
            id="edit-project-personal-goal"
            v-model="form.personalGoal"
            type="number"
            min="0"
            step="any"
            inputmode="decimal"
            :disabled="form.infinite || form.noDeadline"
            @input="updateDeadline"
            @change="updateDeadline"
          />
        </label>
        <label class="workspace-field workspace-field--wide" for="edit-project-work-method">
          <span>{{ t('Метод работы с проектом') }}</span>
          <select id="edit-project-work-method" v-model="form.workMethod">
            <option value="manual">{{ t('Ручное добавление записей') }}</option>
            <option value="sync">{{ t('Синхронизация') }}</option>
            <option value="app">{{ t('В приложении') }}</option>
          </select>
          <small>{{ form.workMethod === 'manual' ? t('Записи добавляются вручную. Текст и синхронизация предложат сменить метод.') : form.workMethod === 'sync' ? t('Прогресс обновляется только из подключённого внешнего файла.') : t('Прогресс добавляется только по тексту во встроенном редакторе.') }}</small>
        </label>

        <div class="workspace-options workspace-field--wide">
          <label v-if="!project.stages.length" class="workspace-check">
            <input v-model="form.infinite" type="checkbox" @change="toggleInfinite" />
            <span><strong>{{ t('Проект без конечной цели') }}</strong></span>
          </label>
          <label class="workspace-check">
            <input v-model="form.stagesEnabled" type="checkbox" />
            <span><strong>{{ t('Проект с этапами') }}</strong></span>
          </label>
          <label v-if="globalStreakEnabled" class="workspace-check">
            <input id="edit-project-streak-enabled" v-model="form.streakEnabled" name="streak_enabled" type="checkbox" />
            <span><strong>{{ t('Отслеживать серию') }}</strong></span>
          </label>
          <label
            v-if="globalStreakEnabled && gameModeEnabled && form.streakEnabled"
            class="workspace-check"
          >
            <input id="edit-project-auto-freeze" v-model="form.autoFreeze" name="auto_freeze" type="checkbox" />
            <span><strong>{{ t('Использовать заморозку автоматически') }}</strong></span>
          </label>
          <label v-if="form.stagesEnabled" class="workspace-check">
            <input v-model="form.combineStageMindmaps" type="checkbox" />
            <span>
              <strong>{{ t('Объединять карты этапов') }}</strong>
              <small>{{ t('Показывать карты этапов как единую карту проекта') }}</small>
            </span>
          </label>
          <label v-if="canRecalculatePlan" class="workspace-check">
            <input v-model="form.recalculatePlan" type="checkbox" @change="updateDeadline" />
            <span><strong>{{ t('Пересчитать цели на день с сегодняшнего дня') }}</strong></span>
          </label>
        </div>

      </form>
    </IonContent>
    <IonFooter class="workspace-dialog-footer ion-no-border">
      <IonToolbar>
        <div class="workspace-form-actions">
          <button class="nf-button nf-button--secondary" type="button" :disabled="submitting" @click="requestClose">
            {{ t('Отмена') }}
          </button>
          <button class="nf-button" type="submit" form="project-edit-form" :disabled="submitting">
            <IonSpinner v-if="submitting" name="crescent" aria-hidden="true" />
            {{ submitting ? t('Сохраняем…') : t('Сохранить') }}
          </button>
        </div>
      </IonToolbar>
    </IonFooter>
  </IonModal>
</template>

<style>
.workspace-dialog-modal {
  --width: min(44rem, calc(100vw - 2rem));
  --height: min(54rem, calc(100dvh - 2rem));
  --border-radius: var(--nf-radius-lg);
  --background: var(--nf-color-surface);
}

.workspace-dialog-modal::part(content) {
  border: 1px solid var(--nf-color-border);
  box-shadow: 0 28px 80px rgb(20 30 27 / 25%);
}

.workspace-dialog-header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  padding: var(--nf-space-5) var(--nf-space-5) var(--nf-space-4);
  background: var(--nf-color-surface);
}

.workspace-dialog-header p {
  margin: 0 0 var(--nf-space-1);
  color: var(--nf-color-primary);
  font-size: 0.75rem;
  font-weight: 800;
  letter-spacing: 0.1em;
  text-transform: uppercase;
}

.workspace-dialog-header h2 {
  margin: 0;
  color: var(--nf-color-text);
  font-family: var(--nf-font-serif);
  font-size: clamp(1.55rem, 5vw, 2rem);
}

.workspace-dialog-close {
  display: grid;
  width: 2.75rem;
  height: 2.75rem;
  padding: 0;
  place-items: center;
  border: 0;
  border-radius: var(--nf-radius-pill);
  background: var(--nf-color-surface-muted);
  color: var(--nf-color-text);
  cursor: pointer;
}

.workspace-dialog-content {
  --background: var(--nf-color-surface);
  --padding-top: var(--nf-space-2);
  --padding-start: var(--nf-space-5);
  --padding-end: var(--nf-space-5);
  --padding-bottom: var(--nf-space-5);
}

.workspace-form {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: var(--nf-space-4);
  align-items: start;
  padding-bottom: var(--nf-space-5);
}

.workspace-field {
  display: grid;
  gap: var(--nf-space-2);
  min-width: 0;
  color: var(--nf-color-text);
  font-size: 0.9rem;
  font-weight: 700;
}

.workspace-field__inner { display: grid; gap: var(--nf-space-2); }

.workspace-field--wide,
.workspace-form-error {
  grid-column: 1 / -1;
}

.workspace-field input,
.workspace-field select {
  width: 100%;
  height: 3rem;
  min-height: 3rem;
  padding: 0.65rem 0.8rem;
  border: 1px solid var(--nf-color-border);
  border-radius: var(--nf-radius-sm);
  background: var(--nf-color-surface-raised);
  color: var(--nf-color-text);
}

.workspace-field input:disabled {
  background: var(--nf-color-surface-muted);
  opacity: 0.65;
}

.workspace-form-error {
  padding: var(--nf-space-4);
  border-left: 0.3rem solid var(--nf-color-danger);
  border-radius: var(--nf-radius-sm);
  background: color-mix(in srgb, var(--nf-color-danger) 10%, var(--nf-color-surface));
  color: var(--nf-color-text);
}

.workspace-form-error p,
.workspace-form-error ul { margin: var(--nf-space-2) 0 0; }

.workspace-options { display: grid; gap: var(--nf-space-2); }

.workspace-check {
  display: grid;
  grid-template-columns: 1.4rem 1fr;
  gap: var(--nf-space-3);
  align-items: start;
  min-height: 3rem;
  padding: var(--nf-space-3);
  border: 1px solid var(--nf-color-border);
  border-radius: var(--nf-radius-sm);
  background: var(--nf-color-surface-raised);
  cursor: pointer;
}

.workspace-check input {
  width: 1.25rem;
  height: 1.25rem;
  margin: 0.15rem 0 0;
  accent-color: var(--nf-color-primary);
}

.workspace-check strong,
.workspace-check small { display: block; }
.workspace-check small { margin-top: var(--nf-space-1); color: var(--nf-color-text-muted); }

.workspace-check--compact {
  grid-template-columns: 1.25rem 1fr;
  min-height: auto;
  padding: 0;
  border: 0;
  background: transparent;
  font-size: 0.8rem;
}

.workspace-field .workspace-check--compact input {
  width: 1.1rem;
  min-height: 0;
  height: 1.1rem;
  margin: 0.05rem 0 0;
  padding: 0;
  border: 0;
  border-radius: 0;
  background: transparent;
}

.workspace-dialog-footer ion-toolbar {
  --background: var(--nf-color-surface);
  --border-color: var(--nf-color-border);
  --padding-bottom: env(safe-area-inset-bottom);
  --padding-end: var(--nf-space-5);
  --padding-start: var(--nf-space-5);
  --padding-top: var(--nf-space-3);
  border-top: 1px solid var(--nf-color-border);
}

.workspace-form-actions {
  display: flex;
  gap: var(--nf-space-3);
  justify-content: flex-end;
  padding-bottom: var(--nf-space-3);
}

@media (max-width: 37.5rem) {
  .workspace-dialog-modal { --width: 100%; --height: 100%; --border-radius: 0; }
  .workspace-form { grid-template-columns: 1fr; }
  .workspace-field--wide,
  .workspace-form-error { grid-column: auto; }
  .workspace-dialog-footer ion-toolbar { --padding-end: var(--nf-space-4); --padding-start: var(--nf-space-4); }
  .workspace-form-actions .nf-button { flex: 1; }
}
</style>
