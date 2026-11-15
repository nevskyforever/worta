<script setup lang="ts">
import { computed, nextTick, reactive, ref, watch } from 'vue'
import { IonContent, IonFooter, IonHeader, IonIcon, IonModal, IonSpinner, IonToolbar } from '@ionic/vue'
import { closeOutline } from 'ionicons/icons'

import { useLocaleStore } from '@/stores/locale'
import type { EntityUpdate, Project, StageCreate, UnitCode, WorkMethod } from '@/types/api'
import {
  automaticDailyGoal,
  automaticDeadline,
  automaticDeadlineAfterGoalChange,
  automaticEditedDeadline,
  planningDate as parsePlanningDate,
  todayIsoDate,
} from '@/utils/projectPlanning'

const props = withDefaults(
  defineProps<{
    open: boolean
    projectUnit: UnitCode
    planningDate?: string
    stage?: Project | null
    sharedSource?: boolean
    defaultName?: string
    submitting?: boolean
    apiError?: string | null
    globalStreakEnabled?: boolean
    gameModeEnabled?: boolean
  }>(),
  {
    stage: null,
    planningDate: undefined,
    sharedSource: false,
    defaultName: '',
    submitting: false,
    apiError: null,
    globalStreakEnabled: false,
    gameModeEnabled: false,
  },
)

const emit = defineEmits<{
  close: []
  submit: [payload: StageCreate | EntityUpdate]
}>()

const locale = useLocaleStore()
const t = locale.translate
const validationErrors = ref<string[]>([])
const errorSummary = ref<HTMLElement | null>(null)
const planningUpdateInProgress = ref(false)
const effectivePlanningDate = computed(() => props.stage?.planning_date ?? props.planningDate)
const minimumDeadline = computed(() => effectivePlanningDate.value ?? todayIsoDate())
const calculationDate = computed(() => parsePlanningDate(effectivePlanningDate.value))
const projectUnitLabel = computed(() => t({
  symbols: 'символы',
  A4: 'листы A4',
  author_list: 'авторские листы',
  ficbook_pages: 'страницы Ficbook',
}[props.projectUnit]))
const form = reactive({
  name: '',
  goal: '10000',
  total: '0',
  deadline: '',
  noDeadline: false,
  personalGoal: '0',
  infinite: false,
  streakEnabled: true,
  autoFreeze: true,
  recalculatePlan: false,
  workMethod: 'manual' as WorkMethod,
})

const canRecalculatePlan = computed(() => {
  const personalGoal = numberFrom(form.personalGoal)
  return Boolean(props.stage && form.deadline)
    && Number.isFinite(personalGoal)
    && personalGoal > 0
    && Math.abs(personalGoal - (props.stage?.personal_goal ?? 0)) < 0.000001
})

function fill(): void {
  form.name = props.stage?.name ?? props.defaultName
  form.goal = props.stage?.goal === null ? '10000' : String(props.stage?.goal ?? 10000)
  form.total = String(props.sharedSource ? 0 : (props.stage?.total ?? 0))
  form.deadline = props.stage?.deadline?.slice(0, 10) ?? ''
  form.noDeadline = !form.deadline
  // Stages without a deadline cannot have a personal daily plan, as in legacy.
  form.personalGoal = form.noDeadline ? '0' : String(props.stage?.personal_goal ?? 0)
  form.infinite = props.sharedSource || (props.stage?.infinite ?? false)
  form.streakEnabled = props.stage?.streak_enabled
    ?? (props.stage ? props.stage.streak_status !== 'Off' : true)
  form.autoFreeze = props.stage?.auto_freeze ?? true
  form.recalculatePlan = false
  form.workMethod = props.sharedSource ? 'sync' : (props.stage?.work_method ?? 'manual')
  validationErrors.value = []
}

function numberFrom(value: string | number): number {
  return Number(String(value).replace(',', '.'))
}

function updateDailyGoal(): void {
  if (planningUpdateInProgress.value || form.infinite || form.noDeadline || props.sharedSource) return
  planningUpdateInProgress.value = true
  try {
  const dailyGoal = automaticDailyGoal(
    numberFrom(form.goal), numberFrom(form.total), form.deadline, props.projectUnit,
    calculationDate.value, props.stage?.added_today ?? 0,
  )
  if (dailyGoal !== null) form.personalGoal = String(dailyGoal)
  } finally {
    planningUpdateInProgress.value = false
  }
}

function updateDeadline(): void {
  if (planningUpdateInProgress.value || form.infinite || form.noDeadline || props.sharedSource) return
  planningUpdateInProgress.value = true
  try {
  const deadline = props.stage
    ? automaticEditedDeadline({
      goal: numberFrom(form.goal),
      total: numberFrom(form.total),
      dailyGoal: numberFrom(form.personalGoal),
      todayGoal: props.stage.today_goal,
      previousDailyGoal: props.stage.plan_daily_goal,
      recalculate: form.recalculatePlan,
      today: calculationDate.value,
    })
    : automaticDeadline(
      numberFrom(form.goal), numberFrom(form.total), numberFrom(form.personalGoal),
      calculationDate.value,
  )
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
  const dailyGoal = numberFrom(form.personalGoal)
  if (dailyGoal <= 0) {
    updateDailyGoal()
    return
  }
  const deadline = props.stage
    ? automaticDeadlineAfterGoalChange({
      goal: numberFrom(form.goal),
      total: numberFrom(form.total),
      dailyGoal,
      todayGoal: props.stage.today_goal,
      recalculate: form.recalculatePlan,
      today: calculationDate.value,
    })
    : automaticDeadline(
      numberFrom(form.goal), numberFrom(form.total), dailyGoal, calculationDate.value,
    )
  if (deadline !== null) form.deadline = deadline
}

function toggleInfinite(): void {
  if (form.infinite) {
    form.noDeadline = true
    form.deadline = ''
    form.personalGoal = '0'
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
  if (!name) errors.push(t(props.sharedSource ? 'Введите название источника.' : 'Введите название этапа.'))
  if (!props.sharedSource && !form.infinite && (!Number.isFinite(goal) || goal <= 0)) {
    errors.push(t('Цель должна быть больше нуля.'))
  }
  if (!props.sharedSource && (!Number.isFinite(total) || total < 0)) {
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

  let payload: StageCreate | EntityUpdate
  if (props.stage) {
    const update: EntityUpdate = {}
    if (name !== props.stage.name) update.name = name
    if (form.infinite !== props.stage.infinite) update.infinite = form.infinite
    if (!form.infinite && (props.stage.goal === null || Math.abs(goal - props.stage.goal) >= 0.000001)) {
      update.goal = goal
    }
    if (Math.abs(total - props.stage.total) >= 0.000001) update.total = total
    const deadline = form.noDeadline ? null : (form.deadline || null)
    if (deadline !== props.stage.deadline?.slice(0, 10) && !(deadline === null && props.stage.deadline === null)) {
      update.deadline = deadline
    }
    if (Math.abs(personalGoal - props.stage.personal_goal) >= 0.000001) {
      if (
        personalGoal > props.stage.personal_goal + 0.000001
        && !window.confirm(t(
          'Вы хотите увеличить цель на день, уменьшить ее не выйдет, пока она не будет выполнена.\nПродолжить?',
        ))
      ) return
      update.personal_goal = personalGoal
      if (personalGoal > props.stage.personal_goal + 0.000001) {
        update.confirm_daily_goal_increase = true
      }
    }
    if (form.recalculatePlan && canRecalculatePlan.value) update.recalculate_plan = true
    if (form.workMethod !== props.stage.work_method) update.work_method = form.workMethod
    if (form.streakEnabled !== props.stage.streak_enabled) update.streak_enabled = form.streakEnabled
    if (form.autoFreeze !== (props.stage.auto_freeze ?? true)) update.auto_freeze = form.autoFreeze
    payload = update
  } else {
    payload = {
      name,
      infinite: props.sharedSource || form.infinite,
      deadline: props.sharedSource || form.noDeadline ? null : (form.deadline || null),
      personal_goal: props.sharedSource ? 0 : personalGoal,
      total: props.sharedSource ? 0 : total,
      streak_enabled: props.sharedSource ? false : form.streakEnabled,
      auto_freeze: props.sharedSource ? true : form.autoFreeze,
      work_method: form.workMethod,
      ...((props.sharedSource || form.infinite) ? {} : { goal }),
    }
  }
  emit('submit', payload)
}

function requestClose(): void {
  if (!props.submitting) emit('close')
}

watch(
  [() => props.open, () => props.stage?.id],
  ([open]) => {
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
    css-class="workspace-dialog-modal workspace-stage-modal"
    :backdrop-dismiss="!submitting"
    :keyboard-close="!submitting"
    @did-dismiss="requestClose"
  >
    <IonHeader class="workspace-dialog-header ion-no-border">
      <div>
        <p>{{ sharedSource ? t('Источники синхронизации') : t('Структура рукописи') }}</p>
        <h2>{{ stage ? t(sharedSource ? 'Редактировать источник' : 'Редактировать этап') : t(sharedSource ? 'Добавить источник' : 'Добавить этап') }}</h2>
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
      <form id="stage-form" class="workspace-form" novalidate @submit.prevent="submit">
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
          <strong>{{ t(sharedSource ? 'Не удалось сохранить источник' : 'Не удалось сохранить этап') }}</strong>
          <p>{{ apiError }}</p>
        </div>

        <label class="workspace-field workspace-field--wide" for="stage-name">
          <span>{{ sharedSource ? t('Название источника синхронизации:') : t('Название') }}</span>
          <input id="stage-name" v-model="form.name" maxlength="300" autocomplete="off" />
        </label>
        <label v-if="!sharedSource" class="workspace-field" for="stage-goal">
          <span>{{ t('Цель') }}</span>
          <input
            id="stage-goal"
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
        <label v-if="!sharedSource" class="workspace-field" for="stage-total">
          <span>{{ t('Текущее значение') }}</span>
          <input
            id="stage-total"
            v-model="form.total"
            type="number"
            min="0"
            step="any"
            inputmode="decimal"
            @input="updatePlanFromTotal"
            @change="updatePlanFromTotal"
          />
        </label>
        <div v-if="!sharedSource" class="workspace-field">
          <label for="stage-deadline">
            <span>{{ t('Срок') }}</span>
            <input id="stage-deadline" v-model="form.deadline" type="date" :min="minimumDeadline" :disabled="form.infinite || form.noDeadline" @input="updateDailyGoal" @change="updateDailyGoal" />
          </label>
          <label class="workspace-check workspace-check--compact">
            <input v-model="form.noDeadline" name="no_deadline" type="checkbox" :disabled="form.infinite" @change="toggleNoDeadline" />
            <span>{{ t('Нет дедлайна') }}</span>
          </label>
        </div>
        <label v-if="!sharedSource" class="workspace-field" for="stage-personal-goal">
          <span>{{ t('Цель на день') }}</span>
          <input
            id="stage-personal-goal"
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
        <p class="stage-unit-note">
          {{ t(sharedSource ? 'Единица источника совпадает с проектом' : 'Единица этапа совпадает с проектом') }}: <strong>{{ projectUnitLabel }}</strong>
        </p>
        <label v-if="!sharedSource" class="workspace-field workspace-field--wide" for="stage-work-method">
          <span>{{ t('Метод работы с проектом') }}</span>
          <select id="stage-work-method" v-model="form.workMethod">
            <option value="manual">{{ t('Ручное добавление записей') }}</option>
            <option value="sync">{{ t('Синхронизация') }}</option>
            <option value="app">{{ t('В приложении') }}</option>
          </select>
          <small>{{ form.workMethod === 'manual' ? t('Записи добавляются вручную. Текст и синхронизация предложат сменить метод.') : form.workMethod === 'sync' ? t('Прогресс обновляется только из подключённого внешнего файла.') : t('Прогресс добавляется только по тексту во встроенном редакторе.') }}</small>
        </label>

        <div v-if="!sharedSource" class="workspace-options workspace-field--wide">
          <label class="workspace-check">
            <input v-model="form.infinite" type="checkbox" @change="toggleInfinite" />
            <span><strong>{{ t('Этап без конечной цели') }}</strong></span>
          </label>
          <label v-if="globalStreakEnabled" class="workspace-check">
            <input id="stage-streak-enabled" v-model="form.streakEnabled" name="streak_enabled" type="checkbox" />
            <span><strong>{{ t('Отслеживать серию') }}</strong></span>
          </label>
          <label
            v-if="globalStreakEnabled && gameModeEnabled && form.streakEnabled"
            class="workspace-check"
          >
            <input id="stage-auto-freeze" v-model="form.autoFreeze" name="auto_freeze" type="checkbox" />
            <span><strong>{{ t('Использовать заморозку автоматически') }}</strong></span>
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
          <button class="nf-button" type="submit" form="stage-form" :disabled="submitting">
            <IonSpinner v-if="submitting" name="crescent" aria-hidden="true" />
            {{ submitting ? t('Сохраняем…') : t('Сохранить') }}
          </button>
        </div>
      </IonToolbar>
    </IonFooter>
  </IonModal>
</template>

<style>
.workspace-stage-modal { --height: min(43rem, calc(100dvh - 2rem)); }
.stage-unit-note {
  align-self: end;
  margin: 0;
  padding: 0.85rem;
  color: var(--nf-color-text-muted);
  font-size: 0.82rem;
}
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
</style>
